//! Client-visible projection only: Codex exclusively owns model history and context.

mod children;
mod diagnostics;
mod items;
mod patches;
mod rich_content;
mod tools;

pub use children::child_item_id;

use agent_client_protocol::schema::v2;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;

const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPLAY_BYTES: usize = 64 * 1024 * 1024;

/// Per-session event projection. No accumulated transcript is retained in memory.
#[derive(Default)]
pub struct Projector {}

impl Projector {
    /// Project one history item without inventing foreground state transitions.
    pub fn replay_item(&mut self, item: &Value) -> Result<Vec<v2::SessionUpdate>> {
        ensure!(
            serde_json::to_vec(item)?.len() <= MAX_EVENT_BYTES,
            "history item exceeds 16 MiB"
        );
        items::project(item, item["status"] != "inProgress")
    }

    /// Translate a supported notification. Other public events remain available via extensions.
    pub fn project(&mut self, method: &str, params: &Value) -> Result<Vec<v2::SessionUpdate>> {
        ensure!(
            serde_json::to_vec(params)?.len() <= MAX_EVENT_BYTES,
            "backend event exceeds 16 MiB"
        );
        let updates = match method {
            "turn/started" => vec![running()],
            "turn/completed" => {
                let turn = &params["turn"];
                let mut updates = self.replay_turn_diagnostic(turn)?;
                if let Some(timestamp) = turn["completedAt"].as_i64() {
                    updates.push(session_activity(timestamp)?);
                }
                updates.push(idle(turn));
                updates
            }
            "error" => vec![diagnostics::error(
                required(params, "turnId")?,
                &params["error"],
                params["willRetry"].as_bool().unwrap_or(false),
            )?],
            "thread/name/updated" => vec![v2::SessionUpdate::SessionInfoUpdate(
                v2::SessionInfoUpdate::new()
                    .title(params["threadName"].as_str().map(str::to_owned)),
            )],
            "item/started" | "item/completed" => {
                items::project(&params["item"], method == "item/completed")?
            }
            "item/agentMessage/delta" | "item/plan/delta" => {
                vec![v2::SessionUpdate::AgentMessageChunk(v2::ContentChunk::new(
                    text(required(params, "delta")?),
                    required(params, "itemId")?,
                ))]
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                let part = if method.contains("summaryText") {
                    "summary"
                } else {
                    "content"
                };
                let index = params[if part == "summary" {
                    "summaryIndex"
                } else {
                    "contentIndex"
                }]
                .as_u64()
                .unwrap_or_default();
                vec![v2::SessionUpdate::AgentThoughtChunk(v2::ContentChunk::new(
                    text(required(params, "delta")?),
                    format!("{}:{part}:{index}", required(params, "itemId")?),
                ))]
            }
            "item/commandExecution/outputDelta" => vec![v2::SessionUpdate::TerminalOutputChunk(
                v2::TerminalOutputChunk::new(
                    terminal_id(required(params, "itemId")?),
                    STANDARD.encode(required(params, "delta")?),
                ),
            )],
            "item/commandExecution/terminalInteraction" => {
                vec![v2::SessionUpdate::ToolCallContentChunk(
                    v2::ToolCallContentChunk::new(
                        required(params, "itemId")?,
                        text(format!(
                            "Input sent to process {}: {}",
                            required(params, "processId")?,
                            required(params, "stdin")?
                        )),
                    ),
                )]
            }
            "item/fileChange/outputDelta" | "item/mcpToolCall/progress" => {
                let content = params
                    .get("delta")
                    .or_else(|| params.get("message"))
                    .and_then(Value::as_str);
                if let Some(content) = content {
                    vec![v2::SessionUpdate::ToolCallContentChunk(
                        v2::ToolCallContentChunk::new(required(params, "itemId")?, text(content)),
                    )]
                } else {
                    vec![]
                }
            }
            "item/fileChange/patchUpdated" => tools::patch_update(params)?,
            "turn/plan/updated" => vec![plan(params)?],
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                match (
                    usage["last"]["totalTokens"].as_u64(),
                    usage["modelContextWindow"].as_u64(),
                ) {
                    (Some(used), Some(size)) => vec![v2::SessionUpdate::UsageUpdate(
                        v2::UsageUpdate::new(used, size),
                    )],
                    _ => vec![],
                }
            }
            _ => vec![],
        };
        Ok(updates)
    }

    /// Reproduce a persisted turn failure without changing foreground state.
    pub fn replay_turn_diagnostic(&mut self, turn: &Value) -> Result<Vec<v2::SessionUpdate>> {
        if turn["status"] != "failed" {
            return Ok(Vec::new());
        }
        Ok(vec![diagnostics::error(
            required(turn, "id")?,
            &turn["error"],
            false,
        )?])
    }

    /// Replay complete hydrated turns in source order, without making inference requests.
    /// The caller must hydrate paginated turns/items before requesting full replay.
    pub fn replay(&mut self, thread: &Value) -> Result<Vec<v2::SessionUpdate>> {
        ensure!(
            serde_json::to_vec(thread)?.len() <= MAX_REPLAY_BYTES,
            "history exceeds 64 MiB replay limit"
        );
        let turns = thread["turns"]
            .as_array()
            .context("thread replay requires turns")?;
        let mut updates = Vec::new();
        for turn in turns {
            for item in turn["items"]
                .as_array()
                .context("turn replay requires items")?
            {
                updates.extend(items::project(item, turn["status"] != "inProgress")?);
            }
            updates.extend(self.replay_turn_diagnostic(turn)?);
        }
        if let Some(turn) = turns.last() {
            updates.push(if turn["status"] == "inProgress" {
                running()
            } else {
                idle(turn)
            });
        }
        Ok(updates)
    }
}

fn session_activity(timestamp: i64) -> Result<v2::SessionUpdate> {
    let timestamp = time::OffsetDateTime::from_unix_timestamp(timestamp)?
        .format(&time::format_description::well_known::Rfc3339)?;
    Ok(v2::SessionUpdate::SessionInfoUpdate(
        v2::SessionInfoUpdate::new().updated_at(timestamp),
    ))
}

pub(crate) fn text(value: impl Into<String>) -> v2::ContentBlock {
    v2::ContentBlock::Text(v2::TextContent::new(value))
}

pub(crate) fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing backend field {key}"))
}

pub(crate) fn terminal_id(item_id: &str) -> String {
    format!("{item_id}:terminal")
}

fn running() -> v2::SessionUpdate {
    v2::SessionUpdate::StateUpdate(v2::StateUpdate::Running(v2::RunningStateUpdate::new()))
}

fn idle(turn: &Value) -> v2::SessionUpdate {
    let reason = match turn["status"].as_str() {
        Some("completed") => v2::StopReason::EndTurn,
        Some("interrupted") => v2::StopReason::Cancelled,
        _ => v2::StopReason::Other("_codex_failed".into()),
    };
    v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(
        v2::IdleStateUpdate::new().stop_reason(reason),
    ))
}

fn plan(params: &Value) -> Result<v2::SessionUpdate> {
    let entries = params["plan"]
        .as_array()
        .context("missing plan entries")?
        .iter()
        .map(|entry| {
            let status = match entry["status"].as_str() {
                Some("completed") => v2::PlanEntryStatus::Completed,
                Some("inProgress") => v2::PlanEntryStatus::InProgress,
                _ => v2::PlanEntryStatus::Pending,
            };
            Ok(v2::PlanEntry::new(
                required(entry, "step")?,
                v2::PlanEntryPriority::Medium,
                status,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(v2::SessionUpdate::PlanUpdate(v2::PlanUpdate::new(
        v2::PlanUpdateContent::Items(v2::PlanItems::new(
            format!("{}:plan", required(params, "turnId")?),
            entries,
        )),
    )))
}
