#!/usr/bin/env python3
"""Minimal receiver for MOYUE-DIAGNOSTICS-1 reports.

Listens on 127.0.0.1:11098 by default. Configuration comes from the
environment so the service can be started by a supervisor without exposing
credentials on the command line.
"""

import hmac
import math
import os
from pathlib import Path
import secrets
import socket
import time
import threading
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, HTTPServer

PATH = "/moyue-diagnostics"
PREFIX = b"MOYUE-DIAGNOSTICS-1\n"
MAX_REQUEST = 1 << 20
DEFAULT_MAX_TOTAL = 64 << 20
DEFAULT_TIMEOUT = 15.0
TOKEN_LIFETIME = 24 * 60 * 60
TOKEN_MIN_LENGTH = 32

BAD_REQUEST = "bad request\n"
UNAUTHORIZED = "unauthorized\n"
NOT_FOUND = "not found\n"
METHOD_NOT_ALLOWED = "method not allowed\n"
TOO_LARGE = "request too large\n"
UNSUPPORTED_MEDIA = "unsupported media type\n"
INSUFFICIENT_STORAGE = "insufficient storage\n"
STORAGE_FAILURE = "storage failure\n"
REQUEST_TIMEOUT = "request timeout\n"


@dataclass(frozen=True)
class Config:
    store: Path
    token: bytes
    max_total: int
    expires_at: float
    timeout: float = DEFAULT_TIMEOUT


def load_config(environ=None):
    """Build a Config from environment variables, raising ValueError on bad input."""
    env = os.environ if environ is None else environ
    token = env.get("MOYUE_DIAG_TOKEN", "")
    if len(token) < TOKEN_MIN_LENGTH:
        raise ValueError(
            f"MOYUE_DIAG_TOKEN must be at least {TOKEN_MIN_LENGTH} characters"
        )
    directory = env.get("MOYUE_DIAG_DIR", "")
    if not directory:
        raise ValueError("MOYUE_DIAG_DIR is required")
    try:
        max_total = int(env.get("MOYUE_DIAG_MAX_TOTAL", DEFAULT_MAX_TOTAL))
    except ValueError:
        raise ValueError("MOYUE_DIAG_MAX_TOTAL must be an integer") from None
    if max_total <= 0:
        raise ValueError("MOYUE_DIAG_MAX_TOTAL must be positive")
    expires_raw = env.get("MOYUE_DIAG_EXPIRES")
    try:
        expires_at = (
            float(expires_raw) if expires_raw else time.time() + TOKEN_LIFETIME
        )
    except ValueError:
        raise ValueError("MOYUE_DIAG_EXPIRES must be a Unix timestamp") from None
    if not math.isfinite(expires_at):
        raise ValueError("MOYUE_DIAG_EXPIRES must be a finite Unix timestamp")
    return Config(
        store=Path(directory).expanduser(),
        token=token.encode(),
        max_total=max_total,
        expires_at=expires_at,
    )


def prepare_store(path):
    """Create the private storage directory with mode 0700."""
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path, 0o700)


def _valid_report(body):
    if not body.startswith(PREFIX) or b"\x00" in body:
        return False
    try:
        body.decode("utf-8")
    except UnicodeDecodeError:
        return False
    return True


def _used_bytes(path):
    total = 0
    with os.scandir(path) as entries:
        for entry in entries:
            if not entry.name.endswith(".txt"):
                continue
            try:
                if entry.is_file(follow_symlinks=False):
                    total += entry.stat(follow_symlinks=False).st_size
            except OSError:
                continue
    return total


class DiagnosticsServer(HTTPServer):
    request_queue_size = 32

    def __init__(self, address, config):
        self.config = config
        self.store_lock = threading.Lock()
        super().__init__(address, DiagnosticsHandler)


class DiagnosticsHandler(BaseHTTPRequestHandler):
    server_version = "moyue-diagnostics"
    sys_version = ""
    protocol_version = "HTTP/1.1"

    def setup(self):
        super().setup()
        self.connection.settimeout(self.server.config.timeout)

    def log_message(self, *args):
        # The request line can contain a report ID; never log requests.
        pass

    def _respond(self, status, text):
        payload = text.encode("ascii")
        self.close_connection = True
        try:
            self.send_response(status)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(payload)
        except OSError:
            pass

    def _expired(self):
        return time.time() >= self.server.config.expires_at

    def _authorized(self):
        values = self.headers.get_all("Authorization") or []
        if len(values) != 1:
            return False
        parts = values[0].split(None, 1)
        if len(parts) != 2 or parts[0].lower() != "bearer":
            return False
        if not hmac.compare_digest(parts[1].encode(), self.server.config.token):
            return False
        return not self._expired()

    def _read_body(self, length):
        try:
            body = self.rfile.read(length)
        except socket.timeout:
            self._respond(408, REQUEST_TIMEOUT)
            return None
        except OSError:
            self._respond(400, BAD_REQUEST)
            return None
        if len(body) != length:
            self._respond(400, BAD_REQUEST)
            return None
        return body

    def _discard(self, length):
        """Drain a bounded prefix of an oversized body before closing."""
        remaining = min(length, MAX_REQUEST * 2)
        self.connection.settimeout(1.0)
        try:
            while remaining:
                chunk = self.rfile.read(min(remaining, 65536))
                if not chunk:
                    break
                remaining -= len(chunk)
        except OSError:
            pass

    def _store(self, body):
        """Return (status, text), checking quota and writing under one lock."""
        config = self.server.config
        with self.server.store_lock:
            try:
                used = _used_bytes(config.store)
            except OSError:
                return 500, STORAGE_FAILURE
            if used + len(body) > config.max_total:
                return 507, INSUFFICIENT_STORAGE
            for _ in range(16):
                report_id = secrets.token_hex(8)
                target = config.store / f"{report_id}.txt"
                try:
                    fd = os.open(
                        target,
                        os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                        0o600,
                    )
                except FileExistsError:
                    continue
                except OSError:
                    return 500, STORAGE_FAILURE
                try:
                    with os.fdopen(fd, "wb") as handle:
                        os.fchmod(handle.fileno(), 0o600)
                        handle.write(body)
                        handle.flush()
                        os.fsync(handle.fileno())
                except OSError:
                    try:
                        os.unlink(target)
                    except OSError:
                        pass
                    return 500, STORAGE_FAILURE
                return 200, report_id
        return 500, STORAGE_FAILURE

    def do_GET(self):
        self._respond(405, METHOD_NOT_ALLOWED)

    def do_POST(self):
        if self.path != PATH:
            return self._respond(404, NOT_FOUND)
        if self.headers.get("Transfer-Encoding") is not None:
            return self._respond(400, BAD_REQUEST)
        lengths = self.headers.get_all("Content-Length") or []
        if len(lengths) != 1:
            return self._respond(400, BAD_REQUEST)
        try:
            length = int(lengths[0])
        except ValueError:
            return self._respond(400, BAD_REQUEST)
        if length < 0:
            return self._respond(400, BAD_REQUEST)
        # Reject bad credentials before touching the body: an unauthenticated
        # slow client must not hold a handler or a socket for the read timeout.
        if not self._authorized():
            return self._respond(401, UNAUTHORIZED)
        if length > MAX_REQUEST:
            self._discard(length)
            return self._respond(413, TOO_LARGE)
        content_type = self.headers.get("Content-Type", "")
        if content_type.split(";", 1)[0].strip().lower() != "text/plain":
            return self._respond(415, UNSUPPORTED_MEDIA)
        body = self._read_body(length)
        if body is None:
            return
        # The credential can expire while a slow body is still arriving.
        if self._expired():
            return self._respond(401, UNAUTHORIZED)
        if not _valid_report(body):
            return self._respond(400, BAD_REQUEST)
        status, text = self._store(body)
        self._respond(status, text)


def create_server(config, host="127.0.0.1", port=11098):
    """Create a ready-to-serve server; port 0 picks an ephemeral port for tests."""
    prepare_store(config.store)
    return DiagnosticsServer((host, port), config)


def main():
    try:
        server = create_server(load_config())
    except (OSError, ValueError) as error:
        raise SystemExit(f"moyue diagnostics: {error}")
    host, port = server.server_address[:2]
    print(
        f"moyue diagnostics listening on http://{host}:{port}{PATH}",
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
