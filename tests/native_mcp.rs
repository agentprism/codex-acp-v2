//! Exercise the real adapter's native ACP envelopes and protected HTTP bridge.
use std::{process::Stdio, time::Duration};

#[tokio::test]
async fn native_mcp_bridges_full_duplex_and_releases_failed_and_closed_sessions() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        tokio::process::Command::new(python)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/native_mcp_probe.py"
            ))
            .arg(env!("CARGO_BIN_EXE_codex-acp-v2"))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("native MCP bridge probe timed out")
    .expect("Python 3 is required for protocol fixtures");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
