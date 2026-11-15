#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for the diagnostics entry point in the main KUAL extension.

The upload credential must come from outside the release tree
(/mnt/us/.aristotle/diagnostics), with the DIAG-001 extension-local location
still accepted for devices that installed that package.
"""
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

EXTENSION = Path(__file__).resolve().parents[2] / "device" / "extension"
SOURCE = EXTENSION / "diagnostics"
TOKEN = "b" * 48


class DiagnosticsIntegrationTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.extension = self.root / "extension"
        shutil.copytree(SOURCE, self.extension)
        self.config = self.root / "mnt/us/.aristotle/diagnostics"
        self.config.mkdir(parents=True)
        (self.config / "endpoint.txt").write_text("https://example.test/moyue-diagnostics\n")
        (self.config / "token.txt").write_text(TOKEN + "\n")
        self.out = self.root / "mnt/us/moyue-diagnostics"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.stub("eips", "exit 0")
        self.env = dict(
            os.environ,
            AGENT_TEST_ROOT=str(self.root),
            PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
        )

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def run_action(self, action):
        return subprocess.run(
            ["sh", str(self.extension / "diagnostics.sh"), action],
            env=self.env,
            capture_output=True,
            text=True,
        )

    def collect(self):
        result = self.run_action("collect")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_upload_uses_out_of_tree_config(self):
        self.collect()
        self.stub(
            "curl",
            'input=$(cat)\n'
            'case "$input" in *"Authorization: Bearer ' + TOKEN + '"*) ;; *) exit 9 ;; esac\n'
            'while [ "$#" -gt 0 ]; do\n'
            '  if [ "$1" = --output ]; then shift; output=$1; fi\n'
            '  shift\n'
            'done\n'
            'printf 0123456789abcdef > "$output"',
        )
        result = self.run_action("upload")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.out / "receipt.txt").read_text().strip(), "0123456789abcdef")

    def test_missing_config_is_a_clear_error(self):
        self.collect()
        (self.config / "token.txt").unlink()
        result = self.run_action("upload")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("upload configuration missing", result.stdout)
        self.assertTrue((self.out / "report.txt").exists())

    def test_legacy_extension_config_still_works(self):
        shutil.rmtree(self.config)
        (self.extension / "endpoint.txt").write_text("https://example.test/moyue-diagnostics\n")
        (self.extension / "token.txt").write_text(TOKEN + "\n")
        self.collect()
        self.stub(
            "curl",
            'while [ "$#" -gt 0 ]; do\n'
            '  if [ "$1" = --output ]; then shift; output=$1; fi\n'
            '  shift\n'
            'done\n'
            'printf 0123456789abcdef > "$output"',
        )
        result = self.run_action("upload")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_menu_keeps_confirmation_and_includes_ota(self):
        menu = json.loads((EXTENSION / "menu.json").read_text())
        items = menu["items"][0]["items"]
        names = [item["name"] for item in items]
        self.assertIn("检查更新（OTA，需 Wi-Fi）", names)
        self.assertIn("诊断日志", names)
        diagnostics = next(item for item in items if item["name"] == "诊断日志")
        upload = next(item for item in diagnostics["items"] if item["name"].startswith("2."))
        choices = [item["name"] for item in upload["items"]]
        self.assertEqual(choices, ["同意发送到开发者服务器", "取消"])


if __name__ == "__main__":
    unittest.main()
