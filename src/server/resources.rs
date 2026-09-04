//! Connection-owned resources whose downstream events omit a thread identifier.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::Server;
use crate::extensions::RequestEnvelope;

impl Server {
    /// Subscription IDs originate at the client and must be claimed before the
    /// start RPC: the backend is allowed to publish events before replying.
    pub(super) async fn start_stream(&self, request: &RequestEnvelope) -> Result<Value> {
        let root = request
            .session_id
            .as_deref()
            .context("stream requires an owned session")?;
        let id = request.params["subscriptionId"]
            .as_str()
            .context("stream requires subscriptionId")?;
        ensure!(
            !id.is_empty() && id.len() <= 1024,
            "invalid subscription id length"
        );
        {
            let mut state = self.state.lock().await;
            ensure!(
                !state.mcp_subscriptions.contains_key(id),
                "subscription id is already active"
            );
            ensure!(
                state.mcp_subscriptions.len() < 1024,
                "MCP subscription limit reached"
            );
            state
                .mcp_subscriptions
                .insert(id.to_owned(), root.to_owned());
        }
        match self
            .backend
            .request(&request.method, request.params.clone())
            .await
        {
            Ok(result) => Ok(result),
            Err(error) => {
                if matches!(error, crate::backend::BackendError::Rpc(_)) {
                    self.state.lock().await.mcp_subscriptions.remove(id);
                }
                Err(error.into())
            }
        }
    }

    pub(super) async fn stop_stream(&self, request: &RequestEnvelope) -> Result<Value> {
        let id = request.params["subscriptionId"]
            .as_str()
            .context("stream requires subscriptionId")?;
        let result = self
            .backend
            .request(&request.method, request.params.clone())
            .await?;
        self.state.lock().await.mcp_subscriptions.remove(id);
        Ok(result)
    }

    pub(super) async fn close_streams(&self, root: &str) -> Result<()> {
        let ids: Vec<_> = self
            .state
            .lock()
            .await
            .mcp_subscriptions
            .iter()
            .filter(|(_, owner)| owner.as_str() == root)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.backend
                .request("mcpServer/event/stream/stop", json!({"subscriptionId":id}))
                .await?;
            self.state.lock().await.mcp_subscriptions.remove(&id);
        }
        Ok(())
    }
}
