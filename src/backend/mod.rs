//! Bounded, bidirectional JSON-lines transport to a Codex-owned child process.

mod executable;
mod transport;

pub use executable::BackendExecutable;

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

/// Process and resource limits. Extra arguments precede the selected transport flags.
#[derive(Clone, Debug)]
pub struct BackendOptions {
    pub executable: BackendExecutable,
    pub args: Vec<OsString>,
    pub request_timeout: Duration,
    pub max_frame_bytes: usize,
    pub event_capacity: usize,
    pub outbound_capacity: usize,
    pub max_in_flight: usize,
    /// Additional Codex initialization capabilities; experimental API is always enabled.
    pub capabilities: Value,
}

impl Default for BackendOptions {
    fn default() -> Self {
        Self {
            executable: BackendExecutable::Bundled,
            args: Vec::new(),
            request_timeout: Duration::from_secs(120),
            max_frame_bytes: 8 * 1024 * 1024,
            event_capacity: 128,
            outbound_capacity: 32,
            max_in_flight: 256,
            capabilities: json!({}),
        }
    }
}

/// A downstream JSON-RPC error, preserved without flattening its structured data.
#[derive(Clone, Debug, Deserialize, Serialize, thiserror::Error)]
#[error("Codex RPC error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Codex transport I/O: {0}")]
    Io(String),
    #[error("invalid Codex protocol frame: {0}")]
    Protocol(String),
    #[error("Codex disconnected: {0}")]
    Disconnected(String),
    #[error("Codex request timed out")]
    Timeout,
    #[error("Codex resource limit: {0}")]
    Limit(String),
    #[error("invalid backend configuration: {0}")]
    Configuration(String),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// Ordered backend activity; a reserved channel slot guarantees a terminal event.
#[derive(Debug)]
pub enum BackendEvent {
    Notification {
        /// Monotonic backend frame order, shared with authoritative RPC snapshots.
        sequence: u64,
        method: String,
        params: Value,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
    Disconnected {
        message: String,
    },
}

pub(crate) struct Snapshot {
    pub value: Value,
    pub sequence: u64,
}

type Reply = oneshot::Sender<Result<Snapshot, BackendError>>;

#[derive(Default)]
struct State {
    pending: HashMap<u64, Reply>,
    disconnected: Option<String>,
    sequence: u64,
}

struct Inner {
    outgoing: mpsc::Sender<Vec<u8>>,
    state: Arc<Mutex<State>>,
    next_id: AtomicU64,
    shutdown: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
    request_timeout: Duration,
    max_frame_bytes: usize,
    max_in_flight: usize,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Cloneable request endpoint. Drop the last clone or call shutdown to reap the child.
#[derive(Clone)]
pub struct Backend(Arc<Inner>);

impl Backend {
    /// Spawn Codex and complete its separate initialize/initialized handshake.
    pub async fn spawn(
        options: BackendOptions,
    ) -> Result<(Self, mpsc::Receiver<BackendEvent>), BackendError> {
        transport::spawn(options).await
    }

    /// Send a correlated RPC. Dropping this future removes its pending reply slot.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, BackendError> {
        Ok(self.request_snapshot(method, params).await?.value)
    }

    /// Return the reader's exact response position for reconciling earlier events.
    pub(crate) async fn request_snapshot(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Snapshot, BackendError> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, receive) = oneshot::channel();
        {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(message) = &state.disconnected {
                return Err(BackendError::Disconnected(message.clone()));
            }
            if state.pending.len() >= self.0.max_in_flight {
                return Err(BackendError::Limit("too many in-flight requests".into()));
            }
            state.pending.insert(id, reply);
        }
        let _pending = PendingGuard {
            id,
            state: Arc::clone(&self.0.state),
        };
        tokio::time::timeout(self.0.request_timeout, async {
            self.send(json!({"id": id, "method": method, "params": params}))
                .await?;
            receive
                .await
                .map_err(|_| BackendError::Disconnected("reply channel closed".into()))?
        })
        .await
        .map_err(|_| BackendError::Timeout)?
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), BackendError> {
        self.send(json!({"method": method, "params": params})).await
    }

    /// Respond to an app-server initiated request, preserving string or numeric IDs.
    pub async fn respond(
        &self,
        id: Value,
        result: Result<Value, RpcError>,
    ) -> Result<(), BackendError> {
        if !valid_id(&id) {
            return Err(BackendError::Protocol(
                "request IDs must be strings or integers".into(),
            ));
        }
        let frame = match result {
            Ok(result) => json!({"id": id, "result": result}),
            Err(error) => json!({"id": id, "error": error}),
        };
        self.send(frame).await
    }

    async fn send(&self, frame: Value) -> Result<(), BackendError> {
        {
            let state = self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(message) = &state.disconnected {
                return Err(BackendError::Disconnected(message.clone()));
            }
        }
        let mut bytes = serde_json::to_vec(&frame)
            .map_err(|error| BackendError::Protocol(error.to_string()))?;
        if bytes.len() > self.0.max_frame_bytes {
            return Err(BackendError::Limit(
                "outbound frame exceeds max_frame_bytes".into(),
            ));
        }
        bytes.push(b'\n');
        tokio::time::timeout(self.0.request_timeout, self.0.outgoing.send(bytes))
            .await
            .map_err(|_| BackendError::Timeout)?
            .map_err(|_| BackendError::Disconnected("writer closed".into()))
    }

    /// Close stdin, allow bounded graceful exit, then kill and reap a stubborn child.
    pub async fn shutdown(&self) -> Result<(), BackendError> {
        let _ = self.0.shutdown.send(true);
        let mut finished = self.0.finished.clone();
        tokio::time::timeout(Duration::from_secs(6), async {
            while !*finished.borrow_and_update() {
                finished
                    .changed()
                    .await
                    .map_err(|_| BackendError::Disconnected("supervisor stopped".into()))?;
            }
            Ok(())
        })
        .await
        .map_err(|_| BackendError::Timeout)?
    }
}

struct PendingGuard {
    id: u64,
    state: Arc<Mutex<State>>,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&self.id);
    }
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_i64() || id.is_u64()
}
