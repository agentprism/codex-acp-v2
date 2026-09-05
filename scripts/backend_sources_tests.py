"""Protect the narrow upstream release-lock repair from dependency resolution drift."""

import tomllib
import unittest

import backend_sources


class SourceLockTests(unittest.TestCase):
    def test_only_local_workspace_versions_are_normalized(self):
        text = '''version = 4
[[package]]
name = "codex-local"
version = "0.0.0"
dependencies = ["external"]
[[package]]
name = "external"
version = "0.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "locked-checksum"
'''
        expected = tomllib.loads(text)
        expected["package"][0]["version"] = "0.153.3"
        self.assertEqual(tomllib.loads(backend_sources.normalize_local_versions(text, "0.153.3")), expected)
        with self.assertRaisesRegex(ValueError, "review source normalization"):
            backend_sources.normalize_local_versions(text.replace('version = "0.0.0"', 'version = "9.9.9"', 1), "0.153.3")


if __name__ == "__main__":
    unittest.main()
