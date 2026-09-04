//! Session controls share serialization and authoritative backend state.

use agent_client_protocol::{Client, V2ConnectionTo, schema::v2};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{PendingSettings, Server, Session};

impl Server {
    pub(super) async fn cancel(&self, id: &str, connection: &V2ConnectionTo<Client>) -> Result<()> {
        let session = self.session(id).await?;
        // Only sequence cancellation with prompt admission, not potentially
        // delayed configuration reconciliation or other session operations.
        let _admission = session.admission.lock().await;
        ensure!(
            session.data.lock().await.open,
            "session is closed; resume it first"
        );
        self.cancel_locked(id, &session, connection).await
    }

    pub(super) async fn cancel_locked(
        &self,
        id: &str,
        session: &Session,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<()> {
        self.cancel_interactions(id).await;
        let active = session.data.lock().await.active_turn.clone();
        if let Some(turn) = active {
            if let Err(error) = self
                .backend
                .request("turn/interrupt", json!({"threadId":id,"turnId":turn}))
                .await
            {
                // Completion and interrupt admission are independent backend operations.
                // Verify a terminal snapshot rather than matching an error's text or
                // treating a normal race as a connection-fatal notification failure.
                let terminal = self
                    .backend
                    .request(
                        "thread/turns/list",
                        json!({
                            "threadId":id,"sortDirection":"desc","limit":100,"itemsView":"summary"
                        }),
                    )
                    .await?;
                let completed = terminal["data"].as_array().and_then(|turns| {
                    turns.iter().find(|candidate| {
                        candidate["id"] == turn
                            && matches!(
                                candidate["status"].as_str(),
                                Some("completed" | "interrupted" | "failed")
                            )
                    })
                });
                if completed.is_none()
                    && session.data.lock().await.last_completed_turn.as_deref()
                        != Some(turn.as_str())
                {
                    return Err(error.into());
                }
            }
            tokio::time::timeout(self.options.backend.request_timeout, async {
                loop {
                    let changed = session.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if session.data.lock().await.active_turn.as_deref() != Some(turn.as_str()) {
                        return;
                    }
                    changed.await;
                }
            })
            .await
            .context("timed out waiting for Codex turn cancellation")?;
        } else {
            self.idle(connection, id, true).await?;
        }
        Ok(())
    }

    /// JSON-RPC notifications have no error response. Surface a session diagnostic
    /// without turning peer mistakes or backend rejections into connection failures.
    pub(super) async fn notification_failure(
        &self,
        connection: &V2ConnectionTo<Client>,
        id: &str,
        operation: &str,
        error: anyhow::Error,
    ) -> Result<()> {
        tracing::warn!(session_id = id, operation, error = %error, "ACP notification rejected");
        if self.state.lock().await.sessions.contains_key(id) {
            self.send_update(
                connection,
                id,
                v2::SessionUpdate::AgentMessage(
                    v2::AgentMessage::new(format!("codex:{operation}:error")).content(vec![
                        v2::ContentBlock::Text(v2::TextContent::new(format!(
                            "{operation} failed: {error}"
                        ))),
                    ]),
                ),
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn set_config(
        &self,
        request: v2::SetSessionConfigOptionRequest,
    ) -> Result<v2::SetSessionConfigOptionResponse> {
        let id = request.session_id.to_string();
        let session = self.session(&id).await?;
        let _gate = session.gate.lock().await;
        self.reconcile_settings(&session).await?;
        let data = session.data.lock().await;
        ensure!(
            data.open && !data.closing,
            "session is closed or closing; resume it first"
        );
        let options = data.configuration.options();
        if let v2::SessionConfigOptionValue::Id { value } = &request.value
            && options.iter().any(|option| option.config_id == request.config_id
                && matches!(&option.kind, v2::SessionConfigKind::Select(select) if select.current_value == *value))
        {
            return Ok(v2::SetSessionConfigOptionResponse::new(options));
        }
        let patch = data
            .configuration
            .patch(&request.config_id.to_string(), &request.value)?;
        drop(data);
        let mut params = Value::Object(patch);
        params["threadId"] = json!(id);
        self.apply_thread_settings(&session, params).await?;
        Ok(v2::SetSessionConfigOptionResponse::new(
            session.data.lock().await.configuration.options(),
        ))
    }

    /// Caller holds the session gate until the queued settings have been observed.
    /// Both extension and standard requests use this barrier before admitting the
    /// next mutation, preventing stale-cache no-op decisions across the two APIs.
    pub(super) async fn apply_thread_settings(
        &self,
        session: &Session,
        params: Value,
    ) -> Result<Value> {
        self.reconcile_settings(session).await?;
        self.record_settings_change(session, &params).await?;
        let response = match self.backend.request("thread/settings/update", params).await {
            Ok(response) => response,
            Err(error) => {
                // Timeouts do not prove a queued mutation failed. Retain the
                // unresolved marker unless the backend explicitly rejected it.
                if matches!(error, crate::backend::BackendError::Rpc(_)) {
                    session.data.lock().await.pending_settings = None;
                }
                return Err(error.into());
            }
        };
        self.reconcile_settings(session).await?;
        Ok(response)
    }

    pub(super) async fn record_settings_change(
        &self,
        session: &Session,
        params: &Value,
    ) -> Result<()> {
        let settings = params
            .as_object()
            .context("settings parameters must be an object")?
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "cwd"
                        | "approvalPolicy"
                        | "approvalsReviewer"
                        | "sandboxPolicy"
                        | "permissions"
                        | "model"
                        | "serviceTier"
                        | "effort"
                        | "summary"
                        | "collaborationMode"
                        | "personality"
                )
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut data = session.data.lock().await;
        if !data.configuration.is_settings_noop(&settings) {
            data.pending_settings = Some(PendingSettings {
                generation: data.settings_generation,
                patch: settings,
            });
        }
        Ok(())
    }

    pub(super) async fn reconcile_settings(&self, session: &Session) -> Result<()> {
        let Some(pending) = session.data.lock().await.pending_settings.clone() else {
            return Ok(());
        };
        tokio::time::timeout(self.options.backend.request_timeout, async {
            loop {
                let changed = session.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let mut data = session.data.lock().await;
                if data.settings_generation > pending.generation
                    && data.configuration.settings_match(&pending.patch)
                {
                    data.pending_settings = None;
                    return Ok::<(), anyhow::Error>(());
                }
                drop(data);
                if let Some(error) = &self.state.lock().await.disconnected {
                    anyhow::bail!("Codex disconnected while updating settings: {error}");
                }
                changed.await;
            }
        })
        .await
        .context("Codex did not report effective thread settings before the timeout")??;
        Ok(())
    }
}
