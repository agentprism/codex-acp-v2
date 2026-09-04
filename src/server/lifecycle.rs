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
        let params = config::thread_parameters(
            &request.cwd,
            &request.additional_directories,
            &request.mcp_servers,
            metadata,
        )?;
        let response = self.setup_request("thread/start", params).await?;
        let id = response["thread"]["id"]
            .as_str()
            .context("thread/start response has no thread id")?
            .to_owned();
        let configuration = Configuration::from_response(&response, models);
        let session = self.register(id.clone(), configuration).await?;
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
            &request.cwd,
            &request.additional_directories,
            &request.mcp_servers,
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
            .or_insert_with(|| Arc::new(Session::new(Configuration::default())))
            .clone();
        drop(state);
        let _gate = session.gate.lock().await;
        let _delivery = session.delivery.lock().await;
        ensure!(
            session.data.lock().await.active_turn.is_none(),
            "session still has foreground work"
        );
        let response = match self.backend.request("thread/resume", params).await {
            Ok(response) => response,
            Err(error) => {
                if !was_registered {
                    self.state.lock().await.sessions.remove(&id);
                }
                return Err(error.into());
            }
        };
        {
            let mut data = session.data.lock().await;
            data.open = true;
            data.configuration = Configuration::from_response(&response, models);
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

    async fn replay(
        &self,
        id: &str,
        session: &Session,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<()> {
        // Page items in chronological order, never retaining a second complete transcript.
        // Resume's delivery lock orders these snapshots before queued live notifications.
        let mut cursor = Value::Null;
        let mut count = 0;
        for _ in 0..self.options.max_replay_items.max(1) {
            let page = self
                .backend
                .request(
                    "thread/items/list",
                    json!({"threadId":id,"cursor":cursor,"sortDirection":"asc","limit":100}),
                )
                .await?;
            let entries = page["data"]
                .as_array()
                .context("thread/items/list response missing data")?;
            count += entries.len();
            ensure!(
                count <= self.options.max_replay_items,
                "session replay exceeds configured item limit"
            );
            for entry in entries {
                let mut data = session.data.lock().await;
                if entry["item"]["status"].as_str() != Some("inProgress")
                    && let Some(item_id) = entry["item"]["id"].as_str()
                {
                    data.replayed_finalized.insert(item_id.to_owned());
                }
                let updates = data.projector.replay_item(&entry["item"])?;
                drop(data);
                for update in updates {
                    self.send_update(connection, id, update).await?;
                }
            }
            let next = page.get("nextCursor").cloned().unwrap_or(Value::Null);
            if next.is_null() {
                return Ok(());
            }
            ensure!(
                next != cursor,
                "history pagination returned a repeating cursor"
            );
            cursor = next;
        }
        bail!("session replay exceeds page limit")
    }

    pub(super) async fn fork_session(
        &self,
        request: v2::ForkSessionRequest,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<v2::ForkSessionResponse> {
        let _creation = self.creation_gate.lock().await;
        let source = self.session(&request.session_id.to_string()).await?;
        let _gate = source.gate.lock().await;
        ensure!(
            source.data.lock().await.active_turn.is_none(),
            "cannot fork while foreground work is active"
        );
        let state = self.state.lock().await;
        ensure!(
            state.sessions.len() < self.options.max_sessions,
            "open session limit reached"
        );
        let metadata = config::metadata(request.meta.as_ref(), "thread", state.extensions)?;
        let models = state.models.clone();
        drop(state);
        let mut params = config::thread_parameters(
            &request.cwd,
            &request.additional_directories,
            &request.mcp_servers,
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
        let input = input::prompt_to_codex(&request)?;
        ensure!(!input.is_empty(), "prompt must contain content");
        let negotiated = self.state.lock().await.extensions;
        let extras = config::turn_parameters(request.meta.as_ref(), negotiated)?;
        let data = session.data.lock().await;
        ensure!(data.open, "session is closed; resume it first");
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

    pub(super) async fn cancel(&self, id: &str, connection: &V2ConnectionTo<Client>) -> Result<()> {
        let session = self.session(id).await?;
        let _gate = session.gate.lock().await;
        self.cancel_locked(id, &session, connection).await
    }

    async fn cancel_locked(
        &self,
        id: &str,
        session: &Session,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<()> {
        self.cancel_interactions(id).await;
        let active = session.data.lock().await.active_turn.clone();
        if let Some(turn) = active {
            self.backend
                .request("turn/interrupt", json!({"threadId":id,"turnId":turn}))
                .await?;
            tokio::time::timeout(self.options.backend.request_timeout, async {
                loop {
                    let changed = session.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if session.data.lock().await.active_turn.is_none() {
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

    pub(super) async fn close(
        &self,
        id: &str,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<v2::CloseSessionResponse> {
        let session = self.session(id).await?;
        let _gate = session.gate.lock().await;
        self.cancel_locked(id, &session, connection).await?;
        self.backend
            .request("thread/backgroundTerminals/clean", json!({"threadId":id}))
            .await?;
        self.backend
            .request("thread/unsubscribe", json!({"threadId":id}))
            .await?;
        session.data.lock().await.open = false;
        self.state.lock().await.sessions.remove(id);
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

    pub(super) async fn set_config(
        &self,
        request: v2::SetSessionConfigOptionRequest,
    ) -> Result<v2::SetSessionConfigOptionResponse> {
        let id = request.session_id.to_string();
        let session = self.session(&id).await?;
        let _gate = session.gate.lock().await;
        let data = session.data.lock().await;
        ensure!(data.open, "session is closed; resume it first");
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
        if patch
            .iter()
            .all(|(key, value)| data.configuration.settings.get(key) == Some(value))
        {
            return Ok(v2::SetSessionConfigOptionResponse::new(
                data.configuration.options(),
            ));
        }
        let generation = data.settings_generation;
        drop(data);
        let mut params = Value::Object(patch);
        params["threadId"] = json!(id);
        self.backend
            .request("thread/settings/update", params)
            .await?;
        tokio::time::timeout(self.options.backend.request_timeout, async {
            loop {
                let changed = session.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let data = session.data.lock().await;
                if data.settings_generation > generation {
                    return data.configuration.options();
                }
                drop(data);
                changed.await;
            }
        })
        .await
        .map(v2::SetSessionConfigOptionResponse::new)
        .context("Codex did not report effective thread settings before the timeout")
    }
}
