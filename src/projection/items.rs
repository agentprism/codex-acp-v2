use agent_client_protocol::schema::v2;
use anyhow::Result;
use serde_json::Value;

use super::{required, text, tools};

pub(super) fn project(item: &Value, completed: bool) -> Result<Vec<v2::SessionUpdate>> {
    let id = required(item, "id")?;
    let updates = match required(item, "type")? {
        "userMessage" => {
            let id = item["clientId"].as_str().unwrap_or(id);
            let content: Vec<_> = item["content"]
                .as_array()
                .map(|blocks| blocks.iter().map(user_content).collect())
                .unwrap_or_default();
            vec![v2::SessionUpdate::UserMessage(
                v2::UserMessage::new(id).content(content),
            )]
        }
        "agentMessage" | "plan" => vec![v2::SessionUpdate::AgentMessage(
            v2::AgentMessage::new(id)
                .content(vec![text(item["text"].as_str().unwrap_or_default())]),
        )],
        "reasoning" => {
            let mut updates = Vec::new();
            for key in ["summary", "content"] {
                if let Some(parts) = item[key].as_array() {
                    for (index, part) in parts.iter().enumerate() {
                        if let Some(content) = part.as_str() {
                            updates.push(v2::SessionUpdate::AgentThought(
                                v2::AgentThought::new(format!("{id}:{key}:{index}"))
                                    .content(vec![text(content)]),
                            ));
                        }
                    }
                }
            }
            updates
        }
        // These are runtime context events, not user-authored chat or tool calls.
        "hookPrompt" | "contextCompaction" => vec![],
        "enteredReviewMode" | "exitedReviewMode" => vec![v2::SessionUpdate::AgentMessage(
            v2::AgentMessage::new(id)
                .content(vec![text(item["review"].as_str().unwrap_or_default())]),
        )],
        _ => tools::project(item, completed)?,
    };
    Ok(updates)
}

fn user_content(value: &Value) -> v2::ContentBlock {
    match value["type"].as_str() {
        Some("text") => text(value["text"].as_str().unwrap_or_default()),
        Some("image" | "audio") => {
            if let Some(url) = value["url"].as_str()
                && let Some((mime, data)) = url
                    .strip_prefix("data:")
                    .and_then(|rest| rest.split_once(";base64,"))
            {
                return if value["type"] == "image" {
                    v2::ContentBlock::Image(v2::ImageContent::new(data, mime))
                } else {
                    v2::ContentBlock::Audio(v2::AudioContent::new(data, mime))
                };
            }
            text("Media attachment (unavailable for replay)")
        }
        Some("localImage" | "localAudio" | "skill" | "mention") => text(format!(
            "{}: {}",
            value["name"].as_str().unwrap_or("Attachment"),
            value["path"].as_str().unwrap_or_default()
        )),
        _ => text("Unsupported attachment (available in Codex history)"),
    }
}
