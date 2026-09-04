use agent_client_protocol::schema::v2;
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;

use super::text;

/// Decode inline output or preserve its URI as a reference; never fetch model-provided URLs.
pub(super) fn media_reference(url: &str) -> v2::ContentBlock {
    if let Some((mime, data)) = url
        .strip_prefix("data:")
        .and_then(|value| value.split_once(";base64,"))
        && STANDARD.decode(data).is_ok()
    {
        if mime.starts_with("image/") {
            return v2::ContentBlock::Image(v2::ImageContent::new(data, mime));
        }
        if mime.starts_with("audio/") {
            return v2::ContentBlock::Audio(v2::AudioContent::new(data, mime));
        }
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return v2::ContentBlock::ResourceLink(v2::ResourceLink::new("Tool media", url));
    }
    text("Media output is unavailable in a supported inline format.")
}

pub(super) fn generated_image(item: &Value) -> Result<Vec<v2::ToolCallContent>> {
    let mut content = Vec::new();
    if let Some(prompt) = item["revisedPrompt"]
        .as_str()
        .filter(|prompt| !prompt.is_empty())
    {
        content.push(text(prompt).into());
    }
    if let Some(result) = item["result"].as_str().filter(|result| !result.is_empty()) {
        // Codex's image extension emits PNG bytes as base64, including persisted items.
        content.push(media_reference(&format!("data:image/png;base64,{result}")).into());
    }
    if let Some(path) = item["savedPath"].as_str() {
        content.push(text(format!("Saved in the execution environment: {path}")).into());
    }
    if let Some(failure) = item.get("failure").filter(|failure| !failure.is_null()) {
        content.push(
            text(format!(
                "Image generation failed: {}",
                serde_json::to_string_pretty(failure)?
            ))
            .into(),
        );
    }
    Ok(content)
}

pub(super) fn web_search(item: &Value) -> Result<Vec<v2::ToolCallContent>> {
    let mut content = Vec::new();
    if let Some(query) = item["query"].as_str().filter(|query| !query.is_empty()) {
        content.push(text(format!("Query: {query}")).into());
    }
    if let Some(action) = item.get("action").filter(|action| !action.is_null()) {
        content.push(
            text(format!(
                "Search action: {}",
                serde_json::to_string_pretty(action)?
            ))
            .into(),
        );
    }
    for result in item["results"].as_array().into_iter().flatten() {
        if let Some(url) = result["url"]
            .as_str()
            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        {
            let title = result["title"].as_str().unwrap_or(url);
            content.push(
                v2::ContentBlock::ResourceLink(
                    v2::ResourceLink::new(title, url)
                        .description(result["snippet"].as_str().map(str::to_owned)),
                )
                .into(),
            );
        } else {
            // The backend intentionally treats future result types as opaque JSON.
            content.push(text(serde_json::to_string_pretty(result)?).into());
        }
    }
    Ok(content)
}
