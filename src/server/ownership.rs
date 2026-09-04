use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use super::Server;
use crate::backend::BackendError;

impl Server {
    /// Resolve descendant authority solely from backend thread metadata, never client claims.
    /// Children share their open ACP root's callback UI, but not its foreground state.
    pub(super) async fn owner_for_thread(&self, thread_id: &str) -> Result<Option<String>> {
        let mut current = thread_id.to_owned();
        let mut visited = HashSet::new();
        for _ in 0..32 {
            let state = self.state.lock().await;
            let root = if state.sessions.contains_key(&current) {
                Some(current.clone())
            } else {
                state
                    .descendant_roots
                    .get(&current)
                    .filter(|root| state.sessions.contains_key(*root))
                    .cloned()
            };
            let candidate = root.and_then(|root| {
                state
                    .sessions
                    .get(&root)
                    .cloned()
                    .map(|session| (root, session))
            });
            drop(state);
            if let Some((root, session)) = candidate {
                if !session.data.lock().await.open {
                    return Ok(None);
                }
                let mut state = self.state.lock().await;
                if !state
                    .sessions
                    .get(&root)
                    .is_some_and(|current| Arc::ptr_eq(current, &session))
                {
                    return Ok(None);
                }
                // Closed roots release all their cached descendants before the next insertion.
                let open: HashSet<_> = state.sessions.keys().cloned().collect();
                state
                    .descendant_roots
                    .retain(|_, owner| open.contains(owner));
                ensure!(
                    state.descendant_roots.len() + visited.len() <= 4096,
                    "descendant ownership cache exceeds limit"
                );
                for child in visited {
                    state.descendant_roots.insert(child, root.clone());
                }
                return Ok(Some(root));
            }
            ensure!(
                visited.insert(current.clone()),
                "cycle in backend thread ancestry"
            );
            let response = match self
                .backend
                .request(
                    "thread/read",
                    json!({"threadId":current,"includeTurns":false}),
                )
                .await
            {
                Ok(response) => response,
                // Missing, expired, or inaccessible threads confer no authority.
                Err(BackendError::Rpc(_)) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            ensure!(
                response["thread"]["id"].as_str() == Some(current.as_str()),
                "backend returned metadata for a different thread"
            );
            let parent = &response["thread"]["parentThreadId"];
            if parent.is_null() {
                return Ok(None);
            }
            current = parent
                .as_str()
                .context("invalid backend parentThreadId")?
                .to_owned();
        }
        anyhow::bail!("backend thread ancestry exceeds 32 levels")
    }
}

pub(super) fn child_item_id(thread_id: &str, item_id: &str) -> String {
    format!("codex-child:{thread_id}:{item_id}")
}
