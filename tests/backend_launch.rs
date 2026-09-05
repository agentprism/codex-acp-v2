#![cfg(not(feature = "bundled-backend"))]

use std::{process::Stdio, time::Duration};

use tokio::process::Command;

#[tokio::test]
async fn missing_bundle_is_an_actionable_protocol_silent_error_and_overrides_are_exclusive() {
    let installation = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) {
        "adapter.exe"
    } else {
        "adapter"
    };
    let adapter = installation.path().join(name);
    std::fs::copy(env!("CARGO_BIN_EXE_codex-acp-v2"), &adapter).unwrap();
    let missing = Command::new(&adapter)
        .env_remove("CODEX_PATH")
        .env_remove("CODEX_APP_SERVER_PATH")
        .current_dir(installation.path())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), missing)
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("bundled Codex backend unavailable or invalid"),
        "{error}"
    );
    assert!(error.contains("--codex-path codex"), "{error}");

    let output = Command::new(&adapter)
        .args([
            "--codex-path",
            "explicit-cli",
            "--app-server-path",
            "explicit-server",
        ])
        .env_remove("CODEX_PATH")
        .env_remove("CODEX_APP_SERVER_PATH")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cannot be used with")
    );
}
