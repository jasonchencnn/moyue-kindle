#!/bin/sh
# Name: 墨阅适配中心
# Author: 墨阅
# DontUseFBInk
#
# SH Integration 书库入口：只调用共用的真实入口，不复制实现。
# 依赖：本机 /mnt/us/koreader 的 KOReader 运行时（显示与触摸），
#       以及 /mnt/us/extensions/moyue-device-probe 里的适配中心脚本。
# 缺少依赖时明确报错，不假装已经启动。
set -u
export PATH=$PATH:/usr/sbin:/sbin

# R stands in for / on the host-side test; it is empty on the device.
R=${AGENT_TEST_ROOT:-}
CENTER_HOME=${MOYUE_CENTER_HOME:-$R/mnt/us/extensions/moyue-device-probe}
KO=${MOYUE_KOREADER_DIR:-$R/mnt/us/koreader}
LOCK=$R/var/tmp/moyue-center-entry.lock
CENTER_LOCK=$R/var/tmp/moyue-center.lock
STORE=$R/mnt/us/moyue-device-probe
LOG=$STORE/center-entry.log

say() {
    printf '%s\n' "$*"
    eips 0 20 "$*" >/dev/null 2>&1 || true
}

note() {
    mkdir -p "$STORE" 2>/dev/null || return 0
    printf '%s\n' "$*" >> "$LOG" 2>/dev/null || true
}

if [ ! -f "$CENTER_HOME/center.sh" ]; then
    say 'Moyue center missing'
    note "适配中心脚本不存在：$CENTER_HOME/center.sh"
    exit 1
fi

if [ ! -x "$KO/luajit" ] || [ ! -f "$KO/koreader.sh" ]; then
    say 'KOReader runtime missing'
    note '未检测到 /mnt/us/koreader 运行时，适配中心无法启动'
    exit 1
fi

# 适配中心自己在跑时不再启动第二份。
owner=$(cat "$CENTER_LOCK/pid" 2>/dev/null || true)
case "$owner" in
'' | *[!0-9]*) ;;
*)
    if kill -0 "$owner" 2>/dev/null; then
        say 'Moyue center busy'
        note '适配中心已在运行'
        exit 0
    fi
    ;;
esac

if ! mkdir -p "$(dirname "$LOCK")" 2>/dev/null; then
    say 'Cannot lock'
    exit 1
fi
if ! mkdir "$LOCK" 2>/dev/null; then
    say 'Moyue center busy'
    note '适配中心已在启动'
    exit 0
fi
printf '%s\n' "$$" > "$LOCK/pid"
trap 'rm -f "$LOCK/pid"; rmdir "$LOCK" 2>/dev/null || :' EXIT
trap 'exit 1' HUP INT TERM

say 'Starting Moyue Center...'
note "启动适配中心：$CENTER_HOME/center.sh"
setsid sh "$CENTER_HOME/center.sh" </dev/null >/dev/null 2>&1 &
exit 0
