use std::{collections::HashSet, time::Duration};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::Server;

impl Server {
    /// Close live descendants only after verifying backend-owned ancestry.
    /// App-server subscriptions are per thread; unsubscribing the root is not a cascade.
    pub(super) async fn close_descendants(&self, root: &str) -> Result<()> {
        let mut processed = HashSet::new();
        // Discover descendants created while their ancestors were being interrupted.
        for _ in 0..32 {
            let mut cursor = Value::Null;
            let mut children = Vec::new();
            for _ in 0..64 {
                let page = self
                    .backend
                    .request("thread/loaded/list", json!({"cursor":cursor,"limit":100}))
                    .await?;
                for id in page["data"]
                    .as_array()
                    .context("loaded thread page missing data")?
                {
                    let id = id.as_str().context("loaded thread id must be a string")?;
                    if id != root
                        && !processed.contains(id)
                        && self.owner_for_thread(id).await?.as_deref() == Some(root)
                    {
                        ensure!(
                            children.len() + processed.len() < 4096,
                            "too many descendants to close safely"
                        );
                        children.push(id.to_owned());
                    }
                }
                let next = page.get("nextCursor").cloned().unwrap_or(Value::Null);
                if next.is_null() {
                    cursor = Value::Null;
                    break;
                }
                ensure!(next != cursor, "loaded threads returned a repeating cursor");
                cursor = next;
            }
            ensure!(
                cursor.is_null(),
                "loaded thread cleanup exceeds its page limit"
            );
            if children.is_empty() {
                self.state
                    .lock()
                    .await
                    .descendant_roots
                    .retain(|_, owner| owner != root);
                return Ok(());
            }
            for child in children {
                let read = self
                    .backend
                    .request(
                        "thread/read",
                        json!({"threadId":child,"includeTurns":false}),
                    )
                    .await?;
                if read["thread"]
                    .pointer("/status/type")
                    .and_then(Value::as_str)
                    == Some("active")
                {
                    let page = self.backend.request("thread/turns/list", json!({"threadId":child,"limit":1,"sortDirection":"desc","itemsView":"summary"})).await?;
                    let active = page["data"]
                        .as_array()
                        .context("child turn page missing data")?
                        .iter()
                        .find(|turn| turn["status"] == "inProgress");
                    if let Some(turn) = active {
                        let turn_id = turn["id"]
                            .as_str()
                            .context("active child turn missing id")?;
                        if let Err(error) = self
                            .backend
                            .request("turn/interrupt", json!({"threadId":child,"turnId":turn_id}))
                            .await
                        {
                            // A child can naturally finish between inspection and interruption.
                            let read = self
                                .backend
                                .request(
                                    "thread/read",
                                    json!({"threadId":child,"includeTurns":false}),
                                )
                                .await?;
                            if read["thread"]
                                .pointer("/status/type")
                                .and_then(Value::as_str)
                                == Some("active")
                            {
                                return Err(error.into());
                            }
                        }
                    } else {
                        // This documented sentinel interrupts startup before a turn has an id.
                        self.backend
                            .request("turn/interrupt", json!({"threadId":child,"turnId":""}))
                            .await?;
                    }
                    tokio::time::timeout(self.options.backend.request_timeout, async {
                        loop {
                            let read = self
                                .backend
                                .request(
                                    "thread/read",
                                    json!({"threadId":child,"includeTurns":false}),
                                )
                                .await?;
                            if read["thread"]
                                .pointer("/status/type")
                                .and_then(Value::as_str)
                                != Some("active")
                            {
                                return Ok::<(), anyhow::Error>(());
                            }
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                    })
                    .await
                    .context("timed out waiting for descendant interruption")??;
                }
                self.backend
                    .request(
                        "thread/backgroundTerminals/clean",
                        json!({"threadId":child}),
                    )
                    .await?;
                self.backend
                    .request("thread/unsubscribe", json!({"threadId":child}))
                    .await?;
                processed.insert(child);
            }
        }
        anyhow::bail!("descendants continued spawning beyond the cleanup limit")
    }
}
