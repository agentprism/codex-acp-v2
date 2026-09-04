use super::{Endpoint, McpManager, listener::Listener};
use anyhow::{Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::{sync::OwnedSemaphorePermit, task::JoinHandle};

/// Temporary listeners owned by one ACP session. Explicit close observes
/// provider disconnect replies; cancelled setup and dropped sessions clean up.
#[derive(Default)]
pub struct McpLeases(pub(super) Vec<Lease>);

pub(super) struct Lease(Option<ListenerCleanup>);
struct ListenerCleanup {
    listener: Arc<Listener>,
    task: JoinHandle<()>,
}

/// A single independently initialized HTTP/native MCP session. Never shared
/// between parent and child MCP clients that happen to use the same URL.
pub(super) struct ProviderLease(Option<ProviderCleanup>);
struct ProviderCleanup {
    endpoint: Arc<Endpoint>,
    manager: McpManager,
    _global_permit: OwnedSemaphorePermit,
    _listener_permit: OwnedSemaphorePermit,
}

impl McpLeases {
    pub async fn close(&mut self) -> Result<()> {
        let results =
            futures::future::join_all(std::mem::take(&mut self.0).into_iter().map(Lease::close))
                .await;
        first_error(results)
    }
}

impl Lease {
    pub(super) fn new(listener: Arc<Listener>, task: JoinHandle<()>) -> Self {
        Self(Some(ListenerCleanup { listener, task }))
    }

    async fn close(mut self) -> Result<()> {
        let Some(cleanup) = self.0.take() else {
            return Ok(());
        };
        cleanup.listener.shutdown.send_replace(true);
        tokio::spawn(cleanup.close())
            .await
            .context("native MCP listener cleanup task failed")?
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup.listener.shutdown.send_replace(true);
            tokio::spawn(async move {
                if cleanup.close().await.is_err() {
                    tracing::warn!("native MCP listener cleanup could not confirm disconnect");
                }
            });
        }
    }
}

impl ListenerCleanup {
    async fn close(mut self) -> Result<()> {
        let result = self.listener.close().await;
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
        }
        result
    }
}

impl ProviderLease {
    pub(super) fn new(
        endpoint: Arc<Endpoint>,
        manager: McpManager,
        global_permit: OwnedSemaphorePermit,
        listener_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self(Some(ProviderCleanup {
            endpoint,
            manager,
            _global_permit: global_permit,
            _listener_permit: listener_permit,
        }))
    }

    pub(super) fn endpoint(&self) -> Result<Arc<Endpoint>> {
        self.0
            .as_ref()
            .map(|cleanup| cleanup.endpoint.clone())
            .context("native MCP session has closed")
    }

    pub(super) async fn close(mut self) -> Result<()> {
        let Some(cleanup) = self.0.take() else {
            return Ok(());
        };
        cleanup.endpoint.shutdown.send_replace(true);
        tokio::spawn(cleanup.close())
            .await
            .context("native MCP session cleanup task failed")?
    }
}

impl Drop for ProviderLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup.endpoint.shutdown.send_replace(true);
            tokio::spawn(async move {
                if cleanup.close().await.is_err() {
                    tracing::warn!("native MCP session cleanup could not confirm disconnect");
                }
            });
        }
    }
}

impl ProviderCleanup {
    async fn close(self) -> Result<()> {
        self.endpoint.shutdown.send_replace(true);
        self.manager
            .endpoints
            .lock()
            .await
            .remove(&self.endpoint.connection_id.to_string());
        self.endpoint
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.endpoint.disconnect().await
    }
}

pub(super) fn first_error(results: Vec<Result<()>>) -> Result<()> {
    results
        .into_iter()
        .find_map(Result::err)
        .map_or(Ok(()), Err)
}
