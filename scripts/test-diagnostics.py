#!/usr/bin/env python3
"""Synthetic host tests for scripts/diagnostics-server.py."""

from collections import Counter
import http.client
import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import threading
import time
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "diagnostics_server", HERE / "diagnostics-server.py"
)
diagnostics = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(diagnostics)

PATH = diagnostics.PATH
TOKEN = "synthetic-diagnostics-token-0123456789"
REPORT = diagnostics.PREFIX + b"synthetic diagnostics line\n"


class FakeClock:
    """Controllable replacement for time.time in expiry tests."""

    def __init__(self, now):
        self.now = now

    def time(self):
        return self.now


class ReceiverTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.store = Path(self.scratch.name) / "store"
        self.server = self.start()

    def start(self, max_total=1 << 20, expires_at=None, timeout=None, token=TOKEN):
        config = diagnostics.Config(
            store=self.store,
            token=token.encode(),
            max_total=max_total,
            expires_at=time.time() + 3600 if expires_at is None else expires_at,
            timeout=diagnostics.DEFAULT_TIMEOUT if timeout is None else timeout,
        )
        server = diagnostics.create_server(config, port=0)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()

        def stop():
            server.shutdown()
            thread.join(timeout=5)
            server.server_close()

        self.addCleanup(stop)
        return server

    def connect(self):
        host, port = self.server.server_address[:2]
        return http.client.HTTPConnection(host, port, timeout=5)

    def read(self, connection):
        response = connection.getresponse()
        data = response.read()
        connection.close()
        return response.status, data

    def post(self, body=REPORT, token=TOKEN, content_type="text/plain"):
        headers = {"Content-Type": content_type}
        if token is not None:
            headers["Authorization"] = f"Bearer {token}"
        connection = self.connect()
        connection.request("POST", PATH, body=body, headers=headers)
        return self.read(connection)

    def raw(self, headers, body=b"", method="POST", path=PATH):
        connection = self.connect()
        connection.putrequest(method, path, skip_accept_encoding=True)
        for name, value in headers:
            connection.putheader(name, value)
        connection.endheaders()
        if body:
            connection.send(body)
        return self.read(connection)

    def reports(self):
        return list(self.store.glob("*.txt"))

    def send_headers_only(self, headers):
        """Announce a body but send none; return (status, data, seconds)."""
        connection = self.connect()
        connection.putrequest("POST", PATH, skip_accept_encoding=True)
        for name, value in headers:
            connection.putheader(name, value)
        connection.endheaders()
        started = time.monotonic()
        status, data = self.read(connection)
        return status, data, time.monotonic() - started

    def test_valid_report_stored_with_private_mode(self):
        status, data = self.post()
        self.assertEqual(status, 200)
        report_id = data.decode()
        self.assertRegex(report_id, r"^[0-9a-f]{16}$")
        files = self.reports()
        self.assertEqual([path.name for path in files], [report_id + ".txt"])
        self.assertEqual(files[0].read_bytes(), REPORT)
        self.assertEqual(stat.S_IMODE(files[0].stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(self.store.stat().st_mode), 0o700)

    def test_missing_and_wrong_tokens_rejected(self):
        for token in (None, "x" * len(TOKEN), TOKEN + "x"):
            status, data = self.post(token=token)
            self.assertEqual((status, data), (401, b"unauthorized\n"))
        status, data = self.raw(
            [
                ("Content-Type", "text/plain"),
                ("Authorization", "Basic " + TOKEN),
                ("Content-Length", str(len(REPORT))),
            ],
            body=REPORT,
        )
        self.assertEqual((status, data), (401, b"unauthorized\n"))
        self.assertEqual(self.reports(), [])

    def test_expired_token_rejected(self):
        self.server = self.start(expires_at=time.time() - 1)
        status, data = self.post()
        self.assertEqual((status, data), (401, b"unauthorized\n"))
        self.assertEqual(self.reports(), [])

    def test_unauthorized_rejected_before_body_read(self):
        for length in (4096, diagnostics.MAX_REQUEST + 1):
            for token in (None, TOKEN + "x"):
                with self.subTest(length=length, token=token):
                    headers = [("Content-Type", "text/plain")]
                    if token is not None:
                        headers.append(("Authorization", f"Bearer {token}"))
                    headers.append(("Content-Length", str(length)))
                    status, data, elapsed = self.send_headers_only(headers)
                    self.assertEqual((status, data), (401, b"unauthorized\n"))
                    # A body read or oversized-body drain would take seconds.
                    self.assertLess(elapsed, 2.0)
        self.assertEqual(self.reports(), [])

    def test_expired_token_rejected_before_body_read(self):
        self.server = self.start(expires_at=time.time() - 1)
        status, data, elapsed = self.send_headers_only(
            [
                ("Content-Type", "text/plain"),
                ("Authorization", f"Bearer {TOKEN}"),
                ("Content-Length", "4096"),
            ]
        )
        self.assertEqual((status, data), (401, b"unauthorized\n"))
        self.assertLess(elapsed, 2.0)
        self.assertEqual(self.reports(), [])

    def test_duplicate_authorization_rejected(self):
        status, data = self.raw(
            [
                ("Content-Type", "text/plain"),
                ("Authorization", f"Bearer {TOKEN}"),
                ("Authorization", f"Bearer {TOKEN}"),
                ("Content-Length", str(len(REPORT))),
            ],
            body=REPORT,
        )
        self.assertEqual((status, data), (401, b"unauthorized\n"))
        self.assertEqual(self.reports(), [])

    def test_expiry_rechecked_after_body_read(self):
        clock = FakeClock(time.time() + 60)
        self.server = self.start(expires_at=clock.now + 10)
        authenticated = threading.Event()
        original = diagnostics.DiagnosticsHandler._authorized

        def record(handler):
            allowed = original(handler)
            if allowed:
                authenticated.set()
            return allowed

        with mock.patch.object(diagnostics.time, "time", clock.time), \
                mock.patch.object(
                    diagnostics.DiagnosticsHandler, "_authorized", record
                ):
            connection = self.connect()
            connection.putrequest("POST", PATH, skip_accept_encoding=True)
            connection.putheader("Content-Type", "text/plain")
            connection.putheader("Authorization", f"Bearer {TOKEN}")
            connection.putheader("Content-Length", str(len(REPORT)))
            connection.endheaders()
            connection.send(REPORT[:1])
            self.assertTrue(
                authenticated.wait(5), "the first authentication check did not pass"
            )
            clock.now += 11
            connection.send(REPORT[1:])
            status, data = self.read(connection)
        self.assertEqual((status, data), (401, b"unauthorized\n"))
        self.assertEqual(self.reports(), [])

    def test_success_flushes_and_fsyncs_before_acknowledging(self):
        synced = []
        real_fsync = os.fsync

        def record(fd):
            synced.append(fd)
            real_fsync(fd)

        with mock.patch.object(diagnostics.os, "fsync", record):
            status, data = self.post()
        self.assertEqual(status, 200)
        self.assertEqual(len(synced), 1)
        readback = self.store / f"{data.decode()}.txt"
        self.assertEqual(readback.read_bytes(), REPORT)

    def test_fsync_failure_reports_500_and_removes_file(self):
        with mock.patch.object(
            diagnostics.os, "fsync", side_effect=OSError("synthetic fsync failure")
        ):
            status, data = self.post()
        self.assertEqual((status, data), (500, b"storage failure\n"))
        self.assertEqual(self.reports(), [])

    def test_slow_body_times_out(self):
        self.server = self.start(timeout=0.2)
        status, data = self.raw(
            [
                ("Content-Type", "text/plain"),
                ("Authorization", f"Bearer {TOKEN}"),
                ("Content-Length", "10"),
            ],
            body=b"M",
        )
        self.assertEqual((status, data), (408, b"request timeout\n"))
        self.assertEqual(self.reports(), [])

    def test_length_and_transfer_encoding_rejected(self):
        base = [
            ("Content-Type", "text/plain"),
            ("Authorization", f"Bearer {TOKEN}"),
        ]
        cases = [
            (base, 400),
            (base + [("Content-Length", "5"), ("Content-Length", "5")], 400),
            (base + [("Transfer-Encoding", "chunked")], 400),
            (base + [("Content-Length", "-1")], 400),
            (base + [("Content-Length", "ten")], 400),
            (base + [("Content-Length", str(diagnostics.MAX_REQUEST + 1))], 413),
        ]
        for headers, expected in cases:
            with self.subTest(headers=headers):
                status, _ = self.raw(headers)
                self.assertEqual(status, expected)
        status, data = self.post(body=b"x" * (diagnostics.MAX_REQUEST + 1))
        self.assertEqual((status, data), (413, b"request too large\n"))
        self.assertEqual(self.reports(), [])

    def test_wrong_path_rejected(self):
        status, data = self.raw(
            [
                ("Content-Type", "text/plain"),
                ("Authorization", f"Bearer {TOKEN}"),
                ("Content-Length", str(len(REPORT))),
            ],
            body=REPORT,
            path="/moyue-diagnostics/other",
        )
        self.assertEqual((status, data), (404, b"not found\n"))
        self.assertEqual(self.reports(), [])

    def test_content_type_rejected(self):
        for content_type in ("application/octet-stream", "text/html", ""):
            status, data = self.post(content_type=content_type)
            self.assertEqual((status, data), (415, b"unsupported media type\n"))
        self.assertEqual(self.reports(), [])

    def test_body_format_and_utf8_rejected(self):
        bad_bodies = (
            b"",
            b"MOYUE-DIAGNOSTICS-2\nsynthetic\n",
            diagnostics.PREFIX + b"\xff\n",
            diagnostics.PREFIX + b"synthetic\x00line\n",
        )
        for body in bad_bodies:
            with self.subTest(body=body):
                status, data = self.post(body=body)
                self.assertEqual((status, data), (400, b"bad request\n"))
        self.assertEqual(self.reports(), [])

    def test_capacity_limit(self):
        self.server = self.start(max_total=len(REPORT) * 2)
        self.assertEqual(self.post()[0], 200)
        self.assertEqual(self.post()[0], 200)
        status, data = self.post()
        self.assertEqual((status, data), (507, b"insufficient storage\n"))
        files = self.reports()
        self.assertEqual(len(files), 2)
        self.assertEqual(sum(path.stat().st_size for path in files), len(REPORT) * 2)

    @unittest.skipIf(
        hasattr(os, "geteuid") and os.geteuid() == 0,
        "root bypasses directory permissions",
    )
    def test_write_failure_reports_500_and_leaves_no_file(self):
        os.chmod(self.store, 0o500)
        self.addCleanup(os.chmod, self.store, 0o700)
        status, data = self.post()
        self.assertEqual((status, data), (500, b"storage failure\n"))
        self.assertEqual(self.reports(), [])

    def test_concurrent_writes_respect_capacity(self):
        self.server = self.start(max_total=len(REPORT) * 3)
        statuses = []
        lock = threading.Lock()

        def send():
            status = self.post()[0]
            with lock:
                statuses.append(status)

        threads = [threading.Thread(target=send) for _ in range(12)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=10)
        self.assertEqual(Counter(statuses), Counter({200: 3, 507: 9}))
        files = self.reports()
        self.assertEqual(len(files), 3)
        self.assertEqual(sum(path.stat().st_size for path in files), len(REPORT) * 3)

    def test_get_and_report_paths_do_not_read_data(self):
        status, data = self.post()
        self.assertEqual(status, 200)
        report_id = data.decode()
        for path in (PATH, f"/{report_id}.txt"):
            status, data = self.raw([], method="GET", path=path)
            self.assertEqual((status, data), (405, b"method not allowed\n"))
        files = self.reports()
        self.assertEqual([path.name for path in files], [report_id + ".txt"])
        self.assertEqual(files[0].read_bytes(), REPORT)

    def test_config_defaults_and_validation(self):
        env = {"MOYUE_DIAG_TOKEN": TOKEN, "MOYUE_DIAG_DIR": str(self.store)}
        before = time.time()
        config = diagnostics.load_config(env)
        self.assertEqual(config.max_total, 64 << 20)
        self.assertAlmostEqual(config.expires_at - before, 24 * 3600, delta=2)
        self.assertEqual(
            diagnostics.load_config(
                dict(env, MOYUE_DIAG_EXPIRES="1234567890")
            ).expires_at,
            1234567890,
        )
        with self.assertRaises(ValueError):
            diagnostics.load_config({"MOYUE_DIAG_TOKEN": "short",
                                     "MOYUE_DIAG_DIR": str(self.store)})
        with self.assertRaises(ValueError):
            diagnostics.load_config({"MOYUE_DIAG_TOKEN": TOKEN})
        with self.assertRaises(ValueError):
            diagnostics.load_config(
                dict(env, MOYUE_DIAG_MAX_TOTAL="not-a-number")
            )
        for expires in ("nan", "inf", "-inf", "Infinity", "-Infinity"):
            with self.subTest(expires=expires):
                with self.assertRaises(ValueError):
                    diagnostics.load_config(dict(env, MOYUE_DIAG_EXPIRES=expires))


if __name__ == "__main__":
    unittest.main()
