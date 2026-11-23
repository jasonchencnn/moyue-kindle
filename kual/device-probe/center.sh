#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
umask 077
export PATH=/usr/sbin:/sbin:/usr/bin:/bin
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# R stands in for / on the host-side test; it is empty on the device.
R=${AGENT_TEST_ROOT:-}
KO=$R/mnt/us/koreader
OUT=$R/mnt/us/moyue-device-probe
LOCK=$R/var/tmp/moyue-center.lock
RUNNER=$R/var/tmp/moyue-center-runner.sh

if [ "${1:-}" != __run ]; then
    [ ! -L "$OUT" ] && [ ! -L "$LOCK" ] || exit 1
    mkdir -p "$OUT"
    mkdir -p "$(dirname "$LOCK")" 2>/dev/null || exit 1
    # The center borrows the installed KOReader runtime for the screen and the
    # touch screen. Check it before detaching, so the failure is visible right
    # away instead of behind a background process.
    if [ ! -x "$KO/luajit" ] || [ ! -f "$KO/koreader.sh" ]; then
        printf '%s\n' 'KOReader runtime missing: /mnt/us/koreader is required' >> "$OUT/center-launch.log" 2>/dev/null || true
        eips 0 58 'KOReader runtime missing' || true
        exit 1
    fi
    if ! mkdir "$LOCK" 2>/dev/null; then
        old=$(cat "$LOCK/pid" 2>/dev/null || true)
        case "$old" in ''|*[!0-9]*) exit 0 ;; esac
        kill -0 "$old" 2>/dev/null && exit 0
        rm -f "$LOCK/pid"
        rmdir "$LOCK" && mkdir "$LOCK" || exit 1
    fi
    printf '%s\n' "$$" > "$LOCK/pid"
    setsid sh "$HERE/center.sh" __run </dev/null >>"$OUT/center-launch.log" 2>&1 &
    printf '%s\n' "$!" > "$LOCK/pid"
    exit 0
fi

printf '%s\n' "$$" > "$LOCK/pid"
trap 'rm -f "$LOCK/pid"; rmdir "$LOCK" 2>/dev/null || :' EXIT
if pidof luajit reader.lua >/dev/null 2>&1; then
    eips 0 58 'Please exit KOReader before opening Moyue Center.' || true
    exit 1
fi
if [ ! -x "$KO/luajit" ] || [ ! -f "$KO/koreader.sh" ]; then
    printf '%s\n' 'KOReader runtime missing: /mnt/us/koreader is required' >> "$OUT/center-launch.log" 2>/dev/null || true
    eips 0 58 'KOReader runtime missing' || true
    exit 1
fi
if [ ! -f "$OUT/report.txt" ]; then
    MOYUE_DEVICE_CENTER=1 sh "$HERE/task.sh" collect || true
fi
export MOYUE_CENTER_ENTRY="$HERE/center.lua"
export MOYUE_CENTER_HOME="$HERE"
export MOYUE_CENTER_LOG="$OUT/center-runtime.log"
export KOREADER_DIR="$KO"
cd "$KO"
./luajit "$HERE/center-bootstrap.lua" "$KO/koreader.sh" "$RUNNER"
sh "$RUNNER" --kual
