#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Name: 墨阅
# Author: 墨阅
# Icon: /mnt/us/aristotle/moyue-icon.png
# DontUseFBInk
#
# SH Integration 书架入口：在 Kindle 原生书架直接启动墨阅（无需经过 KUAL）。
# 优先走标准启动脚本（带防连点锁）；若用户移除了 KUAL 扩展目录，则直接拉起
# /mnt/us/aristotle/run-aristotle.sh。
set -u
export PATH=$PATH:/usr/sbin:/sbin

R=${AGENT_TEST_ROOT:-}
STARTER=$R/mnt/us/extensions/aristotle/bin/aristotle-start.sh
APP_DIR=${ARISTOTLE_APP_DIR:-$R/mnt/us/aristotle}
RUNNER=$APP_DIR/run-aristotle.sh

if [ -f "$STARTER" ]; then
    exec sh "$STARTER"
fi

if [ ! -x "$APP_DIR/aristotle" ] || [ ! -f "$RUNNER" ]; then
    printf '%s\n' 'Moyue not installed'
    eips 0 20 'Moyue not installed' >/dev/null 2>&1 || true
    exit 1
fi

if pidof aristotle >/dev/null 2>&1; then
    exit 0
fi

ARISTOTLE_LOG=$R/var/tmp/aristotle.log \
    setsid sh "$RUNNER" </dev/null >/dev/null 2>&1 &
exit 0
