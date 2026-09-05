use std::fs;
use std::io::{Cursor, Write};
use std::sync::Barrier;

use serde_json::json;
use zip::{ZipWriter, write::SimpleFileOptions};

use super::{MANIFEST, hex_digest, install};

fn payload(path: &str) -> Vec<u8> {
    let contents = b"independent executable fixture";
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "files": [{"path":path,"size":contents.len(),"sha256":hex_digest(contents),"executable":true}],
    })).unwrap();
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file(
            MANIFEST,
            SimpleFileOptions::default().unix_permissions(0o644),
        )
        .unwrap();
    archive.write_all(&manifest).unwrap();
    archive
        .start_file(path, SimpleFileOptions::default().unix_permissions(0o755))
        .unwrap();
    archive.write_all(contents).unwrap();
    archive.finish().unwrap().into_inner()
}

#[test]
fn concurrent_install_is_atomic_and_cached_executables_are_reverified() {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("private-cache");
    let payload = payload("codex/bin/backend");
    let digest = hex_digest(&payload);
    let barrier = Barrier::new(4);
    let installations = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    install(&payload, &digest, &cache).unwrap()
                })
            })
            .collect();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    let runtime = &installations[0];
    assert!(installations.iter().all(|path| path == runtime));
    assert_eq!(
        fs::read(runtime.join("codex/bin/backend")).unwrap(),
        b"independent executable fixture"
    );
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(runtime.join("codex/bin/backend"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    fs::write(
        runtime.join("codex/bin/backend"),
        b"compromised executable bytes!!",
    )
    .unwrap();
    let error = install(&payload, &digest, &cache).unwrap_err();
    assert!(
        format!("{error:#}").contains("SHA-256 mismatch"),
        "{error:#}"
    );
    assert!(error.to_string().contains("remove only"));
    assert_eq!(
        fs::read(runtime.join("codex/bin/backend")).unwrap(),
        b"compromised executable bytes!!"
    );
}

#[test]
fn unsafe_payload_paths_and_corrupted_payloads_never_create_cache_files() {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("private-cache");
    let payload = payload("../escaped");
    let error = install(&payload, &hex_digest(&payload), &cache).unwrap_err();
    assert!(error.to_string().contains("unsafe embedded path"));
    assert!(!cache.exists());
    assert!(!temporary.path().join("escaped").exists());
    let error = install(&payload, &"0".repeat(64), &cache).unwrap_err();
    assert!(error.to_string().contains("payload SHA-256 mismatch"));
}

#[cfg(unix)]
#[test]
fn linked_cache_or_runtime_is_rejected_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("private-cache");
    symlink(other.path(), &cache).unwrap();
    let payload = payload("codex/bin/backend");
    let digest = hex_digest(&payload);
    assert!(
        install(&payload, &digest, &cache)
            .unwrap_err()
            .to_string()
            .contains("not a link")
    );
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);

    fs::remove_file(&cache).unwrap();
    let runtime = install(&payload, &digest, &cache).unwrap();
    let executable = runtime.join("codex/bin/backend");
    fs::remove_file(&executable).unwrap();
    symlink(other.path().join("outside"), &executable).unwrap();
    let error = install(&payload, &digest, &cache).unwrap_err();
    assert!(format!("{error:#}").contains("contains a link"));
    assert!(!other.path().join("outside").exists());
}
