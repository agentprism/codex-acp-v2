//! Session-owned native MCP connections carried over ACP v2 and exposed to
//! Codex through bounded, token-protected loopback Streamable HTTP endpoints.

mod http;
mod resources;
pub use resources::McpLeases;

use agent_client_protocol::{Client, Error, V2ConnectionTo, schema::v2};
use anyhow::{Context, Result, ensure};
use resources::Lease;
use serde_json::{Value, json, value::to_raw_value};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc, oneshot, watch};

const MAX_FRAME: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 128;
const MAX_SERVERS_PER_SESSION: usize = 16;
type Reply = oneshot::Sender<Result<Value, Error>>;

/// Router shared by setup and native `mcp/message` handlers. Connection slots
/// are held by session leases, including during connect and close operations.
#[derive(Clone)]
pub struct McpManager {
    endpoints: Arc<Mutex<HashMap<String, Arc<Endpoint>>>>,
    changed: Arc<Notify>,
    connecting: Arc<AtomicUsize>,
    slots: Arc<Semaphore>,
    timeout: Duration,
}

/// Declarations for Codex plus ownership of their temporary native endpoints.
/// Commit the leases to the session only after backend setup succeeds.
pub struct PreparedMcp {
    pub servers: Vec<v2::McpServer>,
    pub leases: McpLeases,
}

struct Endpoint {
    connection_id: v2::McpConnectionId,
    http_session_id: String,
    http_initialized: AtomicBool,
    client: V2ConnectionTo<Client>,
    outgoing: mpsc::Sender<Value>,
    incoming: Mutex<mpsc::Receiver<Value>>,
    pending: std::sync::Mutex<HashMap<String, Reply>>,
    next_id: AtomicU64,
    requests: Semaphore,
    streams: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
    disconnected: AtomicBool,
    disconnect_result: watch::Sender<Option<Result<(), String>>>,
    timeout: Duration,
}

impl McpManager {
    pub fn new(timeout: Duration) -> Self {
        Self {
            endpoints: Arc::new(Mutex::new(HashMap::new())),
            changed: Arc::new(Notify::new()),
            connecting: Arc::new(AtomicUsize::new(0)),
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            timeout,
        }
    }

    /// Preserve ordinary transports; replace each native declaration with an
    /// isolated HTTP endpoint and a fresh provider-side MCP connection.
    pub async fn prepare(
        &self,
        servers: &[v2::McpServer],
        client: &V2ConnectionTo<Client>,
    ) -> Result<PreparedMcp> {
        ensure!(
            servers.len() <= MAX_SERVERS_PER_SESSION,
            "at most 16 MCP servers per session are supported"
        );
        let mut prepared = PreparedMcp {
            servers: Vec::with_capacity(servers.len()),
            leases: McpLeases::default(),
        };
        for server in servers {
            let mut declaration = serde_json::to_value(server)?;
            if declaration["type"] != "acp" {
                prepared.servers.push(server.clone());
                continue;
            }
            let (url, lease) = match self.open(&declaration, client).await {
                Ok(value) => value,
                Err(error) => {
                    let _ = prepared.leases.close().await;
                    return Err(error);
                }
            };
            let object = declaration
                .as_object_mut()
                .context("invalid MCP declaration")?;
            object.remove("serverId");
            object.insert("type".into(), json!("http"));
            object.insert("url".into(), json!(url));
            object.insert("headers".into(), json!([]));
            prepared.leases.0.push(lease);
            prepared.servers.push(serde_json::from_value(declaration)?);
        }
        Ok(prepared)
    }

    async fn open(
        &self,
        declaration: &Value,
        client: &V2ConnectionTo<Client>,
    ) -> Result<(String, Lease)> {
        let manager = self.clone();
        let declaration = declaration.clone();
        let client = client.clone();
        // Cancellation after a provider allocates its connection must still run
        // registration to completion: the undelivered result's lease cleans up.
        tokio::spawn(async move { manager.open_inner(&declaration, &client).await })
            .await
            .context("native MCP connection task failed")?
    }

    async fn open_inner(
        &self,
        declaration: &Value,
        client: &V2ConnectionTo<Client>,
    ) -> Result<(String, Lease)> {
        let server_id = declaration["serverId"]
            .as_str()
            .context("native MCP server missing serverId")?;
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .context("native MCP connection limit reached")?;
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.connecting.fetch_add(1, Ordering::SeqCst);
        let _connecting = Connecting(self.clone());
        let connected = tokio::time::timeout(
            self.timeout,
            client
                .send_request(v2::ConnectMcpRequest::new(server_id))
                .block_task(),
        )
        .await
        .context("native MCP connection timed out")??;
        let mut endpoints = self.endpoints.lock().await;
        ensure!(
            !endpoints.contains_key(&connected.connection_id.to_string()),
            "client reused an active native MCP connection id"
        );
        let (outgoing, incoming) = mpsc::channel(32);
        let (shutdown, _) = watch::channel(false);
        let (disconnect_result, _) = watch::channel(None);
        let endpoint = Arc::new(Endpoint {
            connection_id: connected.connection_id,
            http_session_id: uuid::Uuid::new_v4().simple().to_string(),
            http_initialized: AtomicBool::new(false),
            client: client.clone(),
            outgoing,
            incoming: Mutex::new(incoming),
            pending: std::sync::Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            requests: Semaphore::new(32),
            streams: Arc::new(Semaphore::new(1)),
            shutdown,
            disconnected: AtomicBool::new(false),
            disconnect_result,
            timeout: self.timeout,
        });
        endpoints.insert(endpoint.connection_id.to_string(), endpoint.clone());
        drop(endpoints);
        self.changed.notify_waiters();
        let task = tokio::spawn(http::serve(listener, token.clone(), endpoint.clone()));
        Ok((
            format!("http://{address}/{token}"),
            Lease::new(endpoint, task, self.clone(), permit),
        ))
    }

    async fn endpoint(&self, id: &str) -> Result<Arc<Endpoint>, Error> {
        tokio::time::timeout(self.timeout, async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let endpoints = self.endpoints.lock().await;
                if let Some(endpoint) = endpoints.get(id) {
                    return Ok(endpoint.clone());
                }
                if self.connecting.load(Ordering::SeqCst) == 0 {
                    return Err(Error::invalid_params().data("unknown native MCP connection"));
                }
                drop(endpoints);
                changed.await;
            }
        })
        .await
        .map_err(|_| Error::new(-32000, "native MCP connection registration timed out"))?
    }

    /// Send a provider request to Codex via SSE and return its HTTP POSTed
    /// result/error, without requiring foreground inference to be active.
    pub async fn request(
        &self,
        request: v2::MessageMcpRequest,
    ) -> Result<v2::MessageMcpResponse, Error> {
        let endpoint = self.endpoint(&request.connection_id.to_string()).await?;
        let id = format!("acp-{}", endpoint.next_id.fetch_add(1, Ordering::Relaxed));
        let (reply, response) = oneshot::channel();
        {
            let mut pending = endpoint
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.len() >= 32 {
                return Err(Error::new(-32000, "too many native MCP reverse requests"));
            }
            pending.insert(id.clone(), reply);
        }
        let _pending = Pending {
            endpoint: endpoint.clone(),
            id: id.clone(),
        };
        endpoint.publish(
            json!({"jsonrpc":"2.0","id":id,"method":request.method,"params":request.params}),
        )?;
        let mut shutdown = endpoint.shutdown.subscribe();
        let value = tokio::select! {
            value = tokio::time::timeout(endpoint.timeout, response) => value
                .map_err(|_| Error::new(-32000, "native MCP reverse request timed out"))?
                .map_err(|_| Error::new(-32000, "native MCP connection closed"))??,
            _ = shutdown.wait_for(|closed| *closed) => return Err(Error::new(-32800, "native MCP connection closed")),
        };
        Ok(v2::MessageMcpResponse::new(Arc::from(to_raw_value(
            &value,
        )?)))
    }

    /// Unknown-connection notifications are ignored. Live-connection queue
    /// exhaustion closes the endpoint explicitly instead of losing messages.
    pub async fn notify(&self, notification: v2::MessageMcpNotification) -> Result<(), Error> {
        let endpoint = match self.endpoint(&notification.connection_id.to_string()).await {
            Ok(endpoint) => endpoint,
            Err(error) if i32::from(error.code) == -32602 => return Ok(()),
            Err(error) => return Err(error),
        };
        endpoint.publish(
            json!({"jsonrpc":"2.0","method":notification.method,"params":notification.params}),
        )
    }
}

impl Endpoint {
    fn publish(self: &Arc<Self>, message: Value) -> Result<(), Error> {
        if serde_json::to_vec(&message)?.len() > MAX_FRAME {
            return Err(Error::new(-32000, "native MCP frame exceeds 1 MiB"));
        }
        if *self.shutdown.borrow() {
            return Err(Error::new(-32800, "native MCP connection closed"));
        }
        self.outgoing.try_send(message).map_err(|_| {
            self.shutdown.send_replace(true);
            let endpoint = self.clone();
            tokio::spawn(async move {
                let _ = endpoint.disconnect().await;
            });
            Error::new(
                -32000,
                "native MCP event queue exhausted; connection closed",
            )
        })
    }

    async fn disconnect(self: &Arc<Self>) -> Result<()> {
        if !self.disconnected.swap(true, Ordering::SeqCst) {
            let endpoint = self.clone();
            tokio::spawn(async move {
                let result = tokio::time::timeout(
                    endpoint.timeout.min(Duration::from_secs(2)),
                    endpoint
                        .client
                        .send_request(v2::DisconnectMcpRequest::new(
                            endpoint.connection_id.clone(),
                        ))
                        .block_task(),
                )
                .await;
                let result = match result {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_) => Err("native MCP disconnect timed out".into()),
                };
                endpoint.disconnect_result.send_replace(Some(result));
            });
        }
        let mut result = self.disconnect_result.subscribe();
        let finished = result
            .wait_for(Option::is_some)
            .await
            .context("native MCP disconnect result closed")?;
        match finished.as_ref() {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => anyhow::bail!("{error}"),
            None => anyhow::bail!("native MCP disconnect result missing"),
        }
    }
}

struct Connecting(McpManager);
impl Drop for Connecting {
    fn drop(&mut self) {
        self.0.connecting.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}

struct Pending {
    endpoint: Arc<Endpoint>,
    id: String,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.endpoint
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}
