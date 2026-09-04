//! Bounded backend-owned history replay and negotiated transcript invalidation.

use std::{collections::HashSet, sync::Arc};

use agent_client_protocol::{Client, V2ConnectionTo, schema::v2};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json, value::to_raw_value};

use super::{Server, Session};

impl Server {
    /// Caller owns `delivery`, ordering replacement snapshots before live events.
    pub(super) async fn replay(
        &self,
        id: &str,
        session: &Session,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<()> {
        let mut remaining = self.options.max_replay_items;
        self.replay_thread_items(id, id, session, connection, &mut remaining)
            .await?;
        self.replay_turn_diagnostics(id, session, connection, &mut remaining)
            .await?;
        let mut seen = HashSet::new();
        // Archived descendants may still own tool entities shown in the parent's
        // transcript, so include both storage partitions. Ordering within every
        // thread is preserved; backend history has no cross-thread event cursor.
        for archived in [false, true] {
            let mut cursor = Value::Null;
            for _ in 0..64 {
                let page = self
                    .backend
                    .request(
                        "thread/list",
                        json!({
                            "ancestorThreadId":id,"sourceKinds":["subAgent"],"archived":archived,
                            "cursor":cursor,"limit":100,"sortKey":"created_at","sortDirection":"asc"
                        }),
                    )
                    .await?;
                for thread in page["data"]
                    .as_array()
                    .context("descendant page missing data")?
                {
                    let child = thread["id"].as_str().context("descendant missing id")?;
                    if child == id || !seen.insert(child.to_owned()) {
                        continue;
                    }
                    ensure!(seen.len() <= 4096, "too many descendants to replay");
                    ensure!(
                        self.owner_for_thread(child).await?.as_deref() == Some(id),
                        "history includes an unowned descendant"
                    );
                    self.replay_thread_items(id, child, session, connection, &mut remaining)
                        .await?;
                }
                let next = page.get("nextCursor").cloned().unwrap_or(Value::Null);
                if next.is_null() {
                    cursor = Value::Null;
                    break;
                }
                ensure!(
                    next != cursor,
                    "descendant history returned a repeating cursor"
                );
                cursor = next;
            }
            ensure!(cursor.is_null(), "descendant history exceeds page limit");
        }
        Ok(())
    }

    async fn replay_thread_items(
        &self,
        root: &str,
        thread: &str,
        session: &Session,
        connection: &V2ConnectionTo<Client>,
        remaining: &mut usize,
    ) -> Result<()> {
        let mut cursor = Value::Null;
        for _ in 0..self.options.max_replay_items.max(1) {
            let page = self
                .backend
                .request(
                    "thread/items/list",
                    json!({"threadId":thread,"cursor":cursor,"sortDirection":"asc","limit":100}),
                )
                .await?;
            let entries = page["data"]
                .as_array()
                .context("thread/items/list response missing data")?;
            *remaining = remaining
                .checked_sub(entries.len())
                .context("session replay exceeds configured item limit")?;
            for entry in entries {
                let mut data = session.data.lock().await;
                if entry["item"]["status"].as_str() != Some("inProgress")
                    && let Some(item_id) = entry["item"]["id"].as_str()
                {
                    data.replayed_finalized.insert(if thread == root {
                        item_id.to_owned()
                    } else {
                        crate::projection::child_item_id(thread, item_id)
                    });
                }
                let updates = if thread == root {
                    data.projector.replay_item(&entry["item"])?
                } else {
                    data.projector.replay_child_item(&entry["item"], thread)?
                };
                drop(data);
                for update in updates {
                    self.send_update(connection, root, update).await?;
                }
            }
            let next = page.get("nextCursor").cloned().unwrap_or(Value::Null);
            if next.is_null() {
                return Ok(());
            }
            ensure!(
                next != cursor,
                "history pagination returned a repeating cursor"
            );
            cursor = next;
        }
        bail!("session replay exceeds page limit")
    }

    async fn replay_turn_diagnostics(
        &self,
        id: &str,
        session: &Session,
        connection: &V2ConnectionTo<Client>,
        remaining: &mut usize,
    ) -> Result<()> {
        let mut cursor = Value::Null;
        for _ in 0..self.options.max_replay_items.max(1) {
            let page = self.backend.request("thread/turns/list", json!({"threadId":id,"cursor":cursor,"sortDirection":"asc","limit":100,"itemsView":"summary"})).await?;
            let turns = page["data"]
                .as_array()
                .context("turn history missing data")?;
            *remaining = remaining
                .checked_sub(turns.len())
                .context("session replay exceeds configured item limit")?;
            for turn in turns {
                let updates = session
                    .data
                    .lock()
                    .await
                    .projector
                    .replay_turn_diagnostic(turn)?;
                for update in updates {
                    self.send_update(connection, id, update).await?;
                }
            }
            let next = page.get("nextCursor").cloned().unwrap_or(Value::Null);
            if next.is_null() {
                return Ok(());
            }
            ensure!(next != cursor, "turn history returned a repeating cursor");
            cursor = next;
        }
        bail!("turn history exceeds page limit")
    }

    /// ACP v2 has no message/tool deletion operation. Clients opting into history
    /// mutation must implement this reset boundary and rebuild from subsequent
    /// standard replay updates, completing at the matching `complete` boundary.
    pub(super) async fn reset_history(
        &self,
        id: &str,
        session: &Session,
        reason: &str,
        connection: &V2ConnectionTo<Client>,
    ) -> Result<()> {
        let revision = {
            let mut data = session.data.lock().await;
            data.history_revision += 1;
            data.replayed_finalized.clear();
            data.projector = crate::projection::Projector::default();
            data.history_revision
        };
        for phase in ["start", "complete"] {
            if phase == "complete" {
                self.replay(id, session, connection).await?;
            }
            connection.send_notification(v2::AgentNotification::ExtNotification(Box::new(v2::ExtNotification::new(
                "_codex/sessionReset", Arc::from(to_raw_value(&json!({"version":1,"sessionId":id,"revision":revision,"phase":phase,"reason":reason}))?)
            ))))?;
        }
        Ok(())
    }
}
