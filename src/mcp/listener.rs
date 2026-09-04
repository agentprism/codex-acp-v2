//! One secret URL may be inherited by multiple Codex MCP clients. Its routing
//! table isolates their protocol state and owns all provider-side connections.

use agent_client_protocol::{Client, V2ConnectionTo};
use anyhow::{Context, Result, ensure};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, Semaphore, watch};

use super::{
    Endpoint, McpManager,
    resources::{ProviderLease, first_error},
};

pub(super) struct Listener {
    pub(super) manager: McpManager,
    client: V2ConnectionTo<Client>,
    server_id: String,
    sessions: Mutex<HashMap<String, ProviderLease>>,
    slots: Arc<Semaphore>,
    pub(super) requests: Semaphore,
    pub(super) shutdown: watch::Sender<bool>,
}

impl Listener {
    pub(super) fn new(
        manager: McpManager,
        client: V2ConnectionTo<Client>,
        server_id: &str,
    ) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            manager,
            client,
            server_id: server_id.to_owned(),
            sessions: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(32)),
            requests: Semaphore::new(32),
            shutdown,
        }
    }

    /// A detached bounded connect finishes even if HTTP disconnects while the
    /// provider allocates an ID. An undelivered ProviderLease then cleans up.
    pub(super) async fn connect(self: &Arc<Self>) -> Result<ProviderLease> {
        let permit = {
            // Serialize admission with close's table drain. After close has
            // observed all slots returned, no late admission may allocate one.
            let _sessions = self.sessions.lock().await;
            ensure!(!*self.shutdown.borrow(), "native MCP listener is closing");
            self.slots
                .clone()
                .try_acquire_owned()
                .context("native MCP HTTP session limit reached")?
        };
        let listener = self.clone();
        tokio::spawn(async move {
            let lease = listener
                .manager
                .connect(&listener.server_id, &listener.client, permit)
                .await?;
            ensure!(
                !*listener.shutdown.borrow(),
                "native MCP listener is closing"
            );
            Ok(lease)
        })
        .await
        .context("native MCP connect task failed")?
    }

    pub(super) async fn register(&self, lease: ProviderLease) -> Result<Arc<Endpoint>> {
        let endpoint = lease.endpoint()?;
        let mut sessions = self.sessions.lock().await;
        ensure!(!*self.shutdown.borrow(), "native MCP listener is closing");
        sessions.insert(endpoint.http_session_id.clone(), lease);
        Ok(endpoint)
    }

    pub(super) async fn endpoint(&self, id: &str) -> Option<Arc<Endpoint>> {
        self.sessions
            .lock()
            .await
            .get(id)
            .and_then(|lease| lease.endpoint().ok())
    }

    pub(super) async fn disconnect(&self, id: &str) -> Result<()> {
        let lease = self
            .sessions
            .lock()
            .await
            .remove(id)
            .context("unknown native MCP HTTP session")?;
        lease.close().await
    }

    pub(super) async fn close(&self) -> Result<()> {
        self.shutdown.send_replace(true);
        let sessions = std::mem::take(&mut *self.sessions.lock().await);
        let results =
            futures::future::join_all(sessions.into_values().map(ProviderLease::close)).await;
        let result = first_error(results);
        // A connect or initialize may have started before shutdown. Its lease
        // is cancellation-safe and observes shutdown before registration; wait
        // for its disconnect without making session close unbounded.
        let drained = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.slots.clone().acquire_many_owned(32),
        )
        .await
        .context("native MCP connections are still closing")?;
        let _permits = drained.context("native MCP connection slots closed")?;
        result
    }
}
