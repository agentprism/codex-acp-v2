"""Protect streamed native payload bytes, permissions and deterministic ZIP output."""

from pathlib import Path
import tempfile
import unittest
import zipfile

import archives


class ArchiveTests(unittest.TestCase):
    def test_streamed_helpers_retain_bytes_and_modes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            helper = root / "helper"
            helper.write_bytes(bytes(range(256)) * 1024)
            archive = root / "package.zip"
            files = {"codex/bin/helper": (helper, 0o755), "metadata.json": (b'{"version":1}', 0o644)}
            archives.write_archive(archive, "", files)
            with zipfile.ZipFile(archive) as content:
                self.assertEqual(content.read("codex/bin/helper"), helper.read_bytes())
                self.assertEqual(content.read("metadata.json"), b'{"version":1}')
                self.assertEqual(content.getinfo("codex/bin/helper").external_attr >> 16, 0o100755)
            original = archive.read_bytes()
            archives.write_archive(archive, "", files)
            self.assertEqual(archive.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
