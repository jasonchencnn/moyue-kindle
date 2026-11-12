# SPDX-License-Identifier: AGPL-3.0-or-later
# Host settings shared by the scripts in tools/. Source it:
#
#   . tools/env.sh
#
# ARISTOTLE_CACHE is the one directory for everything large or local that stays
# out of git: build directories, the C libraries, downloaded tools, the
# emulator, screenshots, probe output and the dev-loop SSH key
# ($ARISTOTLE_CACHE/ssh/kindle_ed25519). Default:
# ${XDG_CACHE_HOME:-$HOME/.cache}/aristotle. Set it before sourcing to put the
# cache elsewhere.
#
# TMPDIR goes there too: the C build, the emulator and the tests write large
# temporary files, and /tmp is a small RAM disk on many hosts.

: "${ARISTOTLE_CACHE:=${XDG_CACHE_HOME:-$HOME/.cache}/aristotle}"
export ARISTOTLE_CACHE
export TMPDIR="$ARISTOTLE_CACHE/tmp"
mkdir -p "$TMPDIR"

# SHA-256, the same on GNU and on macOS. GNU coreutils' sha256sum can check a
# file against a digest ("sha256sum -c"); the sha256sum in macOS is the FreeBSD
# one, which only prints digests, and its shasum that can check.
#
#   sha256_of FILE...      the digest of the digest list of FILE... (one line)
#   sha256_matches DIGEST FILE
#                          exit 0 when FILE has DIGEST
if sha256sum --help 2>&1 | grep -q -- '--status'; then
    sha256_of() { sha256sum "$@" | sha256sum | cut -d' ' -f1; }
    sha256_matches() { printf '%s  %s\n' "$1" "$2" | sha256sum -c --status; }
elif command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$@" | shasum -a 256 | cut -d' ' -f1; }
    sha256_matches() { printf '%s  %s\n' "$1" "$2" | shasum -a 256 -c --status; }
else
    echo "no sha256sum and no shasum found" >&2
    exit 1
fi
