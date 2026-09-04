use std::sync::Arc;

use agent_client_protocol::{Client, V2ConnectionTo, schema::v2};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{Server, Session};
use crate::{
    config::{self, Configuration},
    input,
};

impl Server {
    pub(super) async fn new_session(
        &self,
        request: v2::NewSessionRequest,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<v2::NewSessionResponse> {
        let _creation = self.creation_gate.lock().await;
        let state = self.state.lock().await;
        ensure!(
            state.sessions.len() < self.options.max_sessions,
            "open session limit reached"
        );
        let metadata = config::metadata(request.meta.as_ref(), "thread", state.extensions)?;
        let models = state.models.clone();
        drop(state);
        let prepared = self.mcp.prepare(&request.mcp_servers, connection).await?;
        let params = config::thread_parameters(
            config::ThreadOperation::Start,
            &request.cwd,
            &request.additional_directories,
            &prepared.servers,
            metadata,
        )?;
        let response = self.setup_request("thread/start", params).await?;
        let id = response["thread"]["id"]
            .as_str()
            .context("thread/start response has no thread id")?
            .to_owned();
        let configuration = Configuration::from_response(&response, models);
        let session = self.register(id.clone(), configuration).await?;
        session.data.lock().await.mcp_leases = prepared.leases;
        let options = session.data.lock().await.configuration.options();
        if session.data.lock().await.active_turn.is_none() {
            self.idle(connection, &id, false).await?;
        }
        Ok(v2::NewSessionResponse::new(id).config_options(options))
    }

    pub(super) async fn list_sessions(
        &self,
        request: v2::ListSessionsRequest,
    ) -> Result<v2::ListSessionsResponse> {
        let response = self.backend.request("thread/list", json!({"cursor":request.cursor,"cwd":request.cwd,"archived":false,"limit":100,"sortKey":"updated_at"})).await?;
        let mut sessions = Vec::new();
        for thread in response["data"]
            .as_array()
            .context("thread/list response missing data")?
        {
            let id = thread["id"].as_str().context("thread missing id")?;
            let cwd: v2::AbsolutePath = serde_json::from_value(thread["cwd"].clone())
                .context("thread missing absolute cwd")?;
            let mut info = v2::SessionInfo::new(id, cwd);
            if let Some(title) = thread["name"]
                .as_str()
                .or_else(|| thread["preview"].as_str())
            {
                info = info.title(title);
            }
            if let Some(updated) = thread["updatedAt"].as_i64() {
                let date = time::OffsetDateTime::from_unix_timestamp(updated)?
                    .format(&time::format_description::well_known::Rfc3339)?;
                info = info.updated_at(date);
            }
            sessions.push(info);
        }
        Ok(v2::ListSessionsResponse::new(sessions).next_cursor(
            response["nextCursor"]
                .as_str()
                .map(v2::SessionListCursor::new),
        ))
    }

    pub(super) async fn resume_session(
        &self,
        request: v2::ResumeSessionRequest,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<v2::ResumeSessionResponse> {
        let _creation = self.creation_gate.lock().await;
        if request
            .replay_from
            .as_ref()
            .is_some_and(|cursor| !matches!(cursor, v2::ReplayFrom::Start(_)))
        {
            bail!("only replayFrom start is supported");
        }
        let id = request.session_id.to_string();
        let prepared = self.mcp.prepare(&request.mcp_servers, connection).await?;
        let read = self
            .backend
            .request("thread/read", json!({"threadId":id,"includeTurns":false}))
            .await?;
        ensure!(
            read["thread"]["cwd"] == serde_json::to_value(&request.cwd)?,
            "resume cwd must match the stored session cwd"
        );
        ensure!(
            read["thread"]
                .pointer("/status/type")
                .and_then(Value::as_str)
                != Some("active"),
            "cannot resume a session while it has active foreground work"
        );
        let mut state = self.state.lock().await;
        let metadata = config::metadata(request.meta.as_ref(), "thread", state.extensions)?;
        let models = state.models.clone();
        let mut params = config::thread_parameters(
            config::ThreadOperation::Resume,
            &request.cwd,
            &request.additional_directories,
            &prepared.servers,
            metadata,
        )?;
        params["threadId"] = json!(id);
        params["excludeTurns"] = json!(true);
        for field in [
            "ephemeral",
            "historyMode",
            "environments",
            "dynamicTools",
            "selectedCapabilityRoots",
            "experimentalRawEvents",
            "allowProviderModelFallback",
        ] {
            ensure!(
                params.get(field).is_none(),
                "{field} is only valid when creating a session"
            );
        }
        ensure!(
            state.sessions.contains_key(&id) || state.sessions.len() < self.options.max_sessions,
            "open session limit reached"
        );
        let was_registered = state.sessions.contains_key(&id);
        let session = state
            .sessions
            .entry(id.clone())
            .or_insert_with(|| Arc::new(Session::new(id.clone(), Configuration::default())))
            .clone();
        drop(state);
        let _gate = session.gate.lock().await;
        self.synchronize_session(&session).await?;
        let _delivery = session.delivery.lock().await;
        ensure!(
            session.data.lock().await.active_turn.is_none(),
            "session still has foreground work"
        );
        if was_registered {
            // Codex deliberately ignores overrides when rejoining a subscribed thread.
            // Detaching our sole connection lets its idle cache reload MCP/root settings.
            self.close_streams(&id).await?;
            self.backend
                .request("thread/unsubscribe", json!({"threadId":id}))
                .await?;
            session.data.lock().await.open = false;
        }
        let snapshot = match self.backend.request_snapshot("thread/resume", params).await {
            Ok(response) => response,
            Err(error) => {
                if !was_registered {
                    self.state.lock().await.sessions.remove(&id);
                } else {
                    let mut leases = std::mem::take(&mut session.data.lock().await.mcp_leases);
                    if let Err(cleanup) = leases.close().await {
                        self.notification_failure(connection, &id, "MCP resource cleanup", cleanup)
                            .await?;
                    }
                }
                return Err(error.into());
            }
        };
        let response = snapshot.value;
        let mut previous_leases = {
            let mut data = session.data.lock().await;
            data.open = true;
            data.closing = false;
            let mut configuration = Configuration::from_response(&response, models);
            data.settings_overlay = configuration.settings.clone();
            let mut observed = std::mem::take(&mut data.configuration.settings);
            observed.extend(configuration.settings);
            configuration.settings = observed;
            data.configuration = configuration;
            data.settings_cutoff = snapshot.sequence;
            data.pending_settings = None;
            if request.replay_from.is_some() {
                data.snapshot_cutoffs.clear();
            }
            std::mem::replace(&mut data.mcp_leases, prepared.leases)
        };
        drop(_delivery);
        // Apply full snapshots already emitted around resume. The response is
        // only a partial settings snapshot, so omitted fields remain meaningful.
        self.synchronize_session(&session).await?;
        let _delivery = session.delivery.lock().await;
        if let Err(error) = previous_leases.close().await {
            self.notification_failure(connection, &id, "MCP resource cleanup", error)
                .await?;
        }
        if request.replay_from.is_some()
            && let Err(error) = self.replay(&id, &session, connection).await
        {
            if !was_registered {
                self.state.lock().await.sessions.remove(&id);
                self.backend
                    .request("thread/unsubscribe", json!({"threadId":id}))
                    .await?;
            }
            return Err(error);
        }
        let options = session.data.lock().await.configuration.options();
        self.idle(connection, &id, false).await?;
        Ok(v2::ResumeSessionResponse::new().config_options(options))
    }

    pub(super) async fn fork_session(
        &self,
        request: v2::ForkSessionRequest,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<v2::ForkSessionResponse> {
        let _creation = self.creation_gate.lock().await;
        let source = self.session(&request.session_id.to_string()).await?;
        let _gate = source.gate.lock().await;
        let source_data = source.data.lock().await;
        ensure!(
            source_data.open && !source_data.closing,
            "session is closed; resume it first"
        );
        ensure!(
            source_data.active_turn.is_none(),
            "cannot fork while foreground work is active"
        );
        drop(source_data);
        let state = self.state.lock().await;
        ensure!(
            state.sessions.len() < self.options.max_sessions,
            "open session limit reached"
        );
        let metadata = config::metadata(request.meta.as_ref(), "thread", state.extensions)?;
        let models = state.models.clone();
        drop(state);
        let prepared = self.mcp.prepare(&request.mcp_servers, connection).await?;
        let mut params = config::thread_parameters(
            config::ThreadOperation::Fork,
            &request.cwd,
            &request.additional_directories,
            &prepared.servers,
            metadata,
        )?;
        params["threadId"] = serde_json::to_value(request.session_id)?;
        for field in [
            "historyMode",
            "environments",
            "dynamicTools",
            "selectedCapabilityRoots",
            "experimentalRawEvents",
            "allowProviderModelFallback",
        ] {
            ensure!(
                params.get(field).is_none(),
                "{field} is not supported when forking"
            );
        }
        let response = self.setup_request("thread/fork", params).await?;
        let id = response["thread"]["id"]
            .as_str()
            .context("fork response missing thread id")?
            .to_owned();
        let configuration = Configuration::from_response(&response, models);
        let session = self.register(id.clone(), configuration).await?;
        session.data.lock().await.mcp_leases = prepared.leases;
        let options = session.data.lock().await.configuration.options();
        if session.data.lock().await.active_turn.is_none() {
            self.idle(connection, &id, false).await?;
        }
        Ok(v2::ForkSessionResponse::new(id).config_options(options))
    }

    pub(super) async fn prompt(&self, request: v2::PromptRequest) -> Result<v2::PromptResponse> {
        let id = request.session_id.to_string();
        let session = self.session(&id).await?;
        let _gate = session.gate.lock().await;
        self.reconcile_settings(&session).await?;
        let _admission = session.admission.lock().await;
        let input = input::prompt_to_codex(&request)?;
        ensure!(!input.is_empty(), "prompt must contain content");
        let negotiated = self.state.lock().await.extensions;
        let extras = config::turn_parameters(request.meta.as_ref(), negotiated)?;
        let data = session.data.lock().await;
        ensure!(
            data.open && !data.closing,
            "session is closed or closing; resume it first"
        );
        let active = data.active_turn.clone();
        drop(data);
        if let Some(turn) = active {
            ensure!(
                extras.is_empty(),
                "per-turn overrides cannot be applied to steering input; use a live-turn settings extension"
            );
            self.backend
                .request(
                    "turn/steer",
                    json!({"threadId":id,"expectedTurnId":turn,"input":input}),
                )
                .await?;
        } else {
            let mut params = Value::Object(extras);
            params["threadId"] = json!(id);
            params["input"] = json!(input);
            let response = self.backend.request("turn/start", params).await?;
            let turn = response["turn"]["id"]
                .as_str()
                .context("turn/start response missing turn id")?;
            // Completion may race the accepted response; its status is authoritative.
            let mut data = session.data.lock().await;
            if response["turn"]["status"] == "inProgress" && data.active_turn.is_none() {
                // The event pump records completed IDs to avoid resurrecting a completed turn.
                if data.last_completed_turn.as_deref() != Some(turn) {
                    data.active_turn = Some(turn.to_owned());
                }
            }
        }
        Ok(v2::PromptResponse::new())
    }

    pub(super) async fn close(
        &self,
        id: &str,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<v2::CloseSessionResponse> {
        let session = self.session(id).await?;
        let _gate = session.gate.lock().await;
        session.data.lock().await.closing = true;
        self.cancel_locked(id, &session, connection).await?;
        self.close_streams(id).await?;
        self.close_descendants(id).await?;
        self.backend
            .request("thread/backgroundTerminals/clean", json!({"threadId":id}))
            .await?;
        self.backend
            .request("thread/unsubscribe", json!({"threadId":id}))
            .await?;
        let mut leases = std::mem::take(&mut session.data.lock().await.mcp_leases);
        let resource_result = leases.close().await;
        session.data.lock().await.open = false;
        self.state.lock().await.sessions.remove(id);
        resource_result?;
        Ok(v2::CloseSessionResponse::new())
    }

    pub(super) async fn delete(
        &self,
        id: &str,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<v2::DeleteSessionResponse> {
        // ACP permits soft deletion; archive avoids Codex hard deletion of descendants.
        if self.state.lock().await.sessions.contains_key(id) {
            self.close(id, connection).await?;
        } else {
            // The local client may delete a persisted item returned by session/list.
            let thread = self
                .backend
                .request("thread/read", json!({"threadId":id,"includeTurns":false}))
                .await?;
            ensure!(
                thread["thread"]
                    .pointer("/status/type")
                    .and_then(Value::as_str)
                    != Some("active"),
                "cannot delete an active session; resume and close it first"
            );
        }
        self.backend
            .request("thread/archive", json!({"threadId":id}))
            .await?;
        Ok(v2::DeleteSessionResponse::new())
    }
}
