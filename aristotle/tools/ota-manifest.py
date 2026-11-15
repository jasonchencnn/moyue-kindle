#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the strict MOYUE-OTA-1 manifest for a KO2 preview update.

usage: tools/ota-manifest.py --package FILE --source FILE --version X.Y.Z \
           --out FILE [--base-url URL] [--product NAME] [--channel NAME] \
           [--device NAME] [--firmware PREFIX]

The manifest is a line-oriented text file, not JSON: the Kindle updater has
BusyBox sh, curl and tar, and no JSON parser.  Every field is required exactly
once; the device parser rejects unknown, duplicate or malformed fields.

Package publishing:

  * the OTA package is `tools/package.sh ota` (a tar.gz, because BusyBox has
    tar and not necessarily unzip);
  * the corresponding-source tarball is `tools/package.sh source`; AGPL-3.0
    section 6 requires publishing it with the binary;
  * both files and the manifest go under the same HTTPS path, normally
    https://me.chen-cn.top/moyue/ota/ko2/.

The updater downloads this manifest and the package over HTTPS and checks the
SHA-256 values.  No secret or signing key is stored in the repository; the
manifest's authenticity comes from the HTTPS origin and the digest binding.
"""
from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path
from urllib.parse import urlsplit

HEADER = "MOYUE-OTA-1"
DEFAULT_BASE = "https://me.chen-cn.top/moyue/ota/ko2/"
FIELDS = (
    "product",
    "channel",
    "version",
    "device",
    "firmware",
    "package",
    "sha256",
    "size",
    "source",
    "source-sha256",
    "source-size",
)
VERSION_RE = re.compile(r"^(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})$")
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def _url(value: str) -> str:
    parts = urlsplit(value)
    if parts.scheme != "https" or not parts.netloc:
        raise ValueError("must be an https:// URL")
    if parts.hostname != "me.chen-cn.top":
        raise ValueError("host must be me.chen-cn.top")
    if parts.username or parts.password or parts.port:
        raise ValueError("credentials and ports are not allowed")
    if not parts.path.startswith("/moyue/ota/ko2/") or not value.endswith("/"):
        raise ValueError("must be under https://me.chen-cn.top/moyue/ota/ko2/")
    return value


def _digest(path: Path) -> tuple[str, int]:
    h = hashlib.sha256()
    size = 0
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
            size += len(chunk)
    return h.hexdigest(), size


def build_manifest(
    *,
    package: Path,
    source: Path,
    version: str,
    base_url: str = DEFAULT_BASE,
    product: str = "moyue-preview",
    channel: str = "ko2-preview",
    device: str = "oasis2",
    firmware: str = "5.16",
    package_name: str | None = None,
    source_name: str | None = None,
) -> str:
    """Return the manifest bytes as text for the given local files."""
    if not VERSION_RE.match(version):
        raise ValueError(f"invalid version {version!r}; expected X.Y.Z")
    base = _url(base_url)
    for path in (package, source):
        if not path.is_file():
            raise ValueError(f"not a file: {path}")
    pkg_name = package_name or package.name
    src_name = source_name or source.name
    for name in (pkg_name, src_name):
        if not NAME_RE.match(name):
            raise ValueError(f"unsafe file name {name!r}")
    pkg_digest, pkg_size = _digest(package)
    src_digest, src_size = _digest(source)
    if pkg_size <= 0 or src_size <= 0:
        raise ValueError("package and source must not be empty")
    values = {
        "product": product,
        "channel": channel,
        "version": version,
        "device": device,
        "firmware": firmware,
        "package": base + pkg_name,
        "sha256": pkg_digest,
        "size": str(pkg_size),
        "source": base + src_name,
        "source-sha256": src_digest,
        "source-size": str(src_size),
    }
    for key, value in values.items():
        if not re.match(r"^[A-Za-z0-9._:/@%+,=&?-]+$", value):
            raise ValueError(f"invalid value for {key}: {value!r}")
    lines = [HEADER] + [f"{key}={values[key]}" for key in FIELDS]
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--package", required=True, type=Path, help="OTA tar.gz from tools/package.sh ota")
    parser.add_argument("--source", required=True, type=Path, help="source tar.gz from tools/package.sh source")
    parser.add_argument("--version", required=True, help="release version, X.Y.Z")
    parser.add_argument("--out", required=True, type=Path, help="manifest output path")
    parser.add_argument("--base-url", default=DEFAULT_BASE)
    parser.add_argument("--product", default="moyue-preview")
    parser.add_argument("--channel", default="ko2-preview")
    parser.add_argument("--device", default="oasis2")
    parser.add_argument("--firmware", default="5.16")
    parser.add_argument("--package-name", help="uploaded package file name (default: local basename)")
    parser.add_argument("--source-name", help="uploaded source file name (default: local basename)")
    args = parser.parse_args(argv)
    try:
        text = build_manifest(
            package=args.package,
            source=args.source,
            version=args.version,
            base_url=args.base_url,
            product=args.product,
            channel=args.channel,
            device=args.device,
            firmware=args.firmware,
            package_name=args.package_name,
            source_name=args.source_name,
        )
    except ValueError as exc:
        print(f"ota-manifest: {exc}", file=sys.stderr)
        return 2
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8")
    print(f"wrote {args.out} ({len(text)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
