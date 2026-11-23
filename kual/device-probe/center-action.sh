#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -u
umask 077
export PATH=/usr/sbin:/sbin:/usr/bin:/bin
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUT=${AGENT_TEST_ROOT:-}/mnt/us/moyue-device-probe
LOCK=${AGENT_TEST_ROOT:-}/var/tmp/moyue-center-action.lock
MODE=${1:-}
CATEGORY=''
case "$MODE" in
submit | status | install | feedback_link | feedback_send | feedback_logs) ;;
*) exit 2 ;;
esac
if [ "$MODE" = feedback_send ]; then
    shift
    CATEGORY=${1:-}
    case "$CATEGORY" in
    works | white_screen | touch | layout | launch | other) ;;
    *) exit 2 ;;
    esac
fi
[ ! -L "$OUT" ] && [ ! -L "$LOCK" ] || exit 1
mkdir -p "$OUT" "$(dirname "$LOCK")" || exit 1
if ! mkdir "$LOCK" 2>/dev/null; then
    owner=$(cat "$LOCK/pid" 2>/dev/null || true)
    case "$owner" in '' | *[!0-9]*) exit 75 ;; esac
    kill -0 "$owner" 2>/dev/null && exit 75
    rm -f "$LOCK/pid"
    rmdir "$LOCK" && mkdir "$LOCK" || exit 75
fi
printf '%s\n' "$$" > "$LOCK/pid"
trap 'rm -f "$LOCK/pid"; rmdir "$LOCK" 2>/dev/null || :' EXIT
trap 'exit 1' HUP INT TERM

state() {
    [ ! -L "$OUT/center-action.txt" ] || exit 1
    tmp=$OUT/.center-action.$$
    ( set -C; : > "$tmp" ) 2>/dev/null || exit 1
    if [ -n "$CATEGORY" ]; then
        printf 'MOYUE-CENTER-ACTION-1\nmode=%s\nstate=%s\npid=%s\ncategory=%s\n' \
            "$MODE" "$1" "$$" "$CATEGORY" > "$tmp"
    else
        printf 'MOYUE-CENTER-ACTION-1\nmode=%s\nstate=%s\npid=%s\n' "$MODE" "$1" "$$" > "$tmp"
    fi
    mv -f "$tmp" "$OUT/center-action.txt"
}

run_action() {
    case "$MODE" in
    feedback_link) sh "$HERE/feedback.sh" link ;;
    feedback_send) sh "$HERE/feedback.sh" send "$CATEGORY" ;;
    feedback_logs) sh "$HERE/feedback.sh" logs ;;
    *) MOYUE_DEVICE_CENTER=1 MOYUE_TASK_REFRESH=1 sh "$HERE/task.sh" "$MODE" ;;
    esac
}

state running
[ ! -L "$OUT/center-action.log" ] || exit 1
if run_action > "$OUT/center-action.log" 2>&1; then
    state completed
else
    state failed
fi
