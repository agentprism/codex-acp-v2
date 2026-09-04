use agent_client_protocol::schema::v2;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;

use super::{required, terminal_id, text};

pub(super) fn project(item: &Value, completed: bool) -> Result<Vec<v2::SessionUpdate>> {
    let id = required(item, "id")?;
    let item_type = required(item, "type")?;
    let status = match item["status"].as_str() {
        Some("failed" | "declined") => v2::ToolCallStatus::Failed,
        Some("cancelled" | "interrupted") => v2::ToolCallStatus::Cancelled,
        Some("completed") => v2::ToolCallStatus::Completed,
        Some("inProgress") => v2::ToolCallStatus::InProgress,
        _ if completed => v2::ToolCallStatus::Completed,
        _ => v2::ToolCallStatus::InProgress,
    };
    let (title, kind) = match item_type {
        "commandExecution" => (
            item["command"].as_str().unwrap_or("Run command").to_owned(),
            v2::ToolKind::Execute,
        ),
        "fileChange" => ("Edit files".into(), v2::ToolKind::Edit),
        "mcpToolCall" => (
            format!("{}: {}", required(item, "server")?, required(item, "tool")?),
            v2::ToolKind::Other,
        ),
        "dynamicToolCall" => (required(item, "tool")?.into(), v2::ToolKind::Other),
        "webSearch" => ("Search the web".into(), v2::ToolKind::Search),
        "imageView" => (
            format!("View {}", required(item, "path")?),
            v2::ToolKind::Read,
        ),
        "imageGeneration" => ("Generate image".into(), v2::ToolKind::Other),
        "collabAgentToolCall" => (
            format!("Subagent: {}", item["tool"].as_str().unwrap_or("activity")),
            v2::ToolKind::Other,
        ),
        "subAgentActivity" => (
            format!(
                "Subagent {}",
                item["agentPath"].as_str().unwrap_or("activity")
            ),
            v2::ToolKind::Other,
        ),
        "sleep" => ("Wait".into(), v2::ToolKind::Other),
        _ => (item_type.into(), v2::ToolKind::Other),
    };
    let mut update = v2::ToolCallUpdate::new(id)
        .title(title)
        .kind(kind)
        .status(status);
    let mut before = Vec::new();
    if let Some(arguments) = item.get("arguments") {
        update = update.raw_input(arguments.clone());
    }
    let mut content = Vec::new();
    match item_type {
        "commandExecution" => {
            let mut terminal = v2::TerminalUpdate::new(terminal_id(id));
            if let Some(command) = item["command"].as_str() {
                terminal = terminal.command(command.to_owned());
            }
            if let Ok(cwd) = serde_json::from_value::<v2::AbsolutePath>(item["cwd"].clone()) {
                terminal = terminal.cwd(cwd);
            }
            if let Some(output) = item["aggregatedOutput"].as_str() {
                terminal = terminal.output(v2::TerminalOutput::new(STANDARD.encode(output)));
            }
            // A completed exec tool can leave its process running in the background.
            // Concrete exit evidence, not item completion, closes the display terminal.
            if let Some(code) = item["exitCode"].as_i64() {
                let mut exit = v2::TerminalExitStatus::new();
                if let Ok(code) = u32::try_from(code) {
                    exit = exit.exit_code(code);
                }
                terminal = terminal.exit_status(exit);
            }
            before.push(v2::SessionUpdate::TerminalUpdate(terminal));
            content.push(v2::ToolCallContent::Terminal(v2::Terminal::new(
                terminal_id(id),
            )));
            update =
                update.raw_input(serde_json::json!({"command":item["command"], "cwd":item["cwd"]}));
            if completed {
                update = update.raw_output(serde_json::json!({"processId":item["processId"], "exitCode":item["exitCode"], "durationMs":item["durationMs"]}));
            }
        }
        "fileChange" => {
            let (diffs, locations) = file_changes(&item["changes"])?;
            content.extend(diffs);
            update = update.locations(locations);
        }
        "mcpToolCall" => {
            if let Some(result) = item.get("result").filter(|result| !result.is_null()) {
                if let Some(blocks) = result["content"].as_array() {
                    for block in blocks {
                        // MCP and ACP share standard content schemas; unknown content remains raw output.
                        if let Ok(block) = serde_json::from_value::<v2::ContentBlock>(block.clone())
                        {
                            content.push(block.into());
                        }
                    }
                }
                update = update.raw_output(result.clone());
            }
            if let Some(error) = item.get("error").filter(|error| !error.is_null()) {
                content.push(text(error.to_string()).into());
                update = update
                    .raw_output(error.clone())
                    .status(v2::ToolCallStatus::Failed);
            }
        }
        "dynamicToolCall" => {
            if let Some(blocks) = item["contentItems"].as_array() {
                for block in blocks {
                    if let Some(value) = block["text"].as_str() {
                        content.push(text(value).into());
                    } else if let Some(url) = block["imageUrl"].as_str()
                        && let Some((mime, data)) = url
                            .strip_prefix("data:")
                            .and_then(|value| value.split_once(";base64,"))
                    {
                        content.push(
                            v2::ContentBlock::Image(v2::ImageContent::new(data, mime)).into(),
                        );
                    }
                }
                update = update.raw_output(item["contentItems"].clone());
            }
            if item["success"] == false {
                update = update.status(v2::ToolCallStatus::Failed);
            }
        }
        _ => {
            update = update.raw_output(item.clone());
        }
    }
    before.push(v2::SessionUpdate::ToolCallUpdate(update.content(content)));
    Ok(before)
}

pub(super) fn patch_update(params: &Value) -> Result<Vec<v2::SessionUpdate>> {
    let (content, locations) = file_changes(&params["changes"])?;
    Ok(vec![v2::SessionUpdate::ToolCallUpdate(
        v2::ToolCallUpdate::new(required(params, "itemId")?)
            .content(content)
            .locations(locations),
    )])
}

fn file_changes(changes: &Value) -> Result<(Vec<v2::ToolCallContent>, Vec<v2::ToolCallLocation>)> {
    let changes = changes.as_array().context("missing file changes")?;
    let mut content = Vec::new();
    let mut locations = Vec::new();
    for change in changes {
        let path = required(change, "path")?;
        let patch = required(change, "diff")?;
        // Foreign execution environments can return paths not expressible by this ACP client.
        // Keep the actual patch as text in that case; do not read either file to invent snapshots.
        if let Ok(path) = serde_json::from_value::<v2::AbsolutePath>(Value::String(path.to_owned()))
        {
            locations.push(v2::ToolCallLocation::new(path.clone()));
            let operation = match change["kind"]["type"].as_str() {
                Some("add") => v2::DiffChange::add(path),
                Some("delete") => v2::DiffChange::delete(path),
                _ => v2::DiffChange::modify(path),
            };
            content.push(v2::ToolCallContent::Diff(v2::Diff::patch(
                patch,
                vec![operation],
            )));
        } else {
            content.push(text(format!("{path}\n{patch}")).into());
        }
    }
    Ok((content, locations))
}
