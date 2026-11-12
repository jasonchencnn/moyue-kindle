#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Aristotle contributors
# Build the reading engine (crengine-ng, the inkread-engine crate) for the
# Kindle: a static armv7-unknown-linux-musl library plus the render_page
# example that runs on the device.
#
# Zig is the cross C/C++ compiler (musl, hard float, Cortex-A9 + NEON), the
# same toolchain tools/build-plato.sh uses. The C libraries the device package
# already ships (zlib, libpng, libjpeg, FreeType, HarfBuzz) are reused as they
# are, so a binary that links both engines ends up with one copy of each; the
# symbol analysis is in docs/tasks/ENG-005/notes.md. The four libraries Plato
# does not have (FriBiDi, libunibreak, utf8proc, zstd) are cross-built here.
#
# The cross sysroot itself is built by tools/engine-sysroot.sh, a library this
# script and tools/build-render.sh share: the reader app links crengine-ng
# too, and the sysroot must not be built twice.
#
# Everything lands in $ENGINE_BUILD (default $ARISTOTLE_CACHE/engine-build):
#   downloads/  the source tarballs, SHA-256 checked
#   src/        the unpacked sources
#   libs/       the static archives this script builds
#   sysroot/    usr/include + usr/lib: the cross sysroot the engine links to
#   bin/        the compiler wrappers
#   stamps/     one stamp per finished step, so the script is restartable
#
# The Rust side is built by cargo with CARGO_TARGET_DIR (default
# $ARISTOTLE_CACHE/target-eng005). engine/build.rs reads the cross compiler
# and the sysroot from the environment the sysroot library sets.
#
# Needs on the host: curl, tar, xz, make, cmake, cargo and the rustup target
# armv7-unknown-linux-musleabihf. Nothing is installed system-wide.
#
# Usage:
#   . tools/env.sh
#   JOBS=10 tools/build-engine.sh
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
. "$REPO/tools/env.sh"
ROOT=$(cd "$REPO/.." && pwd)

TARGET=armv7-unknown-linux-musleabihf

. "$REPO/tools/engine-sysroot.sh"

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$ARISTOTLE_CACHE/target-eng005}

# The cross sysroot and the environment engine/build.rs reads.
engine_sysroot_build

# --- The Rust side ------------------------------------------------------------
echo "=== cargo build (render_page for $TARGET)"
(cd "$ROOT" && cargo build --release --target "$TARGET" \
    -p inkread-engine --features crengine --example render_page)

BIN=$CARGO_TARGET_DIR/$TARGET/release/examples/render_page
ls -l "$BIN"
file "$BIN"
