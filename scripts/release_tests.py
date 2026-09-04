"""Protect the publication boundary: no incomplete sets or replaced release bytes."""

import hashlib
from pathlib import Path
import tempfile
import unittest

import release


class PublicationBoundaryTests(unittest.TestCase):
    def test_missing_or_extra_platform_prevents_checksum_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            names = [release.archive_name("v0.1.0", target) for target in release.TARGETS]
            for name in names[:-1]:
                (directory / name).write_bytes(b"archive bytes")
            with self.assertRaisesRegex(ValueError, "missing="):
                release.release_manifest("v0.1.0", directory)
            (directory / names[-1]).write_bytes(b"last archive")
            manifest = release.release_manifest("v0.1.0", directory)
            self.assertEqual(len(manifest.splitlines()), 4)
            for line in manifest.splitlines():
                digest, name = line.split("  ")
                self.assertEqual(digest, hashlib.sha256((directory / name).read_bytes()).hexdigest())
            (directory / "unexpected.zip").write_bytes(b"unreviewed build")
            with self.assertRaisesRegex(ValueError, "extra="):
                release.release_manifest("v0.1.0", directory)

    def test_rerun_only_accepts_identical_uploaded_assets(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "archive.zip"
            path.write_bytes(b"verified bytes")
            asset = {"name": path.name, "size": 14, "digest": f"sha256:{hashlib.sha256(b'verified bytes').hexdigest()}"}
            self.assertEqual(release.published_assets({"assets": [asset]}, {path.name: path}), set())
            path.write_bytes(b"replaced bytes")
            with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
                release.published_assets({"assets": [asset]}, {path.name: path})

    def test_tag_and_both_cargo_versions_must_agree(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "Cargo.toml").write_text('[package]\nname="codex-acp-v2"\nversion="1.2.3"\n')
            (root / "Cargo.lock").write_text('[[package]]\nname="codex-acp-v2"\nversion="1.2.3"\n')
            release.validate("v1.2.3", root)
            with self.assertRaisesRegex(ValueError, "must match"):
                release.validate("v1.2.4", root)
            (root / "Cargo.lock").write_text('[[package]]\nname="codex-acp-v2"\nversion="1.2.2"\n')
            with self.assertRaisesRegex(ValueError, "must match"):
                release.validate("v1.2.3", root)


if __name__ == "__main__":
    unittest.main()
