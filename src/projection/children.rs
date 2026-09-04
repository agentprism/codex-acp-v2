use agent_client_protocol::schema::v2;
use anyhow::Result;
use serde_json::Value;

use super::{Projector, required};

/// Stable identity for a descendant tool projected into its root ACP session.
pub fn child_item_id(thread_id: &str, item_id: &str) -> String {
    format!("codex-child:{thread_id}:{item_id}")
}

impl Projector {
    /// Project child tools without exposing child foreground state or reasoning as the parent's.
    pub fn project_child(
        &mut self,
        method: &str,
        params: &Value,
        thread_id: &str,
    ) -> Result<Vec<v2::SessionUpdate>> {
        let item_event =
            matches!(method, "item/started" | "item/completed") && child_tool(&params["item"]);
        let tool_event = matches!(
            method,
            "item/commandExecution/outputDelta"
                | "item/commandExecution/terminalInteraction"
                | "item/fileChange/outputDelta"
                | "item/fileChange/patchUpdated"
                | "item/mcpToolCall/progress"
        );
        if !item_event && !tool_event {
            return Ok(Vec::new());
        }
        let mut projected = params.clone();
        if item_event {
            projected["item"]["id"] =
                Value::String(child_item_id(thread_id, required(&params["item"], "id")?));
        } else {
            projected["itemId"] =
                Value::String(child_item_id(thread_id, required(params, "itemId")?));
        }
        self.project(method, &projected)
    }

    /// Replay the same descendant tool surface and identities emitted by `project_child`.
    pub fn replay_child_item(
        &mut self,
        item: &Value,
        thread_id: &str,
    ) -> Result<Vec<v2::SessionUpdate>> {
        if !child_tool(item) {
            return Ok(Vec::new());
        }
        let mut item = item.clone();
        item["id"] = Value::String(child_item_id(thread_id, required(&item, "id")?));
        self.replay_item(&item)
    }
}

fn child_tool(item: &Value) -> bool {
    matches!(
        item["type"].as_str(),
        Some(
            "commandExecution"
                | "fileChange"
                | "mcpToolCall"
                | "dynamicToolCall"
                | "webSearch"
                | "imageView"
                | "imageGeneration"
                | "sleep"
                | "collabAgentToolCall"
                | "subAgentActivity"
        )
    )
}
