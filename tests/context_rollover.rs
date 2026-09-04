//! Verify that Codex context resets never replace ACP sessions or author model input.

#[test]
fn backend_context_rollover_keeps_session_and_visible_entities_without_reinjection() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/context_rollover.py"
        ))
        .arg(env!("CARGO_BIN_EXE_codex-acp-v2"))
        .output()
        .expect("Python 3 is required for the context protocol fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
