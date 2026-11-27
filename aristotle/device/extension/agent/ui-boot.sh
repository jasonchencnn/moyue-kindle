#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Aristotle at boot, started by /etc/upstart/aristotle-ui.conf once the stock
# GUI's job has started, with /var/local/aristotle (checked) as the current
# directory. Also started by `aristotlectl app start`.
#
# It stops the stock GUI as takeover.sh does, tells powerd and blanket what
# the stock framework would have (the boot splash is gone), and runs the copy
# of the app in /var/local/aristotle/app, so USB drive mode cannot pull the
# binary away. Back to the stock GUI for this boot when:
#
# - /mnt/us/aristotle-disable exists (checked before anything is stopped);
# - three launches in a row did not stay up for two minutes (the count
#   survives reboots, so three forced restarts in a row also count); the
#   next boots stay stock until autostart is turned on again or the app is
#   started by hand;
# - Aristotle exits normally (the user quit);
# - `aristotlectl app stop` asks for it (not counted as a failure).
#
# Logs to ui.log, the app's output to app.log.
set -u
umask 077

# R stands in for / in the host-side test (device/tests/agent.sh); it is
# empty on the device.
R=${AGENT_TEST_ROOT:-}
S=${ARISTOTLE_DIR:-$R/var/local/aristotle}
LOG=$S/ui.log
FAILS=$S/ui-fails
MAX_FAILS=${MAX_FAILS:-3}
HEALTHY_AFTER=${HEALTHY_AFTER:-120}
MOUNT_WAIT=${MOUNT_WAIT:-30}
APP=$S/app/aristotle
TAKEOVER=$R/var/run/aristotle-takeover
open_kual=0

log() {
    printf '%s ui: %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >> "$LOG"
    if [ "$(wc -c < "$LOG")" -gt 262144 ]; then
        mv -f "$LOG" "$LOG.1"
    fi
}

uptime_s() {
    cut -d' ' -f1 "$R/proc/uptime"
}

fails() {
    n=$(cat "$FAILS" 2> /dev/null)
    case "$n" in '' | *[!0-9]*) n=0 ;; esac
    echo "$n"
}

set_fails() {
    echo "$1" > "$FAILS"
    sync
}

# Give the device back to the stock GUI and end the job; the job does not
# come back in this boot (aristotle-ui.conf).
stock() {
    log "stock GUI: $*"
    ARISTOTLE_TRANSITION_APP="$APP" sh "$S/takeover.sh" stop >> "$LOG" 2>&1
    if [ "$open_kual" = 1 ]; then
        sh "$S/open-kual.sh" >> "$LOG" 2>&1 || log "KUAL unavailable; stock GUI left running"
    fi
    exit 0
}

manual=0
if [ -e "$S/ui-request" ]; then
    manual=1
    rm -f "$S/ui-request"
fi
# Once per boot (aristotle-ui.conf compares this with the current boot id).
cat "$R/proc/sys/kernel/random/boot_id" > "$S/ui-booted" 2> /dev/null
log "start (uptime $(uptime_s) s$([ $manual = 1 ] && echo ', by hand'))"

# The kill switch. The userstore is mounted long before the stock GUI
# starts; wait a little in case it is not.
i=0
until grep -q " $R/mnt/us " "$R/proc/mounts" 2> /dev/null; do
    if [ $i -ge "$MOUNT_WAIT" ]; then
        log "/mnt/us unavailable; cannot check the kill switch; staying on the stock GUI"
        exit 0
    fi
    sleep 1
    i=$((i + 1))
done
if [ -e "$R/mnt/us/aristotle-disable" ]; then
    log "/mnt/us/aristotle-disable exists; staying on the stock GUI"
    exit 0
fi
if [ -d "$R/var/run/aristotle-app-update.lock" ]; then
    log "app update active; staying on the stock GUI"
    exit 0
fi
# An interrupted activation can leave only the previous complete copy.
if [ ! -e "$S/app" ] && [ -x "$S/app.previous/aristotle" ]; then
    APP=$S/app.previous/aristotle
    log "activation interrupted; using the previous boot app"
fi
if ! cmp -s "$(dirname "$APP")/USB-SOURCE.txt" "$R/mnt/us/aristotle/SOURCE.txt"; then
    log "USB app changed or boot copy untracked; refresh autostart from KUAL; staying on the stock GUI"
    exit 0
fi
n=$(fails)
if [ "$n" -ge "$MAX_FAILS" ] && [ $manual = 0 ]; then
    log "$n launches failed in a row; staying on the stock GUI (turn autostart on again to retry)"
    exit 0
fi
[ $manual = 1 ] && set_fails 0
if [ ! -x "$APP" ]; then
    log "no app at $APP; staying on the stock GUI"
    exit 0
fi

# Take the screen: the same steps as takeover.sh start, which is proven on
# the device.
if ! ARISTOTLE_TRANSITION_APP="$APP" sh "$S/takeover.sh" start >> "$LOG" 2>&1; then
    log "takeover refused"
    stock "takeover failed"
fi
# The framework may have been starting when its parent job stopped.
i=0
while [ $i -lt 10 ]; do
    initctl status framework 2> /dev/null | grep -q 'stop/waiting' &&
        ! pgrep -f /usr/java/bin/cvm > /dev/null && break
    stop framework > /dev/null 2>&1
    sleep 1
    i=$((i + 1))
done
# What framework_setup does for powerd and blanket once Java is up: no boot
# splash any more, and powerd may handle the power button.
lipc-set-prop com.lab126.blanket unload splash 2> /dev/null
lipc-send-event -r 5 com.lab126.hal bootSplashCleanup 2> /dev/null
log "stock GUI stopped (uptime $(uptime_s) s)"
timeout -t 5 -s KILL "$APP" --transition prepare </dev/null >> "$LOG" 2>&1 || log "transition display unavailable"

pid=
stop_app() {
    [ -n "$pid" ] && kill -TERM "$pid" 2> /dev/null && wait "$pid"
    rm -f "$S/app.pid"
    log "stopped by upstart"
    exit 0
}
trap stop_app TERM INT
trap '' HUP

while :; do
    n=$(($(fails) + 1))
    set_fails "$n"
    [ -f "$S/app.log" ] && mv -f "$S/app.log" "$S/app.log.1"
    log "launch $n (uptime $(uptime_s) s)"
    # A launch that stays up for HEALTHY_AFTER seconds is a good one.
    (sleep "$HEALTHY_AFTER" && set_fails 0) &
    healthy=$!
    ARISTOTLE_EXIT_ACTIONS=0
    [ ! -r "$S/open-kual.sh" ] || ARISTOTLE_EXIT_ACTIONS=1
    export ARISTOTLE_EXIT_ACTIONS
    ota_dir=${MOYUE_OTA_DIR:-$R/mnt/us/.aristotle/ota}
    MOYUE_OTA_DIR="$ota_dir" "$APP" "$@" >> "$S/app.log" 2>&1 &
    pid=$!
    echo "$pid" > "$S/app.pid"
    wait "$pid"
    rc=$?
    pid=
    kill "$healthy" 2> /dev/null
    rm -f "$S/app.pid"
    if [ -e "$S/ui-stop" ]; then
        how=$(cat "$S/ui-stop")
        rm -f "$S/ui-stop"
        # Asked to stop: not a failure (the count is 0 already if this
        # launch got healthy).
        cur=$(fails)
        [ "$cur" -gt 0 ] && set_fails $((cur - 1))
        if [ "$how" = keep ]; then
            log "stopped by aristotlectl (exit $rc); screen left to the caller"
            exit 0
        fi
        stock "stopped by aristotlectl (exit $rc)"
    fi
    if [ "$rc" = 20 ]; then
        set_fails 0
        open_kual=1
        stock "Aristotle requested KUAL"
    fi
    if [ "$rc" = 0 ]; then
        ota_dir=${MOYUE_OTA_DIR:-$R/mnt/us/.aristotle/ota}
        ext_dir=${ARISTOTLE_EXT_DIR:-$R/mnt/us/extensions/aristotle}
        usb_app=${ARISTOTLE_APP_DIR:-$R/mnt/us/aristotle}
        handover="$ota_dir/handover.conf"
        if [ ! -f "$handover" ] && [ -f "$usb_app/data/ota/handover.conf" ] && [ ! -L "$usb_app/data/ota/handover.conf" ]; then
            _legacy_ho="$usb_app/data/ota/handover.conf"
            _legacy_pkg=$(sed -n 's/^package=//p' "$_legacy_ho" | head -n 1)
            if mkdir -p "$ota_dir" 2> /dev/null && [ -n "$_legacy_pkg" ] && [ -f "$_legacy_pkg" ] && [ ! -L "$_legacy_pkg" ]; then
                _pkg_base=$(basename "$_legacy_pkg")
                if mv "$_legacy_pkg" "$ota_dir/$_pkg_base" 2> /dev/null; then
                    sed "s|^package=.*|package=$ota_dir/$_pkg_base|" "$_legacy_ho" > "$handover.tmp" \
                        && mv "$handover.tmp" "$handover" \
                        && rm -f "$_legacy_ho"
                fi
            fi
        fi
        if [ -f "$handover" ] && [ ! -L "$handover" ]; then
            log "OTA handover detected"
            _ota_tmp="$R/var/tmp/moyue-ota-run.$$"
            _ota_script="$ext_dir/bin/ota-update.sh"
            _ota_ok=0
            if [ -f "$_ota_script" ] && [ ! -L "$_ota_script" ]; then
                if mkdir -p "$_ota_tmp" 2> /dev/null && cp "$_ota_script" "$_ota_tmp/ota-update.sh" 2> /dev/null; then
                    _ota_script="$_ota_tmp/ota-update.sh"
                fi
                if (cd "${R:-/}" && ARISTOTLE_APP_DIR="$usb_app" ARISTOTLE_EXT_DIR="$ext_dir" \
                    MOYUE_OTA_DIR="$ota_dir" MOYUE_OTA_PIVOTED=1 sh "$_ota_script" handover) >> "$LOG" 2>&1; then
                    _ota_ok=1
                fi
                rm -rf "$_ota_tmp" 2> /dev/null || true
            fi
            if [ "$_ota_ok" = 1 ]; then
                # Refresh the internal boot copy from the updated USB app so the
                # restarted instance runs the new version and future boots match.
                if [ -x "$usb_app/aristotle" ] && [ -d "$usb_app/data" ] && [ -s "$usb_app/SOURCE.txt" ]; then
                    rm -rf "$S/app.new"
                    mkdir "$S/app.new"
                    if cp -r "$usb_app/aristotle" "$usb_app/data" "$usb_app/SOURCE.txt" "$S/app.new/" &&
                        cp "$usb_app/SOURCE.txt" "$S/app.new/USB-SOURCE.txt" &&
                        chmod 700 "$S/app.new/aristotle"; then
                        [ ! -f "$usb_app/VERSION" ] || cp "$usb_app/VERSION" "$S/app.new/VERSION"
                        sync
                        rm -rf "$S/app.previous"
                        [ ! -d "$S/app" ] || mv "$S/app" "$S/app.previous"
                        if mv "$S/app.new" "$S/app"; then
                            APP=$S/app/aristotle
                            set_fails 0
                            log "OTA handover applied and boot copy updated; restarting app"
                            continue
                        fi
                        [ ! -d "$S/app.previous" ] || mv "$S/app.previous" "$S/app"
                    fi
                    rm -rf "$S/app.new"
                fi
                log "OTA handover applied to USB app, but refreshing boot copy failed"
                stock "OTA boot copy refresh failed"
            else
                rm -f "$handover"
                log "OTA handover failed"
                stock "OTA handover failed"
            fi
        fi
        set_fails 0
        stock "Aristotle quit"
    fi
    log "Aristotle exited with $rc (launch $n)"
    [ "$(fails)" -ge "$MAX_FAILS" ] && stock "$MAX_FAILS failed launches in a row"
    sleep 2
done
