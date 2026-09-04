//! Exercise callback negotiation, host authority, and lossless answers through ACP.

#[test]
fn advanced_callbacks_preserve_consent_and_errors_with_explicit_host_authority() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/advanced_callbacks.py"
        ))
        .arg(env!("CARGO_BIN_EXE_codex-acp-v2"))
        .output()
        .expect("Python 3 is required for the callback protocol fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
