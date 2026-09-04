//! Opt-in integration through ACP, installed Codex, and loopback-only model/MCP peers.

#[test]
#[cfg(unix)]
#[ignore = "requires installed Codex, Python 3, and permission to execute harmless POSIX fixture commands"]
fn installed_codex_executes_tools_callbacks_mcp_and_replays_actual_history() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let status = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/installed_workflow.py"
        ))
        .arg(env!("CARGO_BIN_EXE_codex-acp-v2"))
        .status()
        .expect("start isolated installed-Codex workflow");
    assert!(
        status.success(),
        "installed Codex workflow failed: {status}"
    );
}
