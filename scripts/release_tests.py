"""Protect the publication boundary: no incomplete sets or replaced release bytes."""

import hashlib
from contextlib import chdir
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import release


class PublicationBoundaryTests(unittest.TestCase):
    def test_missing_or_extra_platform_prevents_checksum_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            names = [release.binary_name("v0.1.0", target) for target in release.TARGETS]
            names.append(release.source_archive_name())
            for name in names[:-1]:
                (directory / name).write_bytes(b"archive bytes")
            with self.assertRaisesRegex(ValueError, "missing="):
                release.release_manifest("v0.1.0", directory)
            (directory / names[-1]).write_bytes(b"last archive")
            manifest = release.release_manifest("v0.1.0", directory)
            self.assertEqual(len(manifest.splitlines()), 5)
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

    def test_new_and_interrupted_drafts_publish_without_the_published_only_tag_endpoint(self):
        for resume_existing in (False, True):
            with self.subTest(resume_existing=resume_existing), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                (root / "Cargo.toml").write_text('[package]\nname="codex-acp-v2"\nversion="1.2.3"\n')
                (root / "Cargo.lock").write_text('[[package]]\nname="codex-acp-v2"\nversion="1.2.3"\n')
                (root / "backend.lock.json").write_bytes(Path("backend.lock.json").read_bytes())
                directory = root / "dist"
                directory.mkdir()
                for target in release.TARGETS:
                    (directory / release.binary_name("v1.2.3", target)).write_bytes(target.encode())
                (directory / release.source_archive_name()).write_bytes(b"corresponding sources")
                (directory / "SHA256SUMS").write_text(release.release_manifest("v1.2.3", directory))
                draft = {"id": 42, "tag_name": "v1.2.3", "draft": True, "assets": []}
                records = [draft] if resume_existing else []
                existing_name = release.binary_name("v1.2.3", release.TARGETS[0])
                if resume_existing:
                    existing = directory / existing_name
                    draft["assets"].append({"name": existing_name, "size": existing.stat().st_size,
                                            "digest": f"sha256:{release.sha256(existing)}"})
                uploaded_names = []
                created = []

                def fake_github(*args, **_kwargs):
                    if args == ("api", "repos/org/repo/commits/v1.2.3"):
                        result = {"sha": "verified-commit"}
                    elif args == ("api", "repos/org/repo/releases?per_page=100", "--paginate", "--slurp"):
                        result = [records]
                    elif args[:4] == ("api", "repos/org/repo/releases", "--method", "POST"):
                        self.assertFalse(records, "existing drafts must not be recreated")
                        created.append(True)
                        records.append(draft)
                        result = draft
                    elif args[:3] == ("release", "upload", "v1.2.3"):
                        for filename in args[5:]:
                            artifact = Path(filename)
                            uploaded_names.append(artifact.name)
                            draft["assets"].append({"name": artifact.name, "size": artifact.stat().st_size,
                                                    "digest": f"sha256:{release.sha256(artifact)}"})
                        result = None
                    elif args == ("api", "repos/org/repo/releases/42"):
                        result = draft
                    elif args == ("api", "repos/org/repo/releases/42", "--method", "PATCH", "--field", "draft=false"):
                        draft["draft"] = False
                        result = draft
                    else:
                        raise AssertionError(f"unexpected API operation: {args}")
                    return subprocess.CompletedProcess(args, 0, json.dumps(result), "")

                with chdir(root), patch.object(release, "github", side_effect=fake_github), \
                        patch.object(release.subprocess, "check_output", return_value="verified-commit\n"):
                    release.publish("v1.2.3", "org/repo")
                self.assertFalse(draft["draft"])
                self.assertEqual(len(created), 0 if resume_existing else 1)
                expected_uploads = {path.name for path in directory.iterdir()}
                if resume_existing:
                    expected_uploads.remove(existing_name)
                self.assertEqual(set(uploaded_names), expected_uploads)


if __name__ == "__main__":
    unittest.main()
