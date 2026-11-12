#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# KUAL > Aristotle > Start Aristotle: run the app in /mnt/us/aristotle in
# place of the stock GUI (run-aristotle.sh: takeover, app, stock GUI back
# when it quits). No computer needed.
#
# KUAL runs this as a child of the Java framework, which the takeover stops;
# upstart then signals the framework's whole process group. So the launcher
# starts in a session of its own (setsid) and this script returns at once.
#
# KUAL delivers taps faster than the app's first process appears (about 2.5 s
# on the PW4). One launch at a time is taken with an atomic mkdir lock on the
# tmpfs before anything is stopped: a second tap exits without touching the
# takeover, which used to bring the stock GUI back over the running app and
# leave a white screen (FIX-107).
#
# The app's log goes to /var/tmp/aristotle.log: on tmpfs so it survives USB
# drive mode unmounts without writing to /mnt/us (USB-001). When the app exits,
# run-aristotle.sh archives it to /mnt/us/.aristotle/app.log.
set -u
# R stands in for / in the host-side test (device/tests/agent.sh); it
# is empty on the device.
R=${AGENT_TEST_ROOT:-}
APP=${ARISTOTLE_APP_DIR:-$R/mnt/us/aristotle}
LOGDIR=$R/mnt/us/.aristotle
# Runtime state on the tmpfs: a reboot clears a lock left behind. run-aristotle.sh
# takes the lock over (it writes its own pid there) and releases it when the
# app has gone.
LOCK=$R/var/tmp/aristotle-launch.lock
# A lock this script has made but not yet handed over has no owner yet; only
# an older one belongs to a launch that died.
HANDOFF=10

say() {
    printf '%s\n' "$*"
    eips 0 1 "$*                                        " > /dev/null 2>&1
}

# Until the lock is handed over, this script's exit releases it.
mine=1
release() {
    [ "$mine" = 1 ] && rm -rf "$LOCK"
}
trap release EXIT
trap 'exit 1' HUP INT TERM

# owner: the pid recorded in the lock, 0 when there is none or it is not a
# number.
owner() {
    p=$(cat "$LOCK/pid" 2> /dev/null)
    case "$p" in '' | *[!0-9]*) p=0 ;; esac
    echo "$p"
}

# made: when the lock was created, 0 when unknown.
made() {
    c=$(cat "$LOCK/created" 2> /dev/null)
    case "$c" in '' | *[!0-9]*) c=0 ;; esac
    echo "$c"
}

mkdir -p "$R/var/tmp"
held=0
if mkdir "$LOCK" 2> /dev/null; then
    held=1
elif [ ! -d "$LOCK" ]; then
    # The tmpfs took no new directory (full?): there is no one to wait for,
    # and a launch must still happen, without a lock.
    mine=0
    say "aristotle: no room for a launch lock"
else
    # Someone else holds it: their lock must outlive this exit.
    p=$(owner)
    if [ "$p" -gt 0 ] && kill -0 "$p" 2> /dev/null; then
        say "aristotle: already running"
        mine=0
        exit 0
    fi
    c=$(made)
    now=$(date +%s)
    if [ "$c" -gt 0 ] && [ $((now - c)) -lt "$HANDOFF" ]; then
        say "aristotle: already starting"
        mine=0
        exit 0
    fi
    # Its launch died without releasing it.
    rm -rf "$LOCK"
    if mkdir "$LOCK" 2> /dev/null; then
        held=1
    else
        say "aristotle: already running"
        mine=0
        exit 0
    fi
fi
[ "$held" = 1 ] && date +%s > "$LOCK/created"

if [ ! -x "$APP/aristotle" ] || [ ! -f "$APP/run-aristotle.sh" ]; then
    say "aristotle: no app in /mnt/us/aristotle"
    exit 1
fi
if pidof aristotle > /dev/null; then
    say "aristotle: already running"
    exit 0
fi
mkdir -p "$LOGDIR"
say "Starting Aristotle..."
ARISTOTLE_LAUNCH_LOCK=$LOCK ARISTOTLE_LOG=/var/tmp/aristotle.log \
    setsid sh "$APP/run-aristotle.sh" < /dev/null > /dev/null 2>&1 &
mine=0
exit 0
