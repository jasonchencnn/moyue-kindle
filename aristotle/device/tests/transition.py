#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Exercise the real takeover and manual launcher against an isolated device root."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

DEVICE = Path(__file__).resolve().parents[1]


class TransitionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="transition-", dir=os.environ.get("TMPDIR"))
        self.root = Path(self.temp.name)
        self.app = self.root / "mnt/us/aristotle"
        self.app.mkdir(parents=True)
        (self.root / "proc").mkdir()
        (self.root / "proc/meminfo").write_text("MemTotal: 512000 kB\nMemFree: 128000 kB\n")
        (self.root / "proc/mounts").touch()
        (self.root / "var/run").mkdir(parents=True)
        (self.root / "mnt/us/.aristotle").mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for command in ["initctl", "start", "stop", "lipc-set-prop"]:
            self.script(self.bin / command, '''
name=${0##*/}
echo "$name $*" >> "$AGENT_TEST_ROOT/calls"
[ "$name" != initctl ] || echo "$2 stop/waiting"
''')
        timeout = shutil.which("gtimeout") or shutil.which("timeout")
        self.assertIsNotNone(timeout, "GNU timeout is needed for this host-side test")
        self.script(self.bin / "timeout", f'exec "{timeout}" -s "$4" "$2" "${{@:5}}"', shell="/bin/bash")
        for name in ["takeover.sh", "run-aristotle.sh"]:
            shutil.copy2(DEVICE / "app" / name, self.app / name)
        self.script(self.app / "aristotle", '''
if [ "${1:-}" = --transition ]; then
    echo "transition $2" >> "$AGENT_TEST_ROOT/calls"
    if [ "$2" = watch ]; then
        mkdir -p "$AGENT_TEST_ROOT/proc/$$"
        printf '%s\\000--transition\\000watch\\000' "$0" > "$AGENT_TEST_ROOT/proc/$$/cmdline"
        trap 'exit 0' TERM INT
        while [ "$(head -n 1 "$AGENT_TEST_ROOT/var/run/aristotle-transition.pid" 2>/dev/null)" = "$$" ]; do sleep 0.1; done
        exit 0
    fi
    [ "${DRAW_HANG:-0}" != 1 ] || sleep 30
    exit "${DRAW_RC:-0}"
fi
echo app >> "$AGENT_TEST_ROOT/calls"
exit "${APP_RC:-0}"
''')
        self.env = dict(os.environ, AGENT_TEST_ROOT=str(self.root), ARISTOTLE_FORCE="1",
                        ARISTOTLE_LOG=str(self.root / "app.log"), PATH=f"{self.bin}:{os.environ['PATH']}")

    @staticmethod
    def script(path, body, shell="/bin/sh"):
        path.write_text(f"#!{shell}\n{body}\n")
        path.chmod(0o755)

    def run_takeover(self, command, **changes):
        return subprocess.run(["sh", str(self.app / "takeover.sh"), command],
                              env=dict(self.env, **changes), capture_output=True, text=True, timeout=15)

    def tearDown(self):
        # Cancellation runs through the production identity check.
        self.run_takeover("start")
        self.temp.cleanup()

    def calls(self):
        return (self.root / "calls").read_text().splitlines()

    def test_manual_launch_orders_prepare_app_and_restore_before_stock_start(self):
        result = subprocess.run(["sh", str(self.app / "run-aristotle.sh")],
                                env=self.env, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertLess(calls.index("transition prepare"), calls.index("app"))
        self.assertLess(calls.index("app"), calls.index("transition restoring"))
        self.assertLess(calls.index("transition restoring"), calls.index("start lab126_gui"))

    def test_app_failure_keeps_its_exit_status_and_restores_stock(self):
        result = subprocess.run(["sh", str(self.app / "run-aristotle.sh")],
                                env=dict(self.env, APP_RC="7"), capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 7)
        self.assertIn("start lab126_gui", self.calls())

    def test_failed_or_hung_renderer_still_restores_stock(self):
        self.assertEqual(self.run_takeover("start", DRAW_RC="1").returncode, 0)
        self.assertEqual(self.run_takeover("stop", DRAW_HANG="1").returncode, 0)
        self.assertIn("start lab126_gui", self.calls())

    def test_repeated_stop_does_not_repaint_active_stock(self):
        self.run_takeover("start")
        self.run_takeover("stop")
        before = self.calls().count("transition restoring")
        self.run_takeover("stop")
        self.assertEqual(self.calls().count("transition restoring"), before)
        self.assertFalse((self.root / "var/run/aristotle-transition.pid").exists())

    def test_dead_or_unrelated_observer_pid_is_discarded(self):
        marker = self.root / "var/run/aristotle-transition.pid"
        for pid in [99999999, os.getpid()]:
            marker.write_text(f"{pid}\n{self.app / 'aristotle'}\n")
            self.assertEqual(self.run_takeover("start").returncode, 0)
            self.assertFalse(marker.exists())
            os.kill(os.getpid(), 0)

    def test_new_launch_cancels_the_matching_old_observer(self):
        self.run_takeover("start")
        self.run_takeover("stop")
        marker = self.root / "var/run/aristotle-transition.pid"
        observer = int(marker.read_text().splitlines()[0])
        for _ in range(50):
            if (self.root / f"proc/{observer}/cmdline").exists():
                break
            time.sleep(0.02)
        self.run_takeover("start")
        self.assertFalse(marker.exists())
        status = subprocess.run(["ps", "-p", str(observer), "-o", "stat="], capture_output=True, text=True)
        self.assertTrue(status.returncode != 0 or "Z" in status.stdout, status.stdout)


if __name__ == "__main__":
    unittest.main()
