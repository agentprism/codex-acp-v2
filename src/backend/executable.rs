use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::process::Command;

use super::BackendError;

/// Selects a packaged standalone backend or an explicit executable override.
///
/// Bundled lookup is anchored to the adapter executable, never its working
/// directory or PATH. The other variants identify the executable's CLI contract;
/// no mode is inferred from its filename.
#[derive(Clone, Debug, Default)]
pub enum BackendExecutable {
    #[default]
    Bundled,
    /// A full Codex CLI, invoked with `app-server --stdio` after extra arguments.
    CodexCli(PathBuf),
    /// A standalone app-server, invoked with `--listen stdio://`.
    AppServer(PathBuf),
}

impl BackendExecutable {
    pub(super) fn command(
        &self,
        arguments: &[OsString],
    ) -> Result<(PathBuf, Command), BackendError> {
        let executable = match self {
            Self::Bundled => {
                let adapter = std::env::current_exe()
                    .and_then(fs::canonicalize)
                    .map_err(|error| bundle_error(format!("cannot locate adapter: {error}")))?;
                resolve_bundled(&adapter)?
            }
            Self::CodexCli(path) | Self::AppServer(path) => path.clone(),
        };
        let mut command = Command::new(&executable);
        command.args(arguments);
        match self {
            Self::CodexCli(_) => command.args(["app-server", "--stdio"]),
            Self::Bundled | Self::AppServer(_) => command.args(["--listen", "stdio://"]),
        };
        Ok((executable, command))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageManifest {
    layout_version: u32,
    target: String,
    variant: String,
    entrypoint: String,
    resources_dir: String,
    path_dir: String,
}

fn resolve_bundled(adapter: &Path) -> Result<PathBuf, BackendError> {
    let directory = adapter
        .parent()
        .ok_or_else(|| bundle_error("adapter has no parent directory"))?
        .join("codex");
    let manifest_path = directory.join("codex-package.json");
    let mut bytes = Vec::new();
    fs::File::open(&manifest_path)
        .and_then(|file| file.take(65_537).read_to_end(&mut bytes))
        .map_err(|error| bundle_error(format!("{}: {error}", manifest_path.display())))?;
    if bytes.len() > 65_536 {
        return Err(bundle_error("package manifest exceeds 64 KiB"));
    }
    let manifest: PackageManifest = serde_json::from_slice(&bytes)
        .map_err(|error| bundle_error(format!("invalid package manifest: {error}")))?;
    let (target, entrypoint, helpers): (&str, &str, &[&str]) =
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", architecture @ ("x86_64" | "aarch64")) => (
                if architecture == "x86_64" {
                    "x86_64-unknown-linux-musl"
                } else {
                    "aarch64-unknown-linux-musl"
                },
                "bin/codex-app-server",
                &[
                    "bin/codex-code-mode-host",
                    "codex-path/rg",
                    "codex-resources/bwrap",
                    "codex-resources/zsh/bin/zsh",
                ],
            ),
            ("macos", "aarch64") => (
                "aarch64-apple-darwin",
                "bin/codex-app-server",
                &[
                    "bin/codex-code-mode-host",
                    "codex-path/rg",
                    "codex-resources/zsh/bin/zsh",
                ],
            ),
            ("windows", "x86_64") => (
                "x86_64-pc-windows-msvc",
                "bin/codex-app-server.exe",
                &[
                    "bin/codex-code-mode-host.exe",
                    "codex-path/rg.exe",
                    "codex-resources/codex-command-runner.exe",
                    "codex-resources/codex-windows-sandbox-setup.exe",
                ],
            ),
            (os, arch) => {
                return Err(bundle_error(format!(
                    "no bundled backend supports {os}/{arch}"
                )));
            }
        };
    if manifest.layout_version != 1
        || manifest.variant != "codex-app-server"
        || manifest.target != target
        || manifest.entrypoint != entrypoint
        || manifest.resources_dir != "codex-resources"
        || manifest.path_dir != "codex-path"
    {
        return Err(bundle_error(format!(
            "package layout or target does not match this adapter ({target})"
        )));
    }
    // Paths are fixed by the supported package layout, never taken from peer JSON.
    // Validate every shipped helper before startup; missing resources must not
    // silently send execution to an unrelated installation on PATH.
    for relative in std::iter::once(entrypoint).chain(helpers.iter().copied()) {
        let path = directory.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| bundle_error(format!("{}: {error}", path.display())))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(bundle_error(format!(
                "{} is not a nonempty regular executable",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(bundle_error(format!(
                    "{} is not executable",
                    path.display()
                )));
            }
        }
    }
    Ok(directory.join(entrypoint))
}

fn bundle_error(message: impl std::fmt::Display) -> BackendError {
    BackendError::Configuration(format!(
        "bundled Codex backend unavailable or invalid: {message}. Extract the complete release archive and keep its codex directory beside the adapter; source builds can explicitly use --codex-path codex or --app-server-path PATH"
    ))
}

#[cfg(test)]
#[path = "executable_tests.rs"]
mod tests;
