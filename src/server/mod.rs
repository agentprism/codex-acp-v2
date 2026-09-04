//! ACP v2 frontend and the independently running Codex event pump.

mod controls;
mod dispatch;
mod events;
mod extension_requests;
mod handlers;
mod history;
mod lifecycle;
mod ownership;
mod resources;
mod teardown;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use agent_client_protocol::{Client, V2ConnectionTo, schema::v2};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc, oneshot};

use crate::{
    backend::{Backend, BackendEvent, BackendOptions},
    config::Configuration,
    extensions::{ExtensionPolicy, Negotiation},
    projection::Projector,
};

/// Resource and authorization limits for one ACP connection and its child server.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub backend: BackendOptions,
    pub allow_host_methods: bool,
    pub max_sessions: usize,
    pub max_replay_items: usize,
    pub interaction_timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            backend: BackendOptions::default(),
            allow_host_methods: false,
            max_sessions: 64,
            max_replay_items: 100_000,
            interaction_timeout: Duration::from_secs(600),
        }
    }
}

#[derive(Clone)]
struct Server {
    backend: Backend,
    mcp: crate::mcp::McpManager,
    state: Arc<Mutex<State>>,
    options: Arc<ServerOptions>,
    policy: Arc<ExtensionPolicy>,
    creation_gate: Arc<Mutex<()>>,
    registrations: mpsc::Sender<Registration>,
    request_slots: Arc<Semaphore>,
}

#[derive(Default)]
struct State {
    initialized: bool,
    capabilities: v2::ClientCapabilities,
    extensions: bool,
    negotiation: Option<Negotiation>,
    sessions: HashMap<String, Arc<Session>>,
    descendant_roots: HashMap<String, String>,
    mcp_subscriptions: HashMap<String, String>,
    models: Vec<Value>,
    pending: HashMap<String, PendingInteraction>,
    disconnected: Option<String>,
    setup_in_progress: bool,
    early_events: VecDeque<BackendEvent>,
    early_bytes: usize,
}

enum Registration {
    New {
        id: String,
        configuration: Configuration,
        reply: oneshot::Sender<Arc<Session>>,
    },
    Fence {
        session: Arc<Session>,
        reply: oneshot::Sender<Arc<Session>>,
    },
}

struct PendingInteraction {
    session_id: Option<String>,
    cancel: oneshot::Sender<InteractionCancellation>,
}

enum InteractionCancellation {
    Cancelled,
    Dismissed,
}

struct Session {
    id: String,
    gate: Mutex<()>,
    admission: Mutex<()>,
    delivery: Mutex<()>,
    data: Mutex<SessionData>,
    changed: Notify,
    events: Mutex<SessionEvents>,
}

#[derive(Default)]
struct SessionEvents {
    pending: VecDeque<(SessionEvent, usize)>,
    bytes: usize,
    running: bool,
}

enum SessionEvent {
    Backend(BackendEvent),
    Registered(oneshot::Sender<Arc<Session>>),
}

struct SessionData {
    open: bool,
    closing: bool,
    active_turn: Option<String>,
    last_completed_turn: Option<String>,
    snapshot_cutoffs: HashMap<String, u64>,
    history_cutoff: u64,
    settings_cutoff: u64,
    settings_generation: u64,
    pending_settings: Option<PendingSettings>,
    history_revision: u64,
    reconciled_revert_notification: bool,
    configuration: Configuration,
    projector: Projector,
    mcp_leases: crate::mcp::McpLeases,
}

#[derive(Clone)]
struct PendingSettings {
    generation: u64,
    patch: serde_json::Map<String, Value>,
}

impl Session {
    fn new(id: String, configuration: Configuration) -> Self {
        Self {
            id,
            gate: Mutex::new(()),
            admission: Mutex::new(()),
            delivery: Mutex::new(()),
            data: Mutex::new(SessionData {
                open: true,
                closing: false,
                active_turn: None,
                last_completed_turn: None,
                snapshot_cutoffs: HashMap::new(),
                history_cutoff: 0,
                settings_cutoff: 0,
                settings_generation: 0,
                pending_settings: None,
                history_revision: 0,
                reconciled_revert_notification: false,
                configuration,
                projector: Projector::default(),
                mcp_leases: crate::mcp::McpLeases::default(),
            }),
            changed: Notify::new(),
            events: Mutex::new(SessionEvents::default()),
        }
    }
}

impl Server {
    async fn setup_request(&self, method: &str, params: Value) -> Result<Value> {
        self.state.lock().await.setup_in_progress = true;
        match self.backend.request(method, params).await {
            Ok(response) => Ok(response),
            Err(error) => {
                let mut state = self.state.lock().await;
                state.setup_in_progress = false;
                let early = std::mem::take(&mut state.early_events);
                state.early_bytes = 0;
                drop(state);
                for event in early {
                    if let BackendEvent::ServerRequest { id, .. } = event {
                        self.backend
                            .respond(
                                id,
                                Err(crate::backend::RpcError {
                                    code: -32000,
                                    message: "session setup failed".into(),
                                    data: None,
                                }),
                            )
                            .await?;
                    }
                }
                Err(error.into())
            }
        }
    }

    async fn register(&self, id: String, configuration: Configuration) -> Result<Arc<Session>> {
        let (reply, result) = oneshot::channel();
        self.registrations
            .send(Registration::New {
                id,
                configuration,
                reply,
            })
            .await
            .context("event pump closed during session setup")?;
        result
            .await
            .context("event pump stopped during session registration")
    }

    /// Observe all backend events already read before replacing session state.
    /// The event pump fences its input and then the session's independent FIFO.
    async fn synchronize_session(&self, session: &Arc<Session>) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.registrations
            .send(Registration::Fence {
                session: session.clone(),
                reply,
            })
            .await
            .context("event pump closed while synchronizing session")?;
        tokio::time::timeout(self.options.backend.request_timeout, result)
            .await
            .context("session event synchronization timed out")?
            .context("event pump closed before session synchronization")?;
        Ok(())
    }

    async fn session(&self, id: &str) -> Result<Arc<Session>> {
        let state = self.state.lock().await;
        if let Some(message) = &state.disconnected {
            anyhow::bail!("Codex disconnected: {message}");
        }
        state
            .sessions
            .get(id)
            .cloned()
            .context("session is not open on this connection; resume it first")
    }

    async fn owned_threads(&self) -> HashSet<String> {
        self.state.lock().await.sessions.keys().cloned().collect()
    }

    async fn send_update(
        &self,
        connection: &V2ConnectionTo<Client>,
        id: &str,
        update: v2::SessionUpdate,
    ) -> Result<()> {
        connection.send_notification(v2::UpdateSessionNotification::new(id, update))?;
        Ok(())
    }

    async fn idle(
        &self,
        connection: &V2ConnectionTo<Client>,
        id: &str,
        cancelled: bool,
    ) -> Result<()> {
        let idle = if cancelled {
            v2::IdleStateUpdate::new().stop_reason(v2::StopReason::Cancelled)
        } else {
            v2::IdleStateUpdate::new()
        };
        self.send_update(
            connection,
            id,
            v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(idle)),
        )
        .await
    }

    async fn cancel_interactions(&self, session_id: &str) {
        let mut state = self.state.lock().await;
        let keys: Vec<_> = state
            .pending
            .iter()
            .filter(|(_, entry)| entry.session_id.as_deref() == Some(session_id))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if let Some(entry) = state.pending.remove(&key) {
                let _ = entry.cancel.send(InteractionCancellation::Cancelled);
            }
        }
    }

    async fn models(&self) -> Result<Vec<Value>> {
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        for _ in 0..100 {
            let page = self
                .backend
                .request("model/list", json!({"cursor":cursor,"limit":100}))
                .await?;
            models.extend(
                page["data"]
                    .as_array()
                    .context("model/list response missing data")?
                    .iter()
                    .cloned(),
            );
            let next = page.get("nextCursor").cloned().unwrap_or(Value::Null);
            if next.is_null() {
                return Ok(models);
            }
            if next == cursor {
                anyhow::bail!("model/list returned a repeating cursor");
            }
            cursor = next;
        }
        anyhow::bail!("model catalog exceeds page limit")
    }
}

fn rpc_error(error: anyhow::Error) -> agent_client_protocol::Error {
    if let Some(error) = error.downcast_ref::<agent_client_protocol::Error>() {
        return error.clone();
    }
    if let Some(crate::backend::BackendError::Rpc(error)) =
        error.downcast_ref::<crate::backend::BackendError>()
    {
        return agent_client_protocol::Error::new(
            i32::try_from(error.code).unwrap_or(-32603),
            error.message.clone(),
        )
        .data(error.data.clone());
    }
    agent_client_protocol::Error::invalid_params().data(error.to_string())
}

/// Serve one ACP v2 connection on stdin/stdout, shutting down its Codex child on exit.
pub async fn run(options: ServerOptions) -> Result<()> {
    let capabilities = options
        .backend
        .capabilities
        .as_object()
        .context("backend capabilities must be a JSON object")?;
    for (name, value) in capabilities {
        match name.as_str() {
            "experimentalApi" | "requestAttestation" | "mcpServerOpenaiFormElicitation" => {
                anyhow::ensure!(value.is_boolean(), "{name} must be a boolean")
            }
            "extensions" => anyhow::ensure!(
                value.is_object() || value.is_null(),
                "extensions must be an object or null"
            ),
            "optOutNotificationMethods" => anyhow::ensure!(
                value.is_null() || value.as_array().is_some_and(Vec::is_empty),
                "notification opt-outs would break the ACP event projection"
            ),
            _ => anyhow::bail!("unknown backend initialization capability {name}"),
        }
    }
    anyhow::ensure!(
        options.backend.capabilities["requestAttestation"] != true || options.allow_host_methods,
        "requestAttestation requires --allow-host-methods"
    );
    let (backend, events) = Backend::spawn(options.backend.clone()).await?;
    let (registrations, registration_receiver) = mpsc::channel(8);
    let server = Server {
        backend: backend.clone(),
        mcp: crate::mcp::McpManager::new(options.interaction_timeout),
        state: Arc::new(Mutex::new(State::default())),
        policy: Arc::new(ExtensionPolicy::new(options.allow_host_methods)),
        creation_gate: Arc::new(Mutex::new(())),
        registrations,
        request_slots: Arc::new(Semaphore::new(128)),
        options: Arc::new(options),
    };
    let result = handlers::serve(
        server,
        Arc::new(Mutex::new(Some((events, registration_receiver)))),
    )
    .await;
    let shutdown = backend.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}

type EventReceiver =
    Arc<Mutex<Option<(mpsc::Receiver<BackendEvent>, mpsc::Receiver<Registration>)>>>;
