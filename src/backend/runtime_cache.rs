use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};

#[cfg(feature = "bundled-backend")]
pub(super) fn default_root() -> anyhow::Result<PathBuf> {
    let configured = std::env::var_os("CODEX_ACP_CACHE_DIR").map(PathBuf::from);
    let directory = if let Some(directory) = configured {
        directory
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?)
            .join("codex-acp-v2")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
            .join("Library/Caches/codex-acp-v2")
    } else if let Some(directory) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(directory).join("codex-acp-v2")
    } else {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
            .join(".cache/codex-acp-v2")
    };
    ensure!(
        directory.is_absolute(),
        "runtime cache must be an absolute path"
    );
    Ok(directory)
}

pub(super) fn prepare(root: &Path) -> anyhow::Result<(PathBuf, File)> {
    ensure!(root.is_absolute(), "runtime cache must be an absolute path");
    let parent = root.parent().context("runtime cache has no parent")?;
    fs::create_dir_all(parent).context("create runtime cache parent")?;
    let root = parent
        .canonicalize()?
        .join(root.file_name().context("runtime cache has no name")?);
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match builder.create(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("create private runtime cache"),
    }
    let metadata = fs::symlink_metadata(&root)?;
    ensure!(
        metadata.is_dir() && !is_link(&metadata),
        "runtime cache must be a real directory, not a link"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            metadata.uid() == rustix::process::geteuid().as_raw(),
            "runtime cache belongs to another user"
        );
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "runtime cache must be private (chmod 700 {})",
            root.display()
        );
    }
    let lock_path = root.join(".install.lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
        ensure!(
            metadata.is_file() && !is_link(&metadata),
            "runtime cache lock must be a regular file"
        );
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(lock_path).context("open runtime cache lock")?;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok((root, lock)),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(fs::TryLockError::WouldBlock) => {
                bail!("timed out waiting for runtime cache installation")
            }
            Err(fs::TryLockError::Error(error)) => return Err(error).context("lock runtime cache"),
        }
    }
}

pub(super) fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
