use std::fs;

use serde_json::json;

use super::resolve_bundled;

#[test]
fn bundled_lookup_is_installation_scoped_and_rejects_redirected_or_incomplete_packages() {
    let installation = tempfile::tempdir().unwrap();
    let adapter = installation.path().join("adapter");
    assert!(
        resolve_bundled(&adapter)
            .unwrap_err()
            .to_string()
            .contains("Extract the complete release archive")
    );
    let directory = installation.path().join("codex");
    fs::create_dir(&directory).unwrap();
    let (target, entrypoint, helpers): (&str, &str, &[&str]) = if cfg!(windows) {
        (
            "x86_64-pc-windows-msvc",
            "bin/codex-app-server.exe",
            &[
                "bin/codex-code-mode-host.exe",
                "codex-path/rg.exe",
                "codex-resources/codex-command-runner.exe",
                "codex-resources/codex-windows-sandbox-setup.exe",
            ],
        )
    } else if cfg!(target_os = "macos") {
        (
            "aarch64-apple-darwin",
            "bin/codex-app-server",
            &[
                "bin/codex-code-mode-host",
                "codex-path/rg",
                "codex-resources/zsh/bin/zsh",
            ],
        )
    } else {
        (
            if cfg!(target_arch = "aarch64") {
                "aarch64-unknown-linux-musl"
            } else {
                "x86_64-unknown-linux-musl"
            },
            "bin/codex-app-server",
            &[
                "bin/codex-code-mode-host",
                "codex-path/rg",
                "codex-resources/bwrap",
                "codex-resources/zsh/bin/zsh",
            ],
        )
    };
    let mut manifest = json!({
        "layoutVersion":1, "target":target, "variant":"codex-app-server",
        "entrypoint":entrypoint, "resourcesDir":"codex-resources", "pathDir":"codex-path",
    });
    fs::write(directory.join("codex-package.json"), manifest.to_string()).unwrap();
    for relative in std::iter::once(entrypoint).chain(helpers.iter().copied()) {
        let file = directory.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"fixture executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    assert_eq!(
        resolve_bundled(&adapter).unwrap(),
        directory.join(entrypoint)
    );

    manifest["entrypoint"] = json!("../../outside-executable");
    fs::write(directory.join("codex-package.json"), manifest.to_string()).unwrap();
    assert!(
        resolve_bundled(&adapter)
            .unwrap_err()
            .to_string()
            .contains("package layout or target")
    );
    manifest["entrypoint"] = json!(entrypoint);
    fs::write(directory.join("codex-package.json"), manifest.to_string()).unwrap();
    let helper = directory.join(helpers[0]);
    fs::remove_file(&helper).unwrap();
    assert!(
        resolve_bundled(&adapter)
            .unwrap_err()
            .to_string()
            .contains(helpers[0].rsplit('/').next().unwrap())
    );

    fs::write(directory.join("codex-package.json"), vec![b' '; 65_537]).unwrap();
    assert!(
        resolve_bundled(&adapter)
            .unwrap_err()
            .to_string()
            .contains("exceeds 64 KiB")
    );
}
