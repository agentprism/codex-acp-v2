//! Negotiated backend operations share standard session ownership and state.

use agent_client_protocol::{Client, V2ConnectionTo, schema::v2};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::Server;
use crate::extensions::{RequestEnvelope, RequestScope};

impl Server {
    pub(super) async fn extension(
        &self,
        request: v2::ExtRequest,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<Value> {
        ensure!(
            request.method.as_ref() == "_codex/request",
            "unsupported ACP extension method"
        );
        ensure!(
            self.state.lock().await.extensions,
            "Codex extensions must be negotiated during initialize"
        );
        let envelope: RequestEnvelope = serde_json::from_str(request.params.get())?;
        if envelope.method == "account/login/start"
            && envelope.params["type"] == "chatgptAuthTokens"
        {
            ensure!(
                self.state
                    .lock()
                    .await
                    .negotiation
                    .as_ref()
                    .is_some_and(|negotiation| negotiation.server_requests),
                "external token authentication requires serverRequests for token refresh"
            );
        }
        let detached_review =
            envelope.method == "review/start" && envelope.params["delivery"] == "detached";
        let _creation = if detached_review {
            Some(self.creation_gate.lock().await)
        } else {
            None
        };
        if detached_review {
            ensure!(
                self.state.lock().await.sessions.len() < self.options.max_sessions,
                "open session limit reached"
            );
        }
        let requested_thread = envelope.params.get("threadId").and_then(Value::as_str);
        let scope = if envelope.method == "mcpServer/event/stream/stop" {
            let state = self.state.lock().await;
            let id = envelope.params["subscriptionId"]
                .as_str()
                .context("stream requires subscriptionId")?;
            let owner = state
                .mcp_subscriptions
                .get(id)
                .context("subscription is not owned by this connection")?;
            ensure!(
                envelope.session_id.as_deref() == Some(owner),
                "subscription belongs to a different session"
            );
            let mut params = envelope.params.clone();
            params["threadId"] = json!(owner);
            let authorized = RequestEnvelope {
                version: envelope.version,
                session_id: envelope.session_id.clone(),
                method: envelope.method.clone(),
                params,
            };
            drop(state);
            self.policy
                .authorize(&authorized, &self.owned_threads().await)?
        } else if let (Some(root), Some(child)) = (envelope.session_id.as_deref(), requested_thread)
            && root != child
            && matches!(
                envelope.method.as_str(),
                "thread/read" | "thread/turns/list" | "thread/items/list"
            )
        {
            ensure!(
                self.owner_for_thread(child).await?.as_deref() == Some(root),
                "requested thread is not a descendant of the owned session"
            );
            // Verify policy against the root session only after backend-verified ancestry.
            // The original child id remains intact in the forwarded read-only request.
            let mut params = envelope.params.clone();
            params["threadId"] = json!(root);
            let authorized = RequestEnvelope {
                version: envelope.version,
                session_id: envelope.session_id.clone(),
                method: envelope.method.clone(),
                params,
            };
            self.policy
                .authorize(&authorized, &self.owned_threads().await)?
        } else {
            self.policy
                .authorize(&envelope, &self.owned_threads().await)?
        };
        let session = match &scope {
            RequestScope::Thread(id) => Some(self.session(id).await?),
            _ => None,
        };
        let _gate = if let Some(session) = &session {
            Some(session.gate.lock().await)
        } else {
            None
        };
        if let Some(session) = &session {
            let data = session.data.lock().await;
            ensure!(data.open && !data.closing, "session is closed or closing");
            if envelope.method == "turn/start" {
                ensure!(
                    data.active_turn.is_none(),
                    "foreground work already active; steer it explicitly"
                );
            }
        }
        if envelope.method == "turn/settings/update" {
            let session = session
                .as_ref()
                .context("live turn settings require a session")?;
            let data = session.data.lock().await;
            ensure!(
                envelope.params["turnId"].as_str().is_some()
                    && envelope.params["turnId"].as_str() == data.active_turn.as_deref(),
                "live settings must target the active turn id"
            );
        }
        if envelope.method == "turn/start" {
            let session = session.as_ref().context("turn start requires a session")?;
            self.reconcile_settings(session).await?;
            self.record_settings_change(session, &envelope.params)
                .await?;
        }
        let starts_foreground = matches!(
            envelope.method.as_str(),
            "turn/start" | "thread/queue/start"
        ) || (envelope.method == "review/start" && !detached_review);
        let admission = if starts_foreground {
            Some(
                session
                    .as_ref()
                    .context("foreground work requires a session")?
                    .admission
                    .lock()
                    .await,
            )
        } else {
            None
        };
        let history_mutation = matches!(
            envelope.method.as_str(),
            "thread/rollback" | "thread/revert"
        );
        let _history_delivery = if history_mutation {
            ensure!(
                self.state
                    .lock()
                    .await
                    .negotiation
                    .as_ref()
                    .is_some_and(|negotiation| negotiation.session_reset),
                "history mutation requires codex sessionReset negotiation"
            );
            let session = session
                .as_ref()
                .context("history mutation requires a session")?;
            self.synchronize_session(session).await?;
            self.reconcile_settings(session).await?;
            ensure!(
                session.data.lock().await.active_turn.is_none(),
                "cannot replace history while foreground work is active"
            );
            Some(session.delivery.lock().await)
        } else {
            None
        };
        if matches!(envelope.method.as_str(), "thread/archive" | "thread/delete") {
            let id = envelope
                .session_id
                .as_deref()
                .context("thread teardown requires a session")?;
            let session = session
                .as_ref()
                .context("thread teardown requires a session")?;
            session.data.lock().await.closing = true;
            self.cancel_locked(id, session, connection).await?;
            self.close_streams(id).await?;
            self.close_descendants(id).await?;
            self.backend
                .request("thread/backgroundTerminals/clean", json!({"threadId":id}))
                .await?;
        }
        let mut response_sequence = 0;
        let response = if envelope.method == "mcpServer/event/stream/start" {
            self.start_stream(&envelope).await?
        } else if envelope.method == "mcpServer/event/stream/stop" {
            self.stop_stream(&envelope).await?
        } else if envelope.method == "thread/settings/update" {
            self.apply_thread_settings(
                session
                    .as_ref()
                    .context("thread settings require a session")?,
                envelope.params,
            )
            .await?
        } else if detached_review {
            self.setup_request(&envelope.method, envelope.params)
                .await?
        } else {
            match self
                .backend
                .request_snapshot(&envelope.method, envelope.params)
                .await
            {
                Ok(snapshot) => {
                    response_sequence = snapshot.sequence;
                    snapshot.value
                }
                Err(error) => {
                    if envelope.method == "turn/start"
                        && matches!(error, crate::backend::BackendError::Rpc(_))
                        && let Some(session) = &session
                    {
                        session.data.lock().await.pending_settings = None;
                    }
                    return Err(error.into());
                }
            }
        };
        if starts_foreground && let Some(session) = &session {
            let mut data = session.data.lock().await;
            let turn_id = response
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .context("turn/start response missing turn id")?;
            if response["turn"]["status"] == "inProgress"
                && data.last_completed_turn.as_deref() != Some(turn_id)
            {
                data.active_turn = Some(turn_id.to_owned());
            }
        }
        drop(admission);
        if envelope.method == "turn/start"
            && let Some(session) = &session
        {
            self.reconcile_settings(session).await?;
        }
        if envelope.method == "review/start"
            && let Some(review_id) = response.get("reviewThreadId").and_then(Value::as_str)
            && envelope.session_id.as_deref() != Some(review_id)
        {
            let source = session
                .as_ref()
                .context("detached review requires a source session")?;
            let configuration = source.data.lock().await.configuration.clone();
            self.register(review_id.to_owned(), configuration).await?;
        }
        if history_mutation {
            let id = envelope
                .session_id
                .as_deref()
                .context("history mutation requires a session")?;
            let session = session
                .as_ref()
                .context("history mutation requires a session")?;
            if envelope.method == "thread/revert" {
                session.data.lock().await.reconciled_revert_notification = true;
            }
            self.reset_history(id, session, &envelope.method, response_sequence, connection)
                .await?;
        }
        drop(_history_delivery);
        if history_mutation && let Some(session) = &session {
            // Revert may restore thread settings as well as transcript items.
            // Observe its effective notification before a later native request
            // can decide that a cached setting is already the desired value.
            self.synchronize_session(session).await?;
        }
        if matches!(envelope.method.as_str(), "thread/archive" | "thread/delete")
            && let Some(id) = envelope.session_id
        {
            self.cancel_interactions(&id).await;
            if let Some(session) = &session {
                let mut data = session.data.lock().await;
                data.open = false;
                let mut leases = std::mem::take(&mut data.mcp_leases);
                drop(data);
                if let Err(error) = leases.close().await {
                    self.notification_failure(connection, &id, "MCP resource cleanup", error)
                        .await?;
                }
            }
            self.state.lock().await.sessions.remove(&id);
            self.idle(connection, &id, true).await?;
        }
        Ok(response)
    }
}
