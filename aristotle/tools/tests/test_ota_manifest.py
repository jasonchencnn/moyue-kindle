#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for tools/ota-manifest.py (python3 -m unittest discover tools/tests)."""
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path

TOOL = Path(__file__).resolve().parents[1] / "ota-manifest.py"
spec = importlib.util.spec_from_file_location("ota_manifest", TOOL)
assert spec and spec.loader
ota = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ota)

BASE = "https://me.chen-cn.top/moyue/ota/ko2/"


class ManifestTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.package = self.dir / "moyue-preview-0.3.1-ota.tar.gz"
        self.source = self.dir / "moyue-preview-0.3.1-source.tar.gz"
        self.package.write_bytes(b"package bytes")
        self.source.write_bytes(b"source bytes")

    def tearDown(self):
        self.tmp.cleanup()

    def build(self, **kw):
        args = dict(package=self.package, source=self.source, version="0.3.1", base_url=BASE)
        args.update(kw)
        return ota.build_manifest(**args)

    def test_fields_and_digests(self):
        text = self.build()
        lines = text.splitlines()
        self.assertEqual(lines[0], ota.HEADER)
        fields = [line.split("=", 1)[0] for line in lines[1:]]
        self.assertEqual(fields, list(ota.FIELDS))
        values = dict(line.split("=", 1) for line in lines[1:])
        self.assertEqual(values["product"], "moyue-preview")
        self.assertEqual(values["channel"], "ko2-preview")
        self.assertEqual(values["device"], "oasis2")
        self.assertEqual(values["firmware"], "5.16")
        self.assertEqual(values["package"], BASE + self.package.name)
        self.assertEqual(values["source"], BASE + self.source.name)
        self.assertEqual(values["sha256"], hashlib.sha256(b"package bytes").hexdigest())
        self.assertEqual(values["source-sha256"], hashlib.sha256(b"source bytes").hexdigest())
        self.assertEqual(values["size"], str(len(b"package bytes")))
        self.assertEqual(values["source-size"], str(len(b"source bytes")))
        self.assertTrue(text.endswith("\n"))

    def test_rejects_bad_version(self):
        for version in ("0.3", "0.3.1.1", "v0.3.1", "01.3.1", "0.3.1 ", "0.3.-1"):
            with self.assertRaises(ValueError):
                self.build(version=version)

    def test_rejects_non_https_and_other_hosts(self):
        for base in (
            "http://me.chen-cn.top/moyue/ota/ko2/",
            "https://example.com/moyue/ota/ko2/",
            "https://me.chen-cn.top/other/",
            "https://user:pass@me.chen-cn.top/moyue/ota/ko2/",
            "https://me.chen-cn.top:8443/moyue/ota/ko2/",
            "https://me.chen-cn.top/moyue/ota/ko2",
        ):
            with self.assertRaises(ValueError):
                self.build(base_url=base)

    def test_rejects_unsafe_names(self):
        for name in ("../x.tar.gz", "a/b.tar.gz", "a b.tar.gz", ".hidden"):
            with self.assertRaises(ValueError):
                self.build(package_name=name)

    def test_rejects_missing_files(self):
        with self.assertRaises(ValueError):
            self.build(package=self.dir / "missing")

    def test_custom_names_are_used(self):
        text = self.build(package_name="release.tar.gz", source_name="source.tar.gz")
        values = dict(line.split("=", 1) for line in text.splitlines()[1:])
        self.assertEqual(values["package"], BASE + "release.tar.gz")
        self.assertEqual(values["source"], BASE + "source.tar.gz")


if __name__ == "__main__":
    unittest.main()
