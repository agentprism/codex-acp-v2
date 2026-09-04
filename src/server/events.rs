use std::{collections::VecDeque, sync::Arc};

use agent_client_protocol::{Client, V2ConnectionTo, schema::v2};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json, value::to_raw_value};
use tokio::sync::{mpsc, oneshot};

use super::{InteractionCancellation, PendingInteraction, Registration, Server, Session};
use crate::{
    backend::{BackendEvent, RpcError},
    interactions::{self, Interaction},
};

impl Server {
    pub(super) async fn event_pump(
        &self,
        mut events: mpsc::Receiver<BackendEvent>,
        mut registrations: mpsc::Receiver<Registration>,
        connection: V2ConnectionTo<Client>,
    ) -> Result<()> {
        let mut queued = VecDeque::new();
        let mut registered = None;
        loop {
            if queued.is_empty()
                && let Some((reply, session)) = registered.take()
            {
                let reply: oneshot::Sender<Arc<Session>> = reply;
                self.state.lock().await.setup_in_progress = false;
                let _ = reply.send(session);
            }
            let event = if let Some(event) = queued.pop_front() {
                event
            } else {
                tokio::select! {
                    registration = registrations.recv() => {
                        let Some(registration) = registration else { return Ok(()) };
                        let mut state = self.state.lock().await;
                        let session = Arc::new(Session::new(registration.configuration));
                        state.sessions.insert(registration.id.clone(), session.clone());
                        queued = std::mem::take(&mut state.early_events);
                        state.early_bytes = 0;
                        registered = Some((registration.reply, session));
                        continue;
                    }
                    event = events.recv() => match event { Some(event) => event, None => return Ok(()) },
                }
            };
            if let Some(id) = event_thread_id(&event) {
                let mut state = self.state.lock().await;
                if !state.sessions.contains_key(id)
                    && state.setup_in_progress
                    && registered.is_none()
                {
                    let bytes = match &event {
                        BackendEvent::Notification { params, .. }
                        | BackendEvent::ServerRequest { params, .. } => params.to_string().len(),
                        BackendEvent::Disconnected { .. } => 0,
                    };
                    anyhow::ensure!(
                        state.early_events.len() < 256
                            && state.early_bytes + bytes <= self.options.backend.max_frame_bytes,
                        "session setup event buffer exceeded its limit"
                    );
                    state.early_bytes += bytes;
                    state.early_events.push_back(event);
                    continue;
                }
            }
            match event {
                BackendEvent::Notification { method, params } => {
                    self.notification(&method, params, &connection).await?
                }
                BackendEvent::ServerRequest { id, method, params } => {
                    self.server_request(id, method, params, connection.clone())
                        .await?
                }
                BackendEvent::Disconnected { message } => {
                    self.state.lock().await.disconnected = Some(message.clone());
                    let sessions: Vec<_> = self
                        .state
                        .lock()
                        .await
                        .sessions
                        .iter()
                        .map(|(id, session)| (id.clone(), session.clone()))
                        .collect();
                    for (id, session) in sessions {
                        session.data.lock().await.active_turn = None;
                        session.changed.notify_waiters();
                        self.cancel_interactions(&id).await;
                        let idle =
                            v2::IdleStateUpdate::new().meta(serde_json::from_value::<v2::Meta>(
                                json!({"codex":{"error":message}}),
                            )?);
                        self.send_update(
                            &connection,
                            &id,
                            v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(idle)),
                        )
                        .await?;
                    }
                    bail!("Codex app-server disconnected: {message}");
                }
            }
        }
    }

    async fn notification(
        &self,
        method: &str,
        params: Value,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<()> {
        if method == "serverRequest/resolved" {
            let id = params
                .get("requestId")
                .context("resolved server request missing requestId")?;
            if let Some(entry) = self.state.lock().await.pending.remove(&id.to_string()) {
                let _ = entry.cancel.send(InteractionCancellation::Dismissed);
            }
        }
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .or_else(|| params.pointer("/thread/id").and_then(Value::as_str));
        let owner = match thread_id {
            Some(id) => self.owner_for_thread(id).await?,
            None => None,
        };
        let state = self.state.lock().await;
        let session = owner
            .as_ref()
            .and_then(|id| state.sessions.get(id))
            .cloned();
        let raw = state
            .negotiation
            .as_ref()
            .is_some_and(|negotiation| negotiation.wants_event(method))
            && (session.is_some() || (thread_id.is_none() && self.options.allow_host_methods));
        drop(state);
        if let (Some(id), Some(session)) = (owner.as_deref(), session) {
            let _delivery = session.delivery.lock().await;
            let mut data = session.data.lock().await;
            let mut updates = Vec::new();
            let descendant = thread_id.filter(|thread| *thread != id);
            if let Some(child) = descendant {
                updates.extend(child_tool_updates(
                    &mut data.projector,
                    method,
                    &params,
                    child,
                )?);
            } else {
                match method {
                    "turn/started" => {
                        data.active_turn = params
                            .pointer("/turn/id")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                    "turn/completed" => {
                        let completed = params.pointer("/turn/id").and_then(Value::as_str);
                        if data.active_turn.as_deref() == completed {
                            data.active_turn = None;
                        }
                        data.last_completed_turn = completed.map(str::to_owned);
                        session.changed.notify_waiters();
                    }
                    "thread/settings/updated" => {
                        data.configuration.settings = params["threadSettings"]
                            .as_object()
                            .cloned()
                            .context("effective thread settings missing")?;
                        data.settings_generation += 1;
                        updates.push(v2::SessionUpdate::ConfigOptionUpdate(
                            v2::ConfigOptionUpdate::new(data.configuration.options()),
                        ));
                        session.changed.notify_waiters();
                    }
                    _ => {}
                }
                let replayed_delta = (method.ends_with("/delta") || method.ends_with("Delta"))
                    && params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .is_some_and(|id| data.replayed_finalized.contains(id));
                let replayed_start = method == "item/started"
                    && params
                        .pointer("/item/id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| data.replayed_finalized.contains(id));
                if !replayed_delta && !replayed_start {
                    updates.extend(data.projector.project(method, &params)?);
                }
            }
            drop(data);
            for update in updates {
                self.send_update(connection, id, update).await?;
            }
        }
        if raw {
            connection.send_notification(v2::AgentNotification::ExtNotification(Box::new(
                v2::ExtNotification::new(
                    "_codex/event",
                    Arc::from(to_raw_value(
                        &json!({"version":1,"sessionId":owner,"method":method,"params":params}),
                    )?),
                ),
            )))?;
        }
        Ok(())
    }

    async fn server_request(
        &self,
        id: Value,
        method: String,
        params: Value,
        connection: V2ConnectionTo<Client>,
    ) -> Result<()> {
        let source_thread = params["threadId"].as_str();
        let session_id = match source_thread {
            Some(id) => self.owner_for_thread(id).await?,
            None => None,
        };
        let root_session = {
            let state = self.state.lock().await;
            session_id
                .as_ref()
                .and_then(|id| state.sessions.get(id))
                .cloned()
        };
        // Serialize callback insertion with close's `closing` transition. No State lock
        // is held while acquiring session data, so teardown and ancestry reads stay live.
        let root_data = match &root_session {
            Some(session) => Some(session.data.lock().await),
            None => None,
        };
        if root_data
            .as_ref()
            .is_some_and(|data| !data.open || data.closing)
        {
            drop(root_data);
            self.backend
                .respond(
                    id,
                    Err(callback_error(
                        "session is closing; client interaction cancelled",
                    )),
                )
                .await?;
            return Ok(());
        }
        let mut state = self.state.lock().await;
        let owned = session_id
            .as_ref()
            .is_some_and(|id| state.sessions.contains_key(id));
        if !owned && !(source_thread.is_none() && self.options.allow_host_methods) {
            drop(state);
            drop(root_data);
            self.backend
                .respond(
                    id,
                    Err(callback_error(
                        "backend callback targets an unowned session",
                    )),
                )
                .await?;
            return Ok(());
        }
        if state.pending.len() >= 128 {
            drop(state);
            drop(root_data);
            self.backend
                .respond(
                    id,
                    Err(callback_error("too many outstanding client interactions")),
                )
                .await?;
            return Ok(());
        }
        let capabilities = state.capabilities.clone();
        let extension = state
            .negotiation
            .as_ref()
            .is_some_and(|negotiation| negotiation.server_requests);
        let mut interaction = match interactions::translate(
            session_id.as_deref().unwrap_or(""),
            &method,
            &params,
            &capabilities,
        ) {
            Ok(interaction) => interaction,
            Err(error) => {
                drop(state);
                drop(root_data);
                self.backend
                    .respond(id, Err(callback_error(&error.to_string())))
                    .await?;
                return Ok(());
            }
        };
        if let Some(child) = source_thread.filter(|id| Some(*id) != session_id.as_deref())
            && let Some(Interaction::Permission { request, .. }) = &mut interaction
            && let Some(v2::RequestPermissionSubject::ToolCall(subject)) = &mut request.subject
        {
            subject.tool_call.tool_call_id = v2::ToolCallId::new(super::ownership::child_item_id(
                child,
                &subject.tool_call.tool_call_id.to_string(),
            ));
            request.description = Some(format!(
                "Subagent thread: {child}\n{}",
                request.description.as_deref().unwrap_or_default()
            ));
        }
        let (cancel, cancelled) = oneshot::channel();
        state.pending.insert(
            id.to_string(),
            PendingInteraction {
                session_id: session_id.clone(),
                cancel,
            },
        );
        drop(state);
        let blocking = params
            .get("isBlocking")
            .and_then(Value::as_bool)
            .unwrap_or(true)
            && !params.get("turnId").is_some_and(Value::is_null);
        let foreground = root_data
            .as_ref()
            .is_some_and(|data| data.active_turn.is_some());
        if blocking
            && foreground
            && let Some(session_id) = &session_id
        {
            self.send_update(
                &connection,
                session_id,
                v2::SessionUpdate::StateUpdate(v2::StateUpdate::RequiresAction(
                    v2::RequiresActionStateUpdate::new(),
                )),
            )
            .await?;
        }
        drop(root_data);
        let server = self.clone();
        let callback_connection = connection.clone();
        connection.spawn(async move {
            let result = server
                .answer_callback(
                    interaction,
                    extension,
                    (&id, &method, &params, session_id.as_deref()),
                    callback_connection.clone(),
                    cancelled,
                )
                .await;
            server.state.lock().await.pending.remove(&id.to_string());
            match result {
                Ok(Some(result)) => server
                    .backend
                    .respond(id, result)
                    .await
                    .map_err(|error| super::rpc_error(error.into()))?,
                Ok(None) => {}
                Err(error) => server
                    .backend
                    .respond(id, Err(callback_error(&error.to_string())))
                    .await
                    .map_err(|error| super::rpc_error(error.into()))?,
            }
            if let Some(id) = &session_id
                && let Ok(session) = server.session(id).await
            {
                let pending = server
                    .state
                    .lock()
                    .await
                    .pending
                    .values()
                    .any(|pending| pending.session_id.as_ref() == Some(id));
                let data = session.data.lock().await;
                let running = !data.closing && data.active_turn.is_some();
                drop(data);
                if !pending && running {
                    server
                        .send_update(
                            &callback_connection,
                            id,
                            v2::SessionUpdate::StateUpdate(v2::StateUpdate::Running(
                                v2::RunningStateUpdate::new(),
                            )),
                        )
                        .await
                        .map_err(super::rpc_error)?;
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    async fn answer_callback(
        &self,
        interaction: Option<Interaction>,
        extension: bool,
        (id, method, params, session_id): (&Value, &str, &Value, Option<&str>),
        connection: V2ConnectionTo<Client>,
        mut cancelled: oneshot::Receiver<InteractionCancellation>,
    ) -> Result<Option<Result<Value, RpcError>>> {
        match interaction {
            Some(Interaction::Permission { request, resolver }) => {
                tokio::select! {
                    response = tokio::time::timeout(self.options.interaction_timeout, connection.send_request(request).block_task()) => {
                        Ok(Some(Ok(resolver.resolve(response.context("permission request timed out")??)?)))
                    }
                    reason = &mut cancelled => match reason {
                        Ok(InteractionCancellation::Dismissed) => Ok(None),
                        _ => Ok(Some(Ok(resolver.cancelled()))),
                    }
                }
            }
            Some(Interaction::Elicitation { request, resolver }) => {
                tokio::select! {
                    response = tokio::time::timeout(self.options.interaction_timeout, connection.send_request(request).block_task()) => {
                        Ok(Some(Ok(resolver.resolve(response.context("elicitation request timed out")??)?)))
                    }
                    reason = &mut cancelled => match reason {
                        Ok(InteractionCancellation::Dismissed) => Ok(None),
                        _ => Ok(Some(Ok(resolver.cancelled()))),
                    }
                }
            }
            None if extension => {
                let request = v2::ExtRequest::new(
                    "_codex/serverRequest",
                    Arc::from(to_raw_value(
                        &json!({"version":1,"sessionId":session_id,"requestId":id,"method":method,"params":params}),
                    )?),
                );
                tokio::select! {
                    response = tokio::time::timeout(self.options.interaction_timeout, connection.send_request(v2::AgentRequest::ExtMethodRequest(Box::new(request))).block_task()) => {
                        let response = response.context("Codex extension callback timed out")?;
                        Ok(Some(response.map_err(|error| RpcError { code: i64::from(i32::from(error.code)), message: error.message, data: error.data })))
                    }
                    reason = &mut cancelled => match reason {
                        Ok(InteractionCancellation::Dismissed) => Ok(None),
                        _ => Ok(Some(Err(callback_error("client interaction cancelled")))),
                    }
                }
            }
            None => Ok(Some(Err(callback_error(
                "client does not support this backend interaction; negotiate codex serverRequests",
            )))),
        }
    }
}

fn callback_error(message: &str) -> RpcError {
    RpcError {
        code: -32000,
        message: message.to_owned(),
        data: None,
    }
}

fn event_thread_id(event: &BackendEvent) -> Option<&str> {
    match event {
        BackendEvent::Notification { params, .. } | BackendEvent::ServerRequest { params, .. } => {
            params
                .get("threadId")
                .and_then(Value::as_str)
                .or_else(|| params.pointer("/thread/id").and_then(Value::as_str))
        }
        BackendEvent::Disconnected { .. } => None,
    }
}

fn child_tool_updates(
    projector: &mut crate::projection::Projector,
    method: &str,
    params: &Value,
    thread_id: &str,
) -> Result<Vec<v2::SessionUpdate>> {
    let item_event = matches!(method, "item/started" | "item/completed")
        && matches!(
            params["item"]["type"].as_str(),
            Some(
                "commandExecution"
                    | "fileChange"
                    | "mcpToolCall"
                    | "dynamicToolCall"
                    | "webSearch"
                    | "imageView"
                    | "imageGeneration"
                    | "sleep"
                    | "collabAgentToolCall"
                    | "subAgentActivity"
            )
        );
    let tool_event = matches!(
        method,
        "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "item/fileChange/patchUpdated"
            | "item/mcpToolCall/progress"
    );
    if !item_event && !tool_event {
        return Ok(Vec::new());
    }
    let mut projected = params.clone();
    let id = if item_event {
        &mut projected["item"]["id"]
    } else {
        &mut projected["itemId"]
    };
    *id = Value::String(super::ownership::child_item_id(
        thread_id,
        id.as_str().context("child tool event missing item id")?,
    ));
    projector.project(method, &projected)
}
