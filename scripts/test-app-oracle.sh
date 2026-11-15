#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run the `aristotle` app crate's unit tests on a native armv7 host.
#
# The app crate's tests are built for the device target
# (armv7-unknown-linux-musleabihf, static) and cannot run on the Mac, so they
# run on the ARM server `oracle-ip2` (aarch64 Ubuntu, runs armv7 static
# binaries natively). Several tests read font files through paths that the
# compiler baked in: env!("CARGO_MANIFEST_DIR")/../aristotle-{ui,render}/assets
# and the repository root for engine/fixtures, so this script mirrors those
# source trees to the same absolute path on the server before it runs.
#
# usage: scripts/test-app-oracle.sh [-n|--no-build] [TEST-ARGS...]
#
#   -n, --no-build   reuse the test binary from the last build
#   TEST-ARGS        passed to the test binary, e.g. `fonts::` or `--nocapture`
#
# Environment:
#   CARGO_TARGET_DIR  build directory (default $HOME/.cache/aristotle/target-QA-103)
#   ORACLE            SSH host (default oracle-ip2), must accept BatchMode keys
#   ORACLE_DIR        work directory on the server (default ~/inkread-debug/qa103)
#
# Every run prints the build, the uploaded binary, the test summary and exits
# with the test binary's status.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
ORACLE=${ORACLE:-oracle-ip2}
ORACLE_DIR=${ORACLE_DIR:-inkread-debug/qa103}
SSH="ssh -o BatchMode=yes -o ConnectTimeout=15 $ORACLE"
TARGET=armv7-unknown-linux-musleabihf
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/aristotle/target-QA-103}

build=1
if [ "${1:-}" = -n ] || [ "${1:-}" = --no-build ]; then
    build=0
    shift
fi

# 1. Cross-build the app crate's test binary, plus everything build-render.sh
#    needs in the environment (Plato libs, crengine sysroot, the Zig cc wrappers).
if [ "$build" = 1 ]; then
    echo "=== build: $TARGET (CARGO_TARGET_DIR=$CARGO_TARGET_DIR)"
    (cd "$REPO/aristotle" && CARGO_CMD=test CARGO_FLAGS="--no-run" tools/build-render.sh device)
fi

DEPS=$CARGO_TARGET_DIR/$TARGET/release/deps
# The newest `aristotle-<16 hex>` file is this build's unit-test binary; the
# renderer's is `aristotle_render-*` and the integration tests are separate.
BIN=$(ls -t "$DEPS"/aristotle-* 2>/dev/null | grep -E '/aristotle-[0-9a-f]{16}$' | head -1 || true)
[ -n "$BIN" ] || { echo "no app test binary in $DEPS; run without -n" >&2; exit 1; }
echo "=== binary: $BIN"

REMOTE_HOME=$($SSH 'printf %s "$HOME"' 2>/dev/null | tr -d '\r')
WORK=$REMOTE_HOME/$ORACLE_DIR
# 2. Mirror the source paths the tests read into the same absolute location on
#    the server. The two asset folders are small; engine/fixtures has the
#    books the renderer tests open.
echo "=== mirror: $REPO -> $ORACLE:$REPO"
$SSH "sudo mkdir -p '$REPO' '$WORK/tmp' '$WORK/data' && sudo chown -R \$(id -u):\$(id -g) '$REPO' '$WORK'"
tar -czf - -C "$REPO" aristotle/crates engine/fixtures core |
    $SSH "tar -xzf - -C '$REPO'"

# 3. The app's tests build the UI with `ui_fonts("serif", "fonts")`, relative
#    to the data directory (the current directory, as at runtime), so the
#    bundle's data has to be in place too.
DATA=$HOME/.cache/aristotle/render/device/data
[ -d "$DATA/serif" ] || { echo "no $DATA/serif; run tools/build-render.sh device" >&2; exit 1; }
tar -czf - -C "$DATA" . | $SSH "tar -xzf - -C '$WORK/data'"

# 4. Upload and run from the data directory.
scp -q -o BatchMode=yes "$BIN" "$ORACLE:$WORK/aristotle-tests"
# ARISTOTLE_RENDER_DATA points the engine-open tests at the bundle; those tests
# skip themselves without it. RENDER_DATA=none leaves it unset, which gives the
# same set as a plain run on a host without tools/build-render.sh's data.
RENDER_DATA=${RENDER_DATA-$WORK/data}
case $RENDER_DATA in
    none) env_prefix="" ;;
    *) env_prefix="ARISTOTLE_RENDER_DATA='$RENDER_DATA'" ;;
esac
echo "=== run on $ORACLE: $WORK/aristotle-tests $*"
status=0
$SSH "cd '$WORK/data' && TMPDIR='$WORK/tmp' $env_prefix ./../aristotle-tests $*" || status=$?
echo "=== exit $status"
exit $status
