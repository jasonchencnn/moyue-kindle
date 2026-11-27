#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run the Aristotle reader in place of the stock UI, then bring the stock UI
# back. Lives next to the `aristotle` binary and its data/ directory (the app
# bundle from tools/build-render.sh unpacks all of them into one folder):
#
#   ssh kindle sh /mnt/us/aristotle/run-aristotle.sh [aristotle options]
#   ssh kindle sh /mnt/us/aristotle/run-aristotle.sh --turn-mode 7
#
# The screen is taken with `takeover.sh start` and given back with
# `takeover.sh stop`, which restarts the stock GUI (30 to 60 s of boot splash).
# `takeover.sh pause`/`resume` (SIGSTOP) is deliberately not used: on our
# firmware the frozen Java framework was killed by Amazon's watchdog and the
# GUI did not come back without a reboot (seen on the device, 2026-10-17).
#
# The app's stderr (timings as `[perf ...]` lines) goes to $ARISTOTLE_LOG
# (default /var/tmp/aristotle.log) and is printed when the app exits. Hangups
# are ignored, so the app keeps running if the SSH session drops; the stock
# GUI comes back whenever it exits. Quit by holding the "Biblioteka" title for
# a second, or with `killall aristotle` (progress is saved either way).
#
# KUAL > Aristotle > Start Aristotle runs this script detached
# (extension/bin/aristotle-start.sh), with the log on the USB drive.
# R stands in for / in the host-side test (device/tests/agent.sh); it is
# empty on the device.
R=${AGENT_TEST_ROOT:-}
DIR=${ARISTOTLE_APP_DIR:-$(cd "$(dirname "$0")" && pwd)}
cd "${R:-/}" 2> /dev/null || cd /
LOG=${ARISTOTLE_LOG:-/var/tmp/aristotle.log}
# Pivot to a tmpfs copy of this launcher so /mnt/us/aristotle has no open file
# handle or working directory reference when OTA renames it on Kindle FUSE/vfat.
RUNNER_TMP=${ARISTOTLE_RUNNER_TMP:-}
if [ -z "$RUNNER_TMP" ] && [ -f "$0" ]; then
    if mkdir -p "$R/var/tmp" 2> /dev/null; then
        _tmp="$R/var/tmp/aristotle-run.$$.sh"
        if cp "$0" "$_tmp" 2> /dev/null; then
            exec env ARISTOTLE_APP_DIR="$DIR" ARISTOTLE_RUNNER_TMP="$_tmp" sh "$_tmp" "$@"
        fi
        rm -f "$_tmp" 2> /dev/null || true
    fi
fi
# The KUAL launch lock, which stops a second tap from starting a second
# instance while this one comes up (FIX-107). This script takes it over from
# the launcher and releases it when it ends; runs by hand have none.
LOCK=${ARISTOTLE_LAUNCH_LOCK:-}

# The app installs its own SIGINT/SIGTERM handlers (save and leave); the
# script itself must survive them to restore the stock GUI.
trap '' HUP INT TERM
trap 'release' EXIT

release() {
    [ -n "$LOCK" ] && rm -rf "$LOCK"
    [ -n "$RUNNER_TMP" ] && rm -f "$RUNNER_TMP"
}

if [ -n "$LOCK" ]; then
    [ -d "$LOCK" ] || mkdir -p "$LOCK"
    echo $$ > "$LOCK/pid"
fi

# An Aristotle started at boot (or by `aristotlectl app start`) has the
# screen: stop it first, without counting it as a crash and without bringing
# the stock GUI back in between.
if initctl status aristotle-ui 2> /dev/null | grep -q start/running; then
    sh "$R/var/local/aristotle/aristotlectl" app stop --keep-takeover
fi

start=0
[ -f "$LOG" ] && start=$(wc -l < "$LOG")
echo "=== aristotle $(date) $*" >> "$LOG"
# takeover.sh refuses anything but a PW4 on firmware 5.13.x; then the app does
# not start either, and the stock GUI is still ours, so it must not be
# restarted: a failed start only means there is nothing to give back
# (FIX-107).
if ! sh "$DIR/takeover.sh" start; then
    echo "=== takeover refused" >> "$LOG"
    exit 1
fi
OTA_DIR=${MOYUE_OTA_DIR:-$R/mnt/us/.aristotle/ota}
EXT_DIR=${ARISTOTLE_EXT_DIR:-$R/mnt/us/extensions/aristotle}
HANDOVER="$OTA_DIR/handover.conf"

while :; do
    ARISTOTLE_EXIT_ACTIONS=0
    [ ! -r "$DIR/open-kual.sh" ] || ARISTOTLE_EXIT_ACTIONS=1
    export ARISTOTLE_EXIT_ACTIONS
    MOYUE_OTA_DIR="$OTA_DIR" "$DIR/aristotle" "$@" >> "$LOG" 2>&1
    status=$?
    echo "=== exit $status" >> "$LOG"
    if [ "$status" = 0 ] && [ ! -f "$HANDOVER" ] && [ -f "$DIR/data/ota/handover.conf" ] && [ ! -L "$DIR/data/ota/handover.conf" ]; then
        # Migrate legacy handover written inside $DIR/data/ota out to $OTA_DIR
        # before ota-update.sh renames $DIR; keeping $OTA_DIR inside $DIR would
        # make `mv "$APP" "$OTA/backup/app"` fail with EINVAL.
        _legacy_dir="$DIR/data/ota"
        _legacy_ho="$_legacy_dir/handover.conf"
        _legacy_pkg=$(sed -n 's/^package=//p' "$_legacy_ho" | head -n 1)
        if mkdir -p "$OTA_DIR" 2> /dev/null && [ -n "$_legacy_pkg" ] && [ -f "$_legacy_pkg" ] && [ ! -L "$_legacy_pkg" ]; then
            _pkg_base=$(basename "$_legacy_pkg")
            if mv "$_legacy_pkg" "$OTA_DIR/$_pkg_base" 2> /dev/null; then
                sed "s|^package=.*|package=$OTA_DIR/$_pkg_base|" "$_legacy_ho" > "$HANDOVER.tmp" \
                    && mv "$HANDOVER.tmp" "$HANDOVER" \
                    && rm -f "$_legacy_ho"
            fi
        fi
    fi
    if [ "$status" = 0 ] && [ -f "$HANDOVER" ] && [ ! -L "$HANDOVER" ]; then
        echo "=== ota handover detected" >> "$LOG"
        if [ -f "$EXT_DIR/bin/ota-update.sh" ] && [ ! -L "$EXT_DIR/bin/ota-update.sh" ]; then
            _ota_tmp="$R/var/tmp/moyue-ota-run.$$"
            _ota_script="$EXT_DIR/bin/ota-update.sh"
            if mkdir -p "$_ota_tmp" 2> /dev/null && cp "$_ota_script" "$_ota_tmp/ota-update.sh" 2> /dev/null; then
                _ota_script="$_ota_tmp/ota-update.sh"
            fi
            if (cd "${R:-/}" && ARISTOTLE_APP_DIR="$DIR" ARISTOTLE_EXT_DIR="$EXT_DIR" \
                MOYUE_OTA_DIR="$OTA_DIR" MOYUE_OTA_PIVOTED=1 sh "$_ota_script" handover) >> "$LOG" 2>&1; then
                rm -rf "$_ota_tmp" 2> /dev/null || true
                echo "=== ota handover applied; restarting app" >> "$LOG"
                continue
            fi
            rm -rf "$_ota_tmp" 2> /dev/null || true
            echo "=== ota handover failed" >> "$LOG"
        else
            echo "=== ota updater missing: $EXT_DIR/bin/ota-update.sh" >> "$LOG"
            rm -f "$HANDOVER"
        fi
    fi
    break
done
sh "$DIR/takeover.sh" stop
if [ "$status" = 20 ]; then
    sh "$DIR/open-kual.sh" >> "$LOG" 2>&1
    status=0
fi
if [ -d "$R/mnt/us/.aristotle" ]; then
    [ -f "$R/mnt/us/.aristotle/app.log" ] && mv -f "$R/mnt/us/.aristotle/app.log" "$R/mnt/us/.aristotle/app.log.1" 2>/dev/null || true
    cp -f "$LOG" "$R/mnt/us/.aristotle/app.log" 2>/dev/null || true
fi
tail -n "+$((start + 1))" "$LOG"
exit $status
