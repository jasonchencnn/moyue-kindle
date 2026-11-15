#!/usr/bin/env python3
"""Run the KUAL entry point against synthetic device files and a fake curl."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

SOURCE = Path(__file__).resolve().parents[1] / 'kual/diagnostics'


class CollectorTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.extension = self.root / 'extension'
        shutil.copytree(SOURCE, self.extension)
        self.data = self.root / 'mnt/us/.aristotle'
        self.data.mkdir(parents=True)
        self.out = self.root / 'mnt/us/moyue-diagnostics'
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.stub('eips', 'exit 0')
        (self.data / 'app.log').write_text('open failed: sample.epub\nAuthorization: Bearer secret\npassword=hidden\n')
        (self.data / 'accounts.json').write_text('NEVER_COPY_ACCOUNT')
        (self.data / 'library.json').write_text('NEVER_COPY_LIBRARY')
        self.env = dict(os.environ, AGENT_TEST_ROOT=str(self.root),
                        PATH=str(self.bin) + os.pathsep + os.environ['PATH'])
        (self.extension / 'endpoint.txt').write_text('https://example.test/moyue-diagnostics\n')
        (self.extension / 'token.txt').write_text('a' * 48 + '\n')

    def stub(self, name, body):
        path = self.bin / name
        path.write_text('#!/bin/sh\n' + body + '\n')
        path.chmod(0o755)

    def run_action(self, action):
        return subprocess.run(['sh', str(self.extension / 'diagnostics.sh'), action],
                              env=self.env, capture_output=True, text=True)

    def collect(self):
        result = self.run_action('collect')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return (self.out / 'report.txt').read_text()

    def test_collection_is_bounded_and_uses_allowlist(self):
        (self.data / 'app.log.1').write_text('x' * 300000)
        report = self.collect()
        self.assertTrue(report.startswith('MOYUE-DIAGNOSTICS-1\n'))
        self.assertIn('open failed: sample.epub', report)
        self.assertIn('[missing or symlink]', report)
        for secret in ('Bearer secret', 'password=hidden', 'NEVER_COPY_ACCOUNT', 'NEVER_COPY_LIBRARY'):
            self.assertNotIn(secret, report)
        self.assertLess(len(report.encode()), 600000)
        self.assertFalse(self.out.joinpath('.lock').exists())
        self.assertEqual(self.out.joinpath('report.txt').stat().st_mode & 0o777, 0o600)

    def test_skips_log_symlink(self):
        (self.data / 'app.log').unlink()
        (self.data / 'app.log').symlink_to(self.data / 'accounts.json')
        self.assertNotIn('NEVER_COPY_ACCOUNT', self.collect())

    def test_upload_requires_collection_and_https(self):
        self.assertNotEqual(self.run_action('upload').returncode, 0)
        self.collect()
        (self.extension / 'endpoint.txt').write_text('http://example.test/upload')
        self.assertNotEqual(self.run_action('upload').returncode, 0)

    def test_upload_failure_keeps_report(self):
        self.collect()
        before = self.out.joinpath('report.txt').read_bytes()
        self.stub('curl', 'cat >/dev/null\nexit 22')
        self.assertNotEqual(self.run_action('upload').returncode, 0)
        self.assertEqual(before, self.out.joinpath('report.txt').read_bytes())
        self.assertFalse(self.out.joinpath('receipt.txt').exists())

    def test_upload_receipt_and_invalid_response(self):
        self.collect()
        # The curl stub verifies that the credential is delivered over stdin.
        body = '''input=$(cat)
case "$input" in *'Authorization: Bearer '*) ;; *) exit 1 ;; esac
for arg in "$@"; do case "$arg" in *aaaa*) exit 1 ;; esac; done
while [ "$#" -gt 0 ]; do
    if [ "$1" = --output ]; then shift; output=$1; fi
    shift
done
printf '%s' '0123456789abcdef' > "$output"'''
        self.stub('curl', body)
        result = self.run_action('upload')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.out.joinpath('receipt.txt').read_text().strip(), '0123456789abcdef')
        self.stub('curl', body.replace('0123456789abcdef', '<html>oops</html>'))
        self.assertNotEqual(self.run_action('upload').returncode, 0)

    def test_clipped_utf8_and_atomic_receipt(self):
        (self.data / 'app.log.1').write_text('前' * 50000 + '\nopen failed: 测试.txt\n')
        self.assertIn('open failed: 测试.txt', self.collect())
        target = self.root / 'protected'
        target.write_text('keep')
        (self.out / 'receipt.txt').symlink_to(target)
        self.stub('curl', 'while [ "$#" -gt 0 ]; do\n'
                  'if [ "$1" = --output ]; then shift; output=$1; fi\nshift\ndone\n'
                  'printf 0123456789abcdef > "$output"')
        self.assertEqual(self.run_action('upload').returncode, 0)
        self.assertEqual(target.read_text(), 'keep')
        self.assertFalse((self.out / 'receipt.txt').is_symlink())

    def test_all_sources_at_limit_and_live_log(self):
        paths = ('etc/version.txt', 'mnt/us/aristotle/SOURCE.txt',
                 'var/tmp/aristotle.log', 'mnt/us/.aristotle/app.log',
                 'mnt/us/.aristotle/app.log.1')
        for relative in paths:
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(('synthetic log line\n' * 20000) + 'last error\n')
        report = self.collect()
        self.assertIn('--- /var/tmp/aristotle.log ---', report)
        self.assertEqual(report.count('last error'), 5)
        self.assertLess(len(report.encode()), 1 << 20)

    def test_lock_lives_in_volatile_directory(self):
        lock = self.root / 'var/tmp/moyue-diagnostics.lock'
        lock.mkdir(parents=True)
        self.assertNotEqual(self.run_action('collect').returncode, 0)
        lock.rmdir()
        self.collect()
        self.assertFalse(lock.exists())

    def test_kual_manifests(self):
        ET.parse(SOURCE / 'config.xml')
        menu = json.loads((SOURCE / 'menu.json').read_text())
        self.assertEqual(len(menu['items'][0]['items']), 2)


if __name__ == '__main__':
    unittest.main()
