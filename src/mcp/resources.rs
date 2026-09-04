use super::{Endpoint, McpManager};
use anyhow::{Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::{sync::OwnedSemaphorePermit, task::JoinHandle};

/// Native endpoints owned by one session. Explicit close observes disconnect
/// replies; dropping setup/session state still guarantees bounded cleanup.
#[derive(Default)]
pub struct McpLeases(pub(super) Vec<Lease>);

pub(super) struct Lease(Option<Cleanup>);
struct Cleanup {
    endpoint: Arc<Endpoint>,
    task: JoinHandle<()>,
    manager: McpManager,
    _permit: OwnedSemaphorePermit,
}

impl McpLeases {
    pub async fn close(&mut self) -> Result<()> {
        let results =
            futures::future::join_all(std::mem::take(&mut self.0).into_iter().map(Lease::close))
                .await;
        let mut first_error = None;
        for result in results {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Lease {
    pub(super) fn new(
        endpoint: Arc<Endpoint>,
        task: JoinHandle<()>,
        manager: McpManager,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self(Some(Cleanup {
            endpoint,
            task,
            manager,
            _permit: permit,
        }))
    }

    async fn close(mut self) -> Result<()> {
        let Some(cleanup) = self.0.take() else {
            return Ok(());
        };
        cleanup.endpoint.shutdown.send_replace(true);
        // Dropping this waiter cannot cancel endpoint cleanup halfway through.
        tokio::spawn(cleanup.close())
            .await
            .context("native MCP cleanup task failed")?
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup.endpoint.shutdown.send_replace(true);
            tokio::spawn(async move {
                if cleanup.close().await.is_err() {
                    tracing::warn!("native MCP cleanup could not confirm provider disconnect");
                }
            });
        }
    }
}

impl Cleanup {
    async fn close(mut self) -> Result<()> {
        self.manager
            .endpoints
            .lock()
            .await
            .remove(&self.endpoint.connection_id.to_string());
        self.endpoint.shutdown.send_replace(true);
        self.endpoint
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
        }
        self.endpoint.disconnect().await
    }
}
