//! Exercise MCP Apps bindings and negotiated backend routes through ACP stdio.

use std::{process::Stdio, time::Duration};

#[tokio::test]
async fn mcp_apps_bindings_support_resource_reads_tool_calls_and_replay() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let directory = tempfile::tempdir().expect("create a protocol workspace");
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(python)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/mcp_apps.py"
            ))
            .arg(env!("CARGO_BIN_EXE_codex-acp-v2"))
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("MCP Apps protocol fixture timed out")
    .expect("Python 3 is required for protocol fixtures");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
