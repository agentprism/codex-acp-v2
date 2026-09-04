//! Untrusted ACP prompt content becomes ordinary Codex user input, never instructions.

use agent_client_protocol::schema::v2;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

const MAX_PROMPT_BYTES: usize = 16 * 1024 * 1024;
const MAX_BLOCKS: usize = 256;

/// Convert an ACP prompt without fetching links or reading client-supplied paths.
pub fn prompt_to_codex(request: &v2::PromptRequest) -> Result<Vec<Value>> {
    to_codex(&request.prompt)
}

/// Convert supported content, rejecting malformed media and oversized prompts atomically.
pub fn to_codex(blocks: &[v2::ContentBlock]) -> Result<Vec<Value>> {
    ensure!(!blocks.is_empty(), "a prompt must contain content");
    ensure!(blocks.len() <= MAX_BLOCKS, "too many prompt content blocks");
    ensure!(
        serde_json::to_vec(blocks)?.len() <= MAX_PROMPT_BYTES,
        "prompt exceeds 16 MiB"
    );
    blocks.iter().map(convert).collect()
}

fn text(value: impl Into<String>) -> Value {
    json!({"type":"text", "text":value.into(), "text_elements":[]})
}

fn media(kind: &str, data: &str, mime: &str) -> Result<Value> {
    ensure!(
        mime.starts_with(&format!("{kind}/")),
        "invalid {kind} MIME type"
    );
    ensure!(
        mime.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/-+._".contains(&byte)),
        "invalid MIME type"
    );
    let decoded = STANDARD.decode(data).context("invalid base64 media")?;
    ensure!(!decoded.is_empty(), "empty media content");
    Ok(json!({"type":kind, "url":format!("data:{mime};base64,{data}")}))
}

fn convert(block: &v2::ContentBlock) -> Result<Value> {
    match block {
        v2::ContentBlock::Text(value) => Ok(text(&value.text)),
        v2::ContentBlock::Image(value) => media("image", &value.data, value.mime_type.as_ref()),
        v2::ContentBlock::Audio(value) => media("audio", &value.data, value.mime_type.as_ref()),
        v2::ContentBlock::ResourceLink(value) => Ok(text(format!(
            "Resource reference (not fetched): {}\nURI: {}{}",
            value.name,
            value.uri,
            value
                .description
                .as_ref()
                .map(|description| format!("\n{description}"))
                .unwrap_or_default()
        ))),
        v2::ContentBlock::Resource(value) => match &value.resource {
            v2::EmbeddedResourceResource::TextResourceContents(resource) => Ok(text(format!(
                "User-provided resource: {}\n{}",
                resource.uri, resource.text
            ))),
            v2::EmbeddedResourceResource::BlobResourceContents(resource) => {
                let mime = resource
                    .mime_type
                    .as_ref()
                    .map(AsRef::as_ref)
                    .unwrap_or("application/octet-stream");
                if mime.starts_with("image/") {
                    media("image", &resource.blob, mime)
                } else if mime.starts_with("audio/") {
                    media("audio", &resource.blob, mime)
                } else if mime.starts_with("text/")
                    || matches!(mime, "application/json" | "application/xml")
                {
                    let bytes = STANDARD
                        .decode(&resource.blob)
                        .context("invalid base64 resource")?;
                    let content = String::from_utf8(bytes).context("resource is not UTF-8")?;
                    Ok(text(format!(
                        "User-provided resource: {}\n{content}",
                        resource.uri
                    )))
                } else {
                    bail!("unsupported embedded binary resource MIME type: {mime}")
                }
            }
            _ => bail!("unsupported embedded resource"),
        },
        _ => bail!("unsupported ACP content block"),
    }
}
