use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::runtime_cache;

const MANIFEST: &str = "BUNDLE-MANIFEST.json";
const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 1024 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    files: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    size: u64,
    sha256: String,
    executable: bool,
}

pub(super) fn install(payload: &[u8], digest: &str, cache: &Path) -> anyhow::Result<PathBuf> {
    ensure!(
        payload.len() <= 256 * 1024 * 1024,
        "embedded payload exceeds 256 MiB"
    );
    ensure!(
        hex_digest(payload) == digest,
        "embedded payload SHA-256 mismatch"
    );
    let mut archive = ZipArchive::new(Cursor::new(payload)).context("read embedded ZIP")?;
    ensure!(
        !archive.is_empty() && archive.len() <= 4096,
        "invalid embedded file count"
    );
    let mut manifest_bytes = Vec::new();
    archive
        .by_name(MANIFEST)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut manifest_bytes)?;
    ensure!(
        manifest_bytes.len() as u64 <= MAX_MANIFEST,
        "embedded manifest exceeds 1 MiB"
    );
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("read embedded manifest")?;
    ensure!(
        manifest.schema_version == 1,
        "unsupported embedded manifest version"
    );
    ensure!(
        manifest.files.len() + 1 == archive.len(),
        "embedded manifest file count mismatch"
    );
    let mut expected = BTreeMap::new();
    let mut folded = BTreeSet::from([MANIFEST.to_lowercase()]);
    let mut expanded = manifest_bytes.len() as u64;
    for entry in &manifest.files {
        validate_path(&entry.path)?;
        ensure!(
            entry.path != MANIFEST && folded.insert(entry.path.to_lowercase()),
            "duplicate embedded path"
        );
        ensure!(entry.size <= MAX_FILE, "embedded file exceeds 512 MiB");
        expanded = expanded
            .checked_add(entry.size)
            .context("embedded size overflow")?;
        ensure!(
            expanded <= MAX_EXPANDED,
            "embedded payload exceeds 1 GiB expanded"
        );
        ensure!(
            entry.sha256.len() == 64
                && entry
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "invalid embedded file digest"
        );
        expected.insert(entry.path.as_str(), entry);
    }
    let mut seen = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name();
        validate_path(name)?;
        ensure!(seen.insert(name.to_owned()), "duplicate embedded ZIP entry");
        let mode = entry
            .unix_mode()
            .context("embedded ZIP has no regular-file mode")?;
        ensure!(
            mode & 0o170000 == 0o100000 && entry.is_file(),
            "embedded ZIP contains a link or non-regular file"
        );
        if name == MANIFEST {
            ensure!(
                entry.size() == manifest_bytes.len() as u64 && mode & 0o111 == 0,
                "embedded manifest metadata mismatch"
            );
        } else {
            let specification = expected.get(name).context("unlisted embedded ZIP file")?;
            ensure!(
                entry.size() == specification.size
                    && (mode & 0o111 != 0) == specification.executable,
                "embedded ZIP file metadata mismatch"
            );
        }
    }
    let (cache, _lock) = runtime_cache::prepare(cache)?;
    let destination = cache.join(digest);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            verify(&destination, &manifest, &manifest_bytes).with_context(|| format!(
                "cached runtime is invalid; stop its running instances, remove only {}, and retry",
                destination.display()
            ))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let staging = tempfile::Builder::new()
                .prefix(".install-")
                .tempdir_in(&cache)?;
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index)?;
                let path = staging.path().join(entry.name());
                fs::create_dir_all(path.parent().context("embedded file has no parent")?)?;
                let mut file = File::create_new(&path)?;
                let size = entry.size();
                let actual = io::copy(&mut entry.by_ref().take(size + 1), &mut file)?;
                ensure!(
                    actual == size,
                    "embedded file decompressed to an unexpected size"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if entry.unix_mode().unwrap_or(0) & 0o111 != 0 {
                        0o755
                    } else {
                        0o644
                    };
                    file.set_permissions(fs::Permissions::from_mode(mode))?;
                }
                file.sync_all()?;
            }
            verify(staging.path(), &manifest, &manifest_bytes)
                .context("verify extracted embedded runtime")?;
            fs::rename(staging.path(), &destination)
                .context("atomically install embedded runtime")?;
        }
        Err(error) => return Err(error).context("inspect runtime cache"),
    }
    Ok(destination)
}

fn validate_path(name: &str) -> anyhow::Result<()> {
    let path = Path::new(name);
    ensure!(
        !name.is_empty()
            && !name.contains(['\\', ':'])
            && !name.ends_with('/')
            && !name.contains("//")
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && name.split('/').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part.chars().any(char::is_control)
                && !matches!(
                    part.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )),
        "unsafe embedded path: {name}"
    );
    Ok(())
}

fn verify(root: &Path, manifest: &Manifest, manifest_bytes: &[u8]) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    ensure!(
        metadata.is_dir() && !runtime_cache::is_link(&metadata),
        "runtime root is not a real directory"
    );
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for path in manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .chain(std::iter::once(MANIFEST))
    {
        files.insert(root.join(path));
        let mut parent = Path::new(path).parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            directories.insert(root.join(path));
            parent = path.parent();
        }
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                !runtime_cache::is_link(&metadata),
                "cached runtime contains a link: {}",
                path.display()
            );
            if metadata.is_dir() {
                ensure!(
                    directories.contains(&path),
                    "cached runtime contains an unexpected directory"
                );
                pending.push(path);
            } else {
                ensure!(
                    metadata.is_file() && files.contains(&path),
                    "cached runtime contains an unexpected file"
                );
            }
        }
    }
    let mut actual_manifest = Vec::new();
    File::open(root.join(MANIFEST))?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut actual_manifest)?;
    ensure!(
        actual_manifest == manifest_bytes,
        "cached runtime manifest differs from embedded bytes"
    );
    for entry in &manifest.files {
        let path = root.join(&entry.path);
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file()
                && !runtime_cache::is_link(&metadata)
                && metadata.len() == entry.size,
            "cached runtime size/type mismatch: {}",
            entry.path
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                (metadata.permissions().mode() & 0o111 != 0) == entry.executable,
                "cached runtime executable mode mismatch: {}",
                entry.path
            );
        }
        let mut source = File::open(path)?.take(entry.size + 1);
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        ensure!(
            hex(&hasher.finalize()) == entry.sha256,
            "cached runtime SHA-256 mismatch: {}",
            entry.path
        );
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 0x0f)]),
            ]
        })
        .collect()
}

#[cfg(test)]
#[path = "embedded_tests.rs"]
mod tests;
