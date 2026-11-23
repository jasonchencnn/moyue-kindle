#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Reversible takeover of the Kindle: stop Amazon's GUI stack so Aristotle owns
# the screen and most of the RAM, and bring it back afterwards.
#
#   ssh kindle sh -s start < device/app/takeover.sh
#   ssh kindle sh -s stop  < device/app/takeover.sh
#   ssh kindle sh -s status < device/app/takeover.sh
#   ssh kindle sh -s pause  < device/app/takeover.sh
#   ssh kindle sh -s resume < device/app/takeover.sh
#
# start/stop take the stock GUI down and bring it back: most RAM is freed, and
# the way back shows the boot splash for 30-60 s while Java restarts. This is
# the reliable path; launchers use it. start does not wait for cvm's 10-13 s
# shutdown: it returns about a second after the stop, while the app draws its
# first frame (STARTUP-001).
#
# pause/resume (EXPERIMENTAL) freeze the stock GUI instead (SIGSTOP, as KOReader
# and slint-kindle-backend do). On our PW4 the frozen framework was killed,
# most likely by Amazon's pmond watchdog (the poll_daemons job). The GUI then
# failed to come back (no Xorg, awesome or pillow) until a reboot. pause now
# refuses unless the stock UI is fully up, and holds poll_daemons while frozen.
# It is still unproven; don't build on it.
#
# Nothing is written to persistent storage: everything here is runtime state,
# so a reboot in the middle of a takeover, or after it, always returns the
# stock system. The flag that keeps the agent's watchdog from restarting the
# stock GUI, /var/run/aristotle-takeover, is on the /var tmpfs too. Kept running on purpose: powerd
# (power keys, battery, screensaver and suspend, which the app follows),
# volumd (USB mass storage), wifid, lipcd and dbus, plus our USB network and
# SSH.
#
# The stock crash collector is turned off for the length of the takeover with
# a stub bound over dmcc.sh, and unmounted when the stock framework is back
# (FIX-107, see DMCC below): it would otherwise write a 3.6 MB diagnostic
# bundle into /mnt/us/documents on every takeover.
#
# start and pause refuse to run, and change nothing, unless the device is a
# Kindle Paperwhite 4 (2018) on firmware 5.13.x or a Kindle Oasis 2 (2017) on
# firmware 5.16.x (the KUAL extension's guard.sh, looked for next to this
# script, then in the installed extension).
# ARISTOTLE_FORCE=1 skips that check, for developers trying another model or
# firmware on purpose, at their own risk. stop, resume and status always run.

# R stands in for / in the host-side test (device/tests/agent.sh); it
# is empty on the device.
R=${AGENT_TEST_ROOT:-}

# upstart's stop/start are symlinks to initctl in /sbin. An ssh session and
# KUAL both run with PATH=/usr/bin:/bin, where `stop` is not found: the stop
# of the stock GUI then fails with a "not found" that goes to /dev/null, and
# the stock UI keeps drawing its status bar (the clock) over ours every
# minute. Put /sbin and /usr/sbin in front so the commands below are found.
PATH=/sbin:/usr/sbin:/usr/bin:/bin${PATH:+:$PATH}
export PATH

# Jobs stopped by `stop lab126_gui`: x, framework (cvm), pillow, kb,
# kppreader, webreader, kfxreader, ... Their "stop on" stanzas follow it.
GUI=lab126_gui
# Background services that only serve the stock UI or Amazon's cloud.
# Same list KOReader stops to reclaim RAM, plus telemetry. `stop` starts the
# first group again; the second starts on its own on framework_ready, once
# the stock GUI is back [fw]. Starting stored by hand before that counts as
# a restart in its own monitor, which reboots the device after five quick
# ones in a session [fw: mesquite_functions].
EXTRA_LAB126="todo tmd rcm archive otav3 otaupd perfd"
EXTRA_FRAMEWORK="stored scanner dmld fastmetrics clickstream_logging"
EXTRA="$EXTRA_LAB126 $EXTRA_FRAMEWORK"
TAKEOVER=$R/var/run/aristotle-takeover
GOVERNOR=$R/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor
SAVED_GOVERNOR=$R/var/run/aristotle-governor
# The stock crash collector. Taking the stock GUI down makes cvm and the home
# app die by signal, and framework_setup dumps cvm when the framework takes
# longer than its 105 s budget to start; either way `dump-stack` runs
# `dmcc.sh`, which writes a 3.6 MB diagnostic bundle into /mnt/us/documents
# with cc.db, cookies and registration data in it (measured, FIX-107). A stub
# bound over dmcc.sh turns that off for the length of the takeover: runtime
# state only, nothing written, and a reboot removes it.
DMCC=$R/usr/bin/dmcc.sh
DMCC_STUB=$R/var/run/aristotle-dmcc-stub
DMCC_UNSTUB_PID=$R/var/run/aristotle-dmcc-unstub.pid
# A program that does nothing, to stand in for the collector. (The host-side
# test also runs on macOS, where true lives in /usr/bin.)
DMCC_SRC=/bin/true
[ -e "$DMCC_SRC" ] || DMCC_SRC=/usr/bin/true
# The mount tools, overridable for the host-side test, which has no root.
MOUNT=${ARISTOTLE_MOUNT:-mount}
UMOUNT=${ARISTOTLE_UMOUNT:-umount}
# Longer than framework_setup's 105 s budget plus the collector's tar.
DMCC_GRACE=${ARISTOTLE_DMCC_GRACE:-150}

governor_start() {
    [ -r "$GOVERNOR" ] && [ -w "$GOVERNOR" ] || return 0
    available=${GOVERNOR%/*}/scaling_available_governors
    [ -r "$available" ] || return 0
    read -r governors < "$available" || return 0
    case " $governors " in *' ondemand '*) ;; *) return 0 ;; esac
    if [ ! -e "$SAVED_GOVERNOR" ]; then
        read -r previous < "$GOVERNOR" || return 1
        printf '%s\n' "$previous" > "$SAVED_GOVERNOR" || return 1
    fi
    # Measured on PW4: early takeover can bypass stock's switch to ondemand.
    printf 'ondemand\n' > "$GOVERNOR"
}

governor_stop() {
    [ -r "$SAVED_GOVERNOR" ] || return 0
    read -r previous < "$SAVED_GOVERNOR" || return 1
    printf '%s\n' "$previous" > "$GOVERNOR" || return 1
    rm -f "$SAVED_GOVERNOR"
}
# dmcc_mounted: 1 when our stub is over dmcc.sh, so a repeated start does not
# stack a second mount on it.
dmcc_mounted() {
    grep -q " $DMCC " "$R/proc/mounts" 2> /dev/null
}

dmcc_stub_on() {
    [ -e "$DMCC" ] && [ -x "$DMCC_SRC" ] || return 0
    # The marker first: it tells a pending off_later that a takeover is on.
    : > "$DMCC_STUB" || return 1
    dmcc_mounted && return 0
    if "$MOUNT" --bind "$DMCC_SRC" "$DMCC" 2> /dev/null; then
        return 0
    fi
    rm -f "$DMCC_STUB"
    echo "takeover: could not block the crash log collector" >&2
    return 1
}

# dmcc_stub_off: unmount now, for a takeover that will not happen.
dmcc_stub_off() {
    rm -f "$DMCC_STUB"
    dmcc_mounted || return 0
    "$UMOUNT" "$DMCC" 2> /dev/null ||
        echo "takeover: could not remove the collector stub; a reboot will" >&2
}

# dmcc_stub_off_later: the framework can start slower than framework_setup's
# 105 s budget after the stock GUI comes back, and that timeout dumps cvm, so
# keep the stub until then. Detached: stop returns at once. A takeover that
# starts in the meantime leaves the stub for its own stop.
dmcc_stub_off_later() {
    rm -f "$DMCC_STUB"
    dmcc_mounted || return 0
    (
        sleep "$DMCC_GRACE"
        [ -e "$DMCC_STUB" ] && exit 0
        dmcc_mounted && "$UMOUNT" "$DMCC" 2> /dev/null
        rm -f "$DMCC_UNSTUB_PID"
    ) < /dev/null > /dev/null 2>&1 &
    echo $! > "$DMCC_UNSTUB_PID"
}
# The device guard: beside this script in the app bundle and the KUAL
# extension, in ../extension/ in the repository (device/app/), or in the
# installed extension when the script comes on stdin (sh -s).
guard() {
    [ "${ARISTOTLE_FORCE:-}" = 1 ] && return 0
    here=
    case "$0" in */takeover.sh) here=${0%/*} ;; takeover.sh) here=. ;; esac
    for g in ${here:+"$here/guard.sh" "$here/../extension/guard.sh"} \
        "$R/mnt/us/extensions/aristotle/guard.sh"; do
        if [ -f "$g" ]; then
            . "$g"
            device_guard
            return
        fi
    done
    echo "takeover: guard.sh not found; refusing (ARISTOTLE_FORCE=1 overrides)" >&2
    return 1
}
# The transition renderer is part of the existing app binary. Display errors
# must not prevent the stock interface from being restored.
TRANSITION_PID=$R/var/run/aristotle-transition.pid
transition_app() {
    [ -n "${ARISTOTLE_TRANSITION_APP:-}" ] && [ -x "$ARISTOTLE_TRANSITION_APP" ] && {
        printf '%s\n' "$ARISTOTLE_TRANSITION_APP"; return;
    }
    case "$0" in */takeover.sh) base=${0%/*} ;; *) base=. ;; esac
    for app in "$base/aristotle" "$base/app/aristotle" "$R/mnt/us/aristotle/aristotle"; do
        [ -x "$app" ] && { printf '%s\n' "$app"; return; }
    done
    return 1
}
transition_draw() {
    renderer=$(transition_app) || return 0
    timeout -t 5 -s KILL "$renderer" --transition "$1" </dev/null ||
        echo "takeover: transition display unavailable" >&2
}
transition_owner() {
    [ -r "$R/proc/$observer/cmdline" ] || return 1
    command_line=$(tr '\000' '\n' < "$R/proc/$observer/cmdline")
    [ "$command_line" = "$(printf '%s\n--transition\nwatch' "$renderer")" ]
}
transition_cancel() {
    [ -r "$TRANSITION_PID" ] || return 0
    { read -r observer; read -r renderer; } < "$TRANSITION_PID"
    case "$observer" in '' | *[!0-9]*) rm -f "$TRANSITION_PID"; return ;; esac
    if [ "$observer" -gt 1 ] && transition_owner; then
        kill -TERM "$observer" 2>/dev/null || true
        attempts=0
        while kill -0 "$observer" 2>/dev/null && transition_owner && [ "$attempts" -lt 20 ]; do
            sleep 0.1
            attempts=$((attempts + 1))
        done
        transition_owner && kill -KILL "$observer" 2>/dev/null || true
    fi
    rm -f "$TRANSITION_PID"
}
transition_watch() {
    renderer=$(transition_app) || return 0
    "$renderer" --transition watch </dev/null >/dev/null 2>&1 &
    printf '%s\n%s\n' "$!" "$renderer" > "$TRANSITION_PID"
}
# Processes frozen by `pause`: everything that would draw over us.
FROZEN="awesome cvm AaMenuApplication KindleContentDownloadManagerApplication"

mem() {
    awk '/MemTotal/ {t=$2} /MemFree/ {f=$2} /^Buffers/ {b=$2} /^Cached/ {c=$2}
         END {printf "RAM: %d MB total, %d MB available\n", t/1024, (f+b+c)/1024}' "$R/proc/meminfo"
}

state() {
    for job in $GUI x framework pillow kb $EXTRA powerd volumd wifid; do
        printf '%-20s %s\n' "$job" "$(initctl status "$job" 2>/dev/null | cut -d' ' -f2 | tr -d ,)"
    done
}

case "${1:-status}" in
start)
    guard || exit 1
    transition_cancel
    mem
    # Before the stock GUI goes: its shutdown is what makes the stock
    # processes die by signal.
    dmcc_stub_on || true
    # Tell the agent's watchdog that the stock GUI is down on purpose.
    touch "$TAKEOVER"
    # Do not wait for the jobs to finish stopping. Measured on the Oasis 2,
    # `stop lab126_gui` blocks 10 to 13 s: cvm tears Java down for that long
    # while the screen has been blank since under a second and nothing draws
    # any more. Waiting put the first frame of the app 15 s after the KUAL
    # tap (STARTUP-001); issuing the stop without waiting brings it to 3.5 s.
    stop -n "$GUI" > /dev/null 2>&1 || stop "$GUI" > /dev/null 2>&1
    for job in $EXTRA; do
        stop -n "$job" > /dev/null 2>&1 || stop "$job" > /dev/null 2>&1
    done
    governor_start || echo "warning: could not select ondemand governor" >&2
    # The stock UI draws a blank screen 0.6 to 1.7 s after the stop, and can
    # draw its splash once more while cvm goes (measured 4.7 to 5.5 s). Draw
    # after the first; the app's startup watch repairs the screen if the
    # second lands on it. A stop that silently failed (no `stop` on PATH
    # before the PATH above) leaves the job in start/running, and the stock
    # UI would draw its status bar over ours.
    sleep 1
    case $(initctl status "$GUI" 2>/dev/null) in
        *stop/stopping* | *stop/waiting*) ;;
        *)
            rm -f "$TAKEOVER"
            dmcc_stub_off
            echo "takeover: $GUI did not stop; refusing to draw over the stock UI" >&2
            exit 1
            ;;
    esac
    # The screensaver and sleep are the app's business from here: it
    # unloads blanket's screensaver and follows powerd's events. The rest of
    # cvm's shutdown runs in the background.
    # Blanket can retain the last framework splash after the GUI job stops.
    lipc-set-prop com.lab126.blanket unload splash 2>/dev/null || true
    lipc-send-event -r 5 com.lab126.hal bootSplashCleanup 2>/dev/null || true
    transition_draw prepare
    echo "takeover: stock GUI stopping; splash cleanup requested"
    mem
    ;;
stop)
    # Repeated stop requests must not repaint an already active stock UI.
    transitioning=0
    [ ! -e "$TAKEOVER" ] || transitioning=1
    transition_cancel
    if [ "$transitioning" = 1 ]; then
        transition_draw restoring
    fi
    governor_stop || echo "warning: could not restore CPU governor" >&2
    # Earlier versions kept the flag in the agent's persistent state.
    rm -f "$TAKEOVER" "$R/var/local/aristotle/takeover"
    dmcc_stub_off_later
    for job in $EXTRA_LAB126; do
        start "$job" > /dev/null 2>&1
    done
    # The app unloads blanket's screensaver and usb while it runs; a crash would
    # leave the stock GUI without them.
    lipc-set-prop com.lab126.blanket load screensaver 2> /dev/null
    lipc-set-prop com.lab126.blanket load usb 2> /dev/null
    [ "$transitioning" = 0 ] || transition_watch
    start "$GUI" > /dev/null 2>&1
    echo "takeover: stock GUI starting (takes about 20 s)"
    ;;
pause)
    guard || exit 1
    # Freezing a GUI that is still starting wedged it until reboot: require
    # the framework, the window manager and Pillow to be running.
    for p in cvm awesome pillowd; do
        pgrep -x "$p" > /dev/null || { echo "takeover: $p not running; refusing to pause (use start)" >&2; exit 1; }
    done
    # pmond kills processes that stop answering, which includes a frozen cvm.
    stop poll_daemons > /dev/null 2>&1
    # Hide the stock chrome, then freeze everything that draws: the window
    # manager, the Java framework and the React Native apps.
    lipc-set-prop com.lab126.pillow disableEnablePillow disable 2> /dev/null
    for p in $FROZEN; do
        killall -STOP "$p" 2> /dev/null
    done
    lipc-set-prop com.lab126.powerd preventScreenSaver 1
    echo "takeover: stock GUI frozen"
    ;;
resume)
    for p in $FROZEN; do
        killall -CONT "$p" 2> /dev/null
    done
    start poll_daemons > /dev/null 2>&1
    lipc-set-prop com.lab126.pillow disableEnablePillow enable 2> /dev/null
    # The stock UI does not know we drew over it. Reopen Home so the booklet
    # repaints, and ask Pillow to show its bars again, as KOReader does: the
    # status bar and toolbar are Pillow windows that do not repaint on their own.
    lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home 2> /dev/null
    for bar in default_status_bar search_bar; do
        lipc-set-prop com.lab126.pillow interrogatePillow \
            "{\"pillowId\": \"$bar\", \"function\": \"nativeBridge.showMe();\"}" 2> /dev/null
    done
    echo "takeover: stock GUI resumed"
    ;;
status)
    state
    mem
    ;;
*)
    echo "usage: takeover.sh start|stop|pause|resume|status" >&2
    exit 2
    ;;
esac
