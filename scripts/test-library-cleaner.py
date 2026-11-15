#!/usr/bin/env python3
"""Exercise the KUAL cleanup against synthetic files without a device."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET


EXTENSION = Path(__file__).resolve().parents[1] / "kual/library-cleaner"


class LibraryCleanerTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.store = self.root / "mnt/us"
        self.data = self.store / ".aristotle"
        self.data.mkdir(parents=True)
        self.index = self.data / "library.json"
        self.original = json.dumps({
            "version": 1,
            "root": "/mnt/us",
            "books": [{"id": "sample", "path": "logs/example.txt",
                       "progress": {"fraction": 0.5}, "annotations": ["sample"]}],
        }).encode()
        self.index.write_bytes(self.original)
        self.previous = b'{"version":1,"books":[]}'
        (self.data / "library.json.bak").write_bytes(self.previous)
        self.book = self.store / "documents/book.txt"
        self.book.parent.mkdir()
        self.book.write_text("Synthetic book\n")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.stub("pidof", "exit 1")
        self.stub("eips", "exit 0")
        self.env = dict(os.environ, AGENT_TEST_ROOT=str(self.root),
                        PATH=str(self.bin) + os.pathsep + os.environ["PATH"])

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def run_cleaner(self):
        return subprocess.run(["sh", str(EXTENSION / "clear-library.sh")],
                              env=self.env, capture_output=True, text=True)

    def backups(self):
        return list(self.store.glob(".moyue-library-backup-*"))

    def test_reset_archives_both_generations_and_is_repeatable(self):
        result = self.run_cleaner()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        archive = self.backups()[0]
        self.assertEqual((archive / "library.json").read_bytes(), self.original)
        self.assertEqual((archive / "library.json.bak").read_bytes(), self.previous)
        self.assertEqual(json.loads(self.index.read_text())["books"], [])
        self.assertEqual(self.index.read_bytes(), (self.data / "library.json.bak").read_bytes())
        self.assertEqual(self.book.read_text(), "Synthetic book\n")
        self.assertEqual(self.run_cleaner().returncode, 0)
        self.assertEqual((archive / "library.json").read_bytes(), self.original)
        self.assertEqual(len(self.backups()), 2)

    def test_running_app_refuses_before_backup(self):
        self.stub("pidof", "exit 0")
        self.assertNotEqual(self.run_cleaner().returncode, 0)
        self.assertEqual(self.index.read_bytes(), self.original)
        self.assertEqual(self.backups(), [])

    def test_backup_failure_preserves_live_index(self):
        self.stub("cp", "exit 1")
        self.assertNotEqual(self.run_cleaner().returncode, 0)
        self.assertEqual(self.index.read_bytes(), self.original)
        self.assertEqual((self.data / "library.json.bak").read_bytes(), self.previous)

    def test_failed_replace_keeps_original_archive_and_allows_retry(self):
        self.stub("mv", "exit 1")
        self.assertNotEqual(self.run_cleaner().returncode, 0)
        self.assertEqual(self.index.read_bytes(), self.original)
        self.assertEqual((self.backups()[0] / "library.json").read_bytes(), self.original)
        self.assertFalse((self.data / ".library-cleaner-lock").exists())
        (self.bin / "mv").unlink()
        self.assertEqual(self.run_cleaner().returncode, 0)

    def test_verification_failure_preserves_both_live_generations(self):
        self.stub("cmp", "exit 1")
        self.assertNotEqual(self.run_cleaner().returncode, 0)
        self.assertEqual(self.index.read_bytes(), self.original)
        self.assertEqual((self.data / "library.json.bak").read_bytes(), self.previous)

    def test_second_replace_failure_retains_original_in_archive(self):
        self.stub("mv", 'case "$2" in */library.json) exit 1;; esac\nexec /bin/mv "$@"')
        self.assertNotEqual(self.run_cleaner().returncode, 0)
        self.assertEqual(self.index.read_bytes(), self.original)
        self.assertEqual((self.backups()[0] / "library.json").read_bytes(), self.original)

    def test_existing_lock_refuses(self):
        (self.data / ".library-cleaner-lock").mkdir()
        self.assertNotEqual(self.run_cleaner().returncode, 0)
        self.assertEqual(self.index.read_bytes(), self.original)
        self.assertEqual(self.backups(), [])

    def test_symlink_index_refuses(self):
        target = self.data / "original.json"
        self.index.rename(target)
        self.index.symlink_to(target)
        self.assertNotEqual(self.run_cleaner().returncode, 0)
        self.assertEqual(target.read_bytes(), self.original)
        self.assertEqual(self.backups(), [])

    def test_missing_primary_archives_fallback(self):
        self.index.unlink()
        self.assertEqual(self.run_cleaner().returncode, 0)
        self.assertEqual((self.backups()[0] / "library.json.bak").read_bytes(), self.previous)

    def test_menu_uses_confirmation_and_existing_script(self):
        ET.parse(EXTENSION / "config.xml")
        menu = json.loads((EXTENSION / "menu.json").read_text())
        confirm = menu["items"][0]["items"][0]["items"][0]
        self.assertEqual(confirm["action"], "sh ./clear-library.sh")


if __name__ == "__main__":
    unittest.main()
