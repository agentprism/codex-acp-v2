use anyhow::Result;
use serde_json::Value;

use super::required;

/// Codex supplies snapshots for add/delete and unheaded hunks for update.
/// Snapshot operations remain structured diffs plus text: constructing complete
/// Git creation/deletion patches would require file modes that Codex does not send.
pub(super) fn git_patch(change: &Value) -> Result<Option<String>> {
    if change["kind"]["type"] != "update" {
        return Ok(None);
    }
    let source = required(change, "path")?;
    let target = change["kind"]["move_path"].as_str().unwrap_or(source);
    let diff = required(change, "diff")?;
    // Git uses C-quoted names. Do not mislabel paths with unsupported control bytes.
    if source.chars().chain(target.chars()).any(char::is_control) {
        return Ok(None);
    }
    let old = serde_json::to_string(source)?;
    let new = serde_json::to_string(target)?;
    let mut patch = format!("diff --git {old} {new}\n");
    if target != source {
        patch.push_str(&format!("rename from {old}\nrename to {new}\n"));
    }
    let diff = if target != source {
        diff.strip_suffix(&format!("\n\nMoved to: {target}"))
            .unwrap_or(diff)
    } else {
        diff
    };
    if let Some(start) = diff.find("@@ ") {
        let hunks = &diff[start..];
        patch.push_str(&format!("--- {old}\n+++ {new}\n{hunks}"));
        if !patch.ends_with('\n') {
            patch.push('\n');
        }
    } else if !diff.trim().is_empty() || target == source {
        return Ok(None);
    }
    Ok(Some(patch))
}
