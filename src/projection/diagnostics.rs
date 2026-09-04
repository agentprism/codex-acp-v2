use agent_client_protocol::schema::v2;
use anyhow::Result;
use serde_json::{Value, json};

use super::text;

pub(super) fn error(turn_id: &str, error: &Value, retrying: bool) -> Result<v2::SessionUpdate> {
    let message = error["message"]
        .as_str()
        .unwrap_or("Codex could not complete this turn.");
    let mut explanation = if retrying {
        format!("Codex is retrying: {message}")
    } else {
        format!("Codex error: {message}")
    };
    for detail in [
        error["additionalDetails"].as_str(),
        error
            .pointer("/misalignment/detailedExplanation")
            .and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    {
        if !detail.is_empty() && detail != message {
            explanation.push_str("\n\n");
            explanation.push_str(detail);
        }
    }
    // Notifications and final/replayed turn errors replace one diagnostic entity.
    // Continuation data is available to capable clients but is never submitted automatically.
    Ok(v2::SessionUpdate::AgentMessage(
        v2::AgentMessage::new(format!("{turn_id}:diagnostic"))
            .content(vec![text(explanation)])
            .meta(serde_json::from_value::<v2::Meta>(json!({
                "codex": {"diagnostic": error, "willRetry": retrying}
            }))?),
    ))
}
