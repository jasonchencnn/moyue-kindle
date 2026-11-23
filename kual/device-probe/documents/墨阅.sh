#!/bin/sh
# Name: 墨阅
# Author: 墨阅
# DontUseFBInk
#
# SH Integration 书库入口：启动本机已安装的墨阅应用。
# 依赖：/mnt/us/aristotle 里的已安装应用（run-aristotle.sh 与可执行文件）。
# 应用没装时明确报错，不假装已经启动。
set -u
export PATH=$PATH:/usr/sbin:/sbin

# R stands in for / on the host-side test; it is empty on the device.
R=${AGENT_TEST_ROOT:-}
APP_DIR=${MOYUE_APP_DIR:-$R/mnt/us/aristotle}
RUNNER=$APP_DIR/run-aristotle.sh
LOCK=$R/var/tmp/moyue-app-entry.lock
STORE=$R/mnt/us/moyue-device-probe
LOG=$STORE/app-entry.log

say() {
    printf '%s\n' "$*"
    eips 0 20 "$*" >/dev/null 2>&1 || true
}

note() {
    mkdir -p "$STORE" 2>/dev/null || return 0
    printf '%s\n' "$*" >> "$LOG" 2>/dev/null || true
}

if [ ! -f "$RUNNER" ] || [ ! -x "$APP_DIR/aristotle" ]; then
    say 'Moyue not installed'
    note "墨阅没有安装在 ${APP_DIR}，请先安装墨阅再启动"
    exit 1
fi

if pidof aristotle >/dev/null 2>&1; then
    say 'Moyue already running'
    note '墨阅已经在运行'
    exit 0
fi

if ! mkdir -p "$(dirname "$LOCK")" 2>/dev/null; then
    say 'Cannot lock'
    exit 1
fi
if ! mkdir "$LOCK" 2>/dev/null; then
    say 'Moyue already running'
    note '墨阅已经在启动'
    exit 0
fi
printf '%s\n' "$$" > "$LOCK/pid"
trap 'rm -f "$LOCK/pid"; rmdir "$LOCK" 2>/dev/null || :' EXIT
trap 'exit 1' HUP INT TERM

say 'Starting Moyue...'
note "启动墨阅：$RUNNER"
ARISTOTLE_LOG=$R/var/tmp/aristotle.log \
    setsid sh "$RUNNER" </dev/null >/dev/null 2>&1 &
exit 0
