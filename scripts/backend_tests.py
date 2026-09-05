"""Regression coverage for the downloaded-code trust and extraction boundary."""

import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import backend


class BackendPackageTests(unittest.TestCase):
    def test_redistribution_notices_cannot_be_removed_or_changed_after_review(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            notice = root / "LICENSE"
            notice.write_bytes(b"reviewed license text")
            lock = {"version": "0.153.3", "commit": "reviewed-source"}
            manifest = {"schemaVersion": 1, "backendVersion": lock["version"],
                        "backendCommit": lock["commit"], "files": [
                            {"path": "LICENSE", "size": notice.stat().st_size,
                             "sha256": hashlib.sha256(notice.read_bytes()).hexdigest()}]}
            (root / "MANIFEST.json").write_text(json.dumps(manifest), encoding="utf-8")
            backend.verify_notices(lock, root)
            notice.write_bytes(b"incomplete license")
            with self.assertRaisesRegex(ValueError, "pinned size and SHA-256"):
                backend.verify_notices(lock, root)
            notice.unlink()
            with self.assertRaisesRegex(ValueError, "missing or unexpected"):
                backend.verify_notices(lock, root)

    def fixture(self, root, extra=None):
        target = "x86_64-unknown-linux-musl"
        archive = root / "backend.tar.gz"
        lock = {"version": "0.153.3", "packages": {target: {}}}
        metadata = json.dumps(backend.metadata(lock, target)).encode()
        with tarfile.open(archive, "w:gz") as output:
            for name in sorted(backend.required_files(target)):
                content = metadata if name == "codex-package.json" else f"executable:{name}".encode()
                item = tarfile.TarInfo(name)
                item.size = len(content)
                item.mode = 0o644 if name == "codex-package.json" else 0o755
                output.addfile(item, io.BytesIO(content))
            if extra is not None:
                output.addfile(extra)
        source = root / "source.tar.gz"
        source.write_bytes(b"pinned corresponding source")
        for path, spec in ((archive, lock["packages"][target]), (source, lock.setdefault("source", {}))):
            spec.update(size=path.stat().st_size, sha256=hashlib.sha256(path.read_bytes()).hexdigest())
        return target, archive, source, lock

    def test_staging_preserves_helpers_and_rejects_later_replacement(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            target, archive, source, lock = self.fixture(root)
            destination = root / "bundle"
            backend.stage(lock, target, archive, source, destination)
            backend.stage(lock, target, archive, source, destination)
            files = backend.verified_files(destination, archive, lock, target)
            self.assertEqual(files["codex-resources/bwrap"].read_bytes(), b"executable:codex-resources/bwrap")
            files["bin/codex-app-server"].write_bytes(b"replaced after download")
            with self.assertRaisesRegex(ValueError, "differs from its verified"):
                backend.verified_files(destination, archive, lock, target)

    def test_modified_download_never_creates_a_bundle(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            target, archive, source, lock = self.fixture(root)
            archive.write_bytes(b"unverified replacement")
            with self.assertRaisesRegex(ValueError, "pinned size and SHA-256"):
                backend.stage(lock, target, archive, source, root / "bundle")
            self.assertFalse((root / "bundle").exists())

    def test_unsafe_archive_members_and_expansion_are_rejected_before_extraction(self):
        traversal = tarfile.TarInfo("../escaped")
        link = tarfile.TarInfo("codex-resources/linked")
        link.type = tarfile.SYMTYPE
        link.linkname = "../../escaped"
        for extra in (traversal, link):
            with self.subTest(path=extra.name), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                target, archive, source, lock = self.fixture(root, extra)
                with self.assertRaises(ValueError):
                    backend.stage(lock, target, archive, source, root / "bundle")
                self.assertFalse((root / "bundle").exists())
                self.assertFalse((root / "escaped").exists())
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            target, archive, source, lock = self.fixture(root)
            with patch.object(backend, "MAX_EXPANDED_BYTES", 1), self.assertRaisesRegex(ValueError, "expanded size"):
                backend.stage(lock, target, archive, source, root / "bundle")
            self.assertFalse((root / "bundle").exists())


if __name__ == "__main__":
    unittest.main()
