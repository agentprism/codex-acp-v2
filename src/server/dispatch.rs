//! Ordered, bounded session delivery independent of the connection-wide reader.

use std::sync::Arc;

use agent_client_protocol::{Client, V2ConnectionTo};
use anyhow::{Result, ensure};
use serde_json::Value;

use super::{Server, Session, SessionEvent};
use crate::backend::BackendEvent;

impl Server {
    /// Partition known-session activity before doing work that may wait for a
    /// history replay's delivery lock. One slow session must not stop the global
    /// event pump from servicing another session's inference or permissions.
    pub(super) async fn route_session_event(
        &self,
        event: BackendEvent,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<Option<BackendEvent>> {
        let owner = match &event {
            BackendEvent::Notification { params, .. }
            | BackendEvent::ServerRequest { params, .. } => {
                if let Some(id) = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .or_else(|| params.pointer("/thread/id").and_then(Value::as_str))
                {
                    self.owner_for_thread(id).await?
                } else {
                    let state = self.state.lock().await;
                    params
                        .get("subscriptionId")
                        .and_then(Value::as_str)
                        .and_then(|id| state.mcp_subscriptions.get(id).cloned())
                }
            }
            BackendEvent::Disconnected { .. } => None,
        };
        let state = self.state.lock().await;
        let session = owner
            .as_ref()
            .and_then(|id| state.sessions.get(id).cloned());
        drop(state);
        if let Some(session) = session {
            self.enqueue_session_event(session, SessionEvent::Backend(event), connection)
                .await?;
            Ok(None)
        } else {
            Ok(Some(event))
        }
    }

    pub(super) async fn enqueue_session_event(
        &self,
        session: Arc<Session>,
        event: SessionEvent,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<()> {
        let bytes = match &event {
            SessionEvent::Backend(
                BackendEvent::Notification { params, .. }
                | BackendEvent::ServerRequest { params, .. },
            ) => serde_json::to_vec(params)?.len(),
            SessionEvent::Backend(BackendEvent::Disconnected { message }) => message.len(),
            SessionEvent::Registered(_) => 0,
        };
        let mut events = session.events.lock().await;
        ensure!(
            events.pending.len() < 256
                && events.bytes.saturating_add(bytes)
                    <= self.options.backend.max_frame_bytes.saturating_mul(4),
            "session delivery buffer exceeded its limit"
        );
        events.bytes += bytes;
        events.pending.push_back((event, bytes));
        if events.running {
            return Ok(());
        }
        events.running = true;
        drop(events);
        let server = self.clone();
        let delivery = connection.clone();
        connection.spawn(async move {
            loop {
                let event = {
                    let mut events = session.events.lock().await;
                    let Some((event, bytes)) = events.pending.pop_front() else {
                        events.running = false;
                        return Ok(());
                    };
                    events.bytes -= bytes;
                    event
                };
                let result = match event {
                    SessionEvent::Registered(reply) => {
                        let _ = reply.send(session.clone());
                        Ok(())
                    }
                    SessionEvent::Backend(BackendEvent::Notification {
                        sequence,
                        method,
                        params,
                    }) => {
                        server
                            .notification(
                                &method,
                                params,
                                &delivery,
                                Some(session.clone()),
                                sequence,
                            )
                            .await
                    }
                    SessionEvent::Backend(BackendEvent::ServerRequest { id, method, params }) => {
                        server
                            .server_request(
                                id,
                                method,
                                params,
                                delivery.clone(),
                                Some(session.clone()),
                            )
                            .await
                    }
                    SessionEvent::Backend(BackendEvent::Disconnected { message }) => {
                        Err(anyhow::anyhow!("Codex disconnected: {message}"))
                    }
                };
                result.map_err(super::rpc_error)?;
            }
        })?;
        Ok(())
    }
}
