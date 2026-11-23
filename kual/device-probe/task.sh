#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# 墨阅设备适配任务客户端（KUAL）
#
# 用法：
#   task.sh submit   本机采集后，把适配请求交给后台 worker 提交到适配服务
#   task.sh status   查询适配状态与安装状态，前台有界自动刷新
#   task.sh collect  仅在本机采集，不联网
#   task.sh install  用户确认后，重新查询状态并后台下载安装匹配包
#
# 约定：
#   - 报告、任务凭据、安装状态与安装日志都在 /mnt/us/moyue-device-probe/，
#     文件 0600，目录 0700。
#   - task.txt 保存任务 id 与 key；key 只经 curl 标准输入传递，
#     不出现在命令行参数、报告与日志中。install 的 worker 参数只有公开的
#     安装包 URL、SHA256 与版本号。
#   - 后台 worker 用 setsid 脱离 KUAL 进程组，关闭 KUAL 不终止远端任务；
#     worker 只写日志文件，不向屏幕输出，避免覆盖正在阅读的画面。
#   - 服务端响应与安装状态按固定协议逐行校验，不 source、不 eval、不执行。
#   - AGENT_TEST_ROOT 指向测试根时，工具与路径都在测试根内解析；
#     接口地址只允许在测试根下用 MOYUE_DEVICE_API 覆盖。
set -eu
umask 077

R=${AGENT_TEST_ROOT:-}
WORKER=''
MODE=${1:-}
REPORT_ARG=''
INSTALL_URL=''
INSTALL_SHA=''
INSTALL_VERSION=''
if [ "$MODE" = '__worker' ]; then
    WORKER=1
    MODE=worker
    shift
    REPORT_ARG=${1:-}
fi
if [ "$MODE" = '__install' ]; then
    WORKER=1
    MODE=install_worker
    shift
    [ $# -eq 3 ] || { printf '%s\n' '安装 worker 参数不对' >&2; exit 1; }
    INSTALL_URL=$1
    INSTALL_SHA=$2
    INSTALL_VERSION=$3
fi
case "$MODE" in
submit | status | collect | install) ;;
worker | install_worker) ;;
*) printf '%s\n' '用法：task.sh submit | status | collect | install' >&2; exit 1 ;;
esac

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SELF=$HERE/$(basename -- "$0")
PROBE=$HERE/probe.sh
INSTALLER=$HERE/install.sh
STORE=$R/mnt/us
OUTDIR=$STORE/moyue-device-probe
REPORT=$OUTDIR/report.txt
TASK=$OUTDIR/task.txt
CACHE=$OUTDIR/status.txt
LOGFILE=$OUTDIR/task.log
INSTLOG=$OUTDIR/install.log
INSTALL_STATE=$OUTDIR/install.txt
DONE=$OUTDIR/.task.done
LOCKBASE=$R/var/tmp
LOCK=$LOCKBASE/moyue-device-task.lock
INSTALL_LOCK=$LOCKBASE/moyue-device-install.lock
API=https://me.chen-cn.top/moyue/device/jobs

# 接口地址固定。只有测试根允许覆盖，避免把设备报告发到别的地址。
if [ -n "$R" ] && [ -n "${MOYUE_DEVICE_API:-}" ]; then
    API=$MOYUE_DEVICE_API
fi
case "$API" in
https://*) ;;
*) printf '%s\n' '适配服务地址必须使用 HTTPS' >&2; exit 1 ;;
esac
case "$API" in
*[!a-zA-Z0-9:/._-]*) printf '%s\n' '适配服务地址含非法字符' >&2; exit 1 ;;
esac

REFRESH=${MOYUE_TASK_REFRESH:-5}
INTERVAL=${MOYUE_TASK_INTERVAL:-10}
POLLS=${MOYUE_TASK_POLLS:-3}
POLL_INTERVAL=${MOYUE_TASK_POLL_INTERVAL:-300}
WAIT_SECONDS=${MOYUE_TASK_WAIT:-45}

is_uint() {
    case "$1" in
    '' | *[!0-9]*) return 1 ;;
    esac
    return 0
}

is_uint "$REFRESH" || REFRESH=5
is_uint "$INTERVAL" || INTERVAL=10
is_uint "$POLLS" || POLLS=3
is_uint "$POLL_INTERVAL" || POLL_INTERVAL=300
is_uint "$WAIT_SECONDS" || WAIT_SECONDS=45

# 解析工具位置。测试模式下只看测试根，不会误用宿主命令。
find_tool() {
    if [ -n "$R" ]; then
        for d in bin usr/bin sbin usr/sbin; do
            if [ -f "$R/$d/$1" ] && [ -x "$R/$d/$1" ]; then
                printf '%s\n' "$R/$d/$1"
                return 0
            fi
        done
        return 1
    fi
    command -v "$1" 2>/dev/null
}

EIPS_BIN=$(find_tool eips) || EIPS_BIN=

say() {
    printf '%s\n' "$*"
}

# eips 只支持 ASCII，中文详情留在标准输出和日志。
show() {
    say "$*"
    printf '%s\n' "$*" >> "$LOGFILE"
}

screen() {
    [ "${MOYUE_DEVICE_CENTER:-0}" != 1 ] || return 0
    if [ -z "$WORKER" ] && [ -n "$EIPS_BIN" ]; then
        screen_text=$(printf '%s' "$2" | LC_ALL=C tr -cd ' -~' | cut -c 1-58)
        screen_row=$1
        case "$screen_row" in
        2) screen_row=54 ;; 4) screen_row=55 ;; 6) screen_row=56 ;;
        8) screen_row=57 ;; 42) screen_row=58 ;;
        esac
        "$EIPS_BIN" 0 "$screen_row" "$(printf '%-58s' "$screen_text")" 2>/dev/null || true
    fi
}

fail() {
    screen 2 'Stage: failed'
    printf '%s\n' "任务失败：$*" >&2
    exit 1
}

is_hex() {
    case "$1" in
    '' | *[!0-9a-f]*) return 1 ;;
    esac
    [ "${#1}" -eq "$2" ]
}

valid_version() {
    case "$1" in '' | *[!0-9.]* | .* | *. | *..*) return 1 ;; esac
    old_ifs=$IFS
    IFS=.
    set -- $1
    IFS=$old_ifs
    [ $# -eq 3 ] || return 1
    for part in "$@"; do
        case "$part" in 0 | [1-9]*) ;; *) return 1 ;; esac
        [ ${#part} -le 3 ] || return 1
    done
    return 0
}

# 只接受服务器约定的公开下载前缀与 .tar.gz 文件名，查询串和路径段都拒绝。
allowed_package() {
    case "$1" in
    https://me.chen-cn.top/moyue/downloads/*.tar.gz) ;;
    *) return 1 ;;
    esac
    rest=${1#https://me.chen-cn.top/moyue/downloads/}
    case "$rest" in
    '' | */* | *[!A-Za-z0-9._-]*) return 1 ;;
    esac
    return 0
}

now_epoch() {
    date_bin=$(find_tool date) || return 1
    "$date_bin" -u +%s 2>/dev/null | head -n 1 | tr -d ' \t\r\n'
}

# --- 目录与文件安全 ---------------------------------------------------

prepare_outdir() {
    if [ -L "$OUTDIR" ]; then
        fail '输出目录是符号链接'
    fi
    if [ -e "$OUTDIR" ] && [ ! -d "$OUTDIR" ]; then
        fail '输出路径不是目录'
    fi
    mkdir -p "$OUTDIR" 2>/dev/null || fail '无法创建输出目录'
    if [ -L "$OUTDIR" ] || [ ! -d "$OUTDIR" ]; then
        fail '输出目录不是安全目录'
    fi
    chmod 700 "$OUTDIR" 2>/dev/null || true
    [ ! -L "$LOGFILE" ] || fail '日志是符号链接'
    if [ -e "$LOGFILE" ] && [ ! -f "$LOGFILE" ]; then
        fail '日志不是普通文件'
    fi
    [ ! -L "$INSTLOG" ] || fail '安装日志是符号链接'
    if [ -e "$INSTLOG" ] && [ ! -f "$INSTLOG" ]; then
        fail '安装日志不是普通文件'
    fi
    [ ! -L "$INSTALL_STATE" ] || fail '安装状态是符号链接'
    if [ -e "$INSTALL_STATE" ] && [ ! -f "$INSTALL_STATE" ]; then
        fail '安装状态不是普通文件'
    fi
}

# 原子替换写文件，拒绝符号链接目标。失败时返回非零，由调用方决定处理方式。
write_private() {
    target=$1
    content=$2
    if [ -L "$target" ]; then
        printf '%s\n' "$target 是符号链接，拒绝写入" >&2
        return 1
    fi
    if [ -e "$target" ] && [ ! -f "$target" ]; then
        printf '%s\n' "$target 不是普通文件" >&2
        return 1
    fi
    tmp=$OUTDIR/.$(basename -- "$target").tmp.$$
    ( set -C; : > "$tmp" ) 2>/dev/null || {
        printf '%s\n' '无法创建临时文件' >&2
        return 1
    }
    chmod 600 "$tmp" 2>/dev/null || true
    printf '%s\n' "$content" > "$tmp" 2>/dev/null || {
        rm -f "$tmp"
        printf '%s\n' '无法写入临时文件' >&2
        return 1
    }
    mv -f "$tmp" "$target" 2>/dev/null || {
        rm -f "$tmp"
        printf '%s\n' '无法替换目标文件' >&2
        return 1
    }
    return 0
}

# --- 任务凭据 ---------------------------------------------------------

# task.txt 格式固定为三行，不含任何其它字段。
save_task() {
    job_id=$1
    job_key=$2
    is_hex "$job_id" 32 || {
        printf '%s\n' '任务号格式不对' >&2
        return 1
    }
    is_hex "$job_key" 64 || {
        printf '%s\n' '任务密钥格式不对' >&2
        return 1
    }
    content=$(printf 'MOYUE-DEVICE-TASK-1\nid=%s\nkey=%s' "$job_id" "$job_key")
    write_private "$TASK" "$content"
}

TASK_ID=''
TASK_KEY=''

load_task() {
    TASK_ID=''
    TASK_KEY=''
    if [ ! -f "$TASK" ] || [ -L "$TASK" ]; then
        return 1
    fi
    lines=$(wc -l < "$TASK" | tr -d ' \t')
    [ "$lines" = 3 ] || return 1
    header=$(sed -n '1p' "$TASK" | tr -d '\r')
    [ "$header" = 'MOYUE-DEVICE-TASK-1' ] || return 1
    id_line=$(sed -n '2p' "$TASK" | tr -d '\r')
    key_line=$(sed -n '3p' "$TASK" | tr -d '\r')
    case "$id_line" in id=*) ;; *) return 1 ;; esac
    case "$key_line" in key=*) ;; *) return 1 ;; esac
    TASK_ID=${id_line#id=}
    TASK_KEY=${key_line#key=}
    is_hex "$TASK_ID" 32 || return 1
    is_hex "$TASK_KEY" 64 || return 1
    return 0
}

# --- 响应解析 ---------------------------------------------------------
# 响应内容只做逐行取值与格式比对，绝不 source、绝不 eval。

PARSE_ERROR=''

parse_job() {
    file=$1
    PARSE_ERROR=''
    if [ ! -f "$file" ]; then
        PARSE_ERROR='没有收到提交响应'
        return 1
    fi
    lines=$(wc -l < "$file" | tr -d ' \t')
    if [ "$lines" != 3 ]; then
        PARSE_ERROR='提交响应行数不对'
        return 1
    fi
    header=$(sed -n '1p' "$file" | tr -d '\r')
    if [ "$header" != 'MOYUE-DEVICE-JOB-1' ]; then
        PARSE_ERROR='提交响应标识不对'
        return 1
    fi
    id_line=$(sed -n '2p' "$file" | tr -d '\r')
    key_line=$(sed -n '3p' "$file" | tr -d '\r')
    case "$id_line" in id=*) ;; *) PARSE_ERROR='提交响应缺少任务号'; return 1 ;; esac
    case "$key_line" in key=*) ;; *) PARSE_ERROR='提交响应缺少任务密钥'; return 1 ;; esac
    JOB_ID=${id_line#id=}
    JOB_KEY=${key_line#key=}
    is_hex "$JOB_ID" 32 || { PARSE_ERROR='任务号格式不对'; return 1; }
    is_hex "$JOB_KEY" 64 || { PARSE_ERROR='任务密钥格式不对'; return 1; }
    return 0
}

ST_STATE=''
ST_MESSAGE=''
ST_UPDATED=''
ST_VERSION=''
ST_PACKAGE=''
ST_SHA256=''
ST_VALIDATION=''

parse_status() {
    file=$1
    PARSE_ERROR=''
    ST_STATE=''
    ST_MESSAGE=''
    ST_UPDATED=''
    ST_VERSION=''
    ST_PACKAGE=''
    ST_SHA256=''
    ST_VALIDATION=''
    if [ ! -f "$file" ]; then
        PARSE_ERROR='没有收到状态响应'
        return 1
    fi
    lines=$(wc -l < "$file" | tr -d ' \t')
    if [ "$lines" != 8 ]; then
        PARSE_ERROR='状态响应行数不对'
        return 1
    fi
    header=$(sed -n '1p' "$file" | tr -d '\r')
    if [ "$header" != 'MOYUE-DEVICE-STATUS-1' ]; then
        PARSE_ERROR='状态响应标识不对'
        return 1
    fi
    seen=''
    n=2
    while [ "$n" -le 8 ]; do
        line=$(sed -n "${n}p" "$file" | tr -d '\r')
        if [ -z "$line" ]; then
            PARSE_ERROR='状态响应有空行'
            return 1
        fi
        key=${line%%=*}
        val=${line#*=}
        if [ "$key" = "$line" ]; then
            PARSE_ERROR='状态响应字段格式不对'
            return 1
        fi
        case " $seen " in
        *" $key "*) PARSE_ERROR='状态响应字段重复'; return 1 ;;
        esac
        seen="$seen $key"
        case "$key" in
        state) ST_STATE=$val ;;
        message) ST_MESSAGE=$val ;;
        updated) ST_UPDATED=$val ;;
        version) ST_VERSION=$val ;;
        package) ST_PACKAGE=$val ;;
        sha256) ST_SHA256=$val ;;
        validation) ST_VALIDATION=$val ;;
        *)
            PARSE_ERROR='状态响应含未知字段'
            return 1
            ;;
        esac
        n=$((n + 1))
    done
    for k in state message updated version package sha256 validation; do
        case " $seen " in
        *" $k "*) ;;
        *)
            PARSE_ERROR='状态响应缺少字段'
            return 1
            ;;
        esac
    done
    check_status_values
}

check_status_values() {
    case "$ST_STATE" in
    package_ready | waiting_adaptation | adapting | building | testing | failed) ;;
    *)
        PARSE_ERROR='状态值不在约定范围'
        return 1
        ;;
    esac
    if [ -z "$ST_MESSAGE" ]; then
        PARSE_ERROR='状态消息为空'
        return 1
    fi
    if [ "${#ST_MESSAGE}" -gt 200 ]; then
        PARSE_ERROR='状态消息过长'
        return 1
    fi
    noise=$(printf '%s' "$ST_MESSAGE" | LC_ALL=C tr -d '\000-\010\013\014\016-\037\177')
    if [ "$noise" != "$ST_MESSAGE" ]; then
        PARSE_ERROR='状态消息含非法字符'
        return 1
    fi
    case "$ST_UPDATED" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]*) ;;
    *)
        PARSE_ERROR='状态时间格式不对'
        return 1
        ;;
    esac
    if [ -n "$ST_VERSION" ]; then
        case "$ST_VERSION" in
        *[!a-zA-Z0-9._+-]*)
            PARSE_ERROR='版本号含非法字符'
            return 1
            ;;
        esac
        if [ "${#ST_VERSION}" -gt 64 ]; then
            PARSE_ERROR='版本号过长'
            return 1
        fi
    fi
    if [ -n "$ST_PACKAGE" ]; then
        case "$ST_PACKAGE" in
        https://*) ;;
        *)
            PARSE_ERROR='下载地址不是 HTTPS'
            return 1
            ;;
        esac
        case "$ST_PACKAGE" in
        *[!a-zA-Z0-9:/._~+?=%-]*)
            PARSE_ERROR='下载地址含非法字符'
            return 1
            ;;
        esac
        if [ "${#ST_PACKAGE}" -gt 512 ]; then
            PARSE_ERROR='下载地址过长'
            return 1
        fi
    fi
    if [ -n "$ST_SHA256" ]; then
        is_hex "$ST_SHA256" 64 || { PARSE_ERROR='校验值格式不对'; return 1; }
    fi
    if [ -n "$ST_VALIDATION" ]; then
        case "$ST_VALIDATION" in
        preview | trial) ;;
        *)
            PARSE_ERROR='试用状态不对'
            return 1
            ;;
        esac
    fi
    if [ -n "$ST_PACKAGE" ]; then
        if [ -z "$ST_SHA256" ] || [ -z "$ST_VALIDATION" ]; then
            PARSE_ERROR='有下载地址时缺少校验值或试用状态'
            return 1
        fi
    else
        if [ -n "$ST_SHA256" ] || [ -n "$ST_VALIDATION" ]; then
            PARSE_ERROR='没有下载地址却带校验值或试用状态'
            return 1
        fi
    fi
    return 0
}

# --- 安装状态 ---------------------------------------------------------
# install.txt 只含状态，不含任务凭据、URL 或 SHA。格式不合法时只提示不可读，
# 不影响适配状态的查询。

IN_STATE=''
IN_STEP=''
IN_VERSION=''
IN_UPDATED=''

parse_install_state() {
    file=$1
    IN_STATE=''
    IN_STEP=''
    IN_VERSION=''
    IN_UPDATED=''
    [ -f "$file" ] && [ ! -L "$file" ] || return 1
    [ "$(wc -l < "$file" | tr -d ' \t')" = 5 ] || return 1
    [ "$(sed -n '1p' "$file" | tr -d '\r')" = 'MOYUE-DEVICE-INSTALL-1' ] || return 1
    state_line=$(sed -n '2p' "$file" | tr -d '\r')
    step_line=$(sed -n '3p' "$file" | tr -d '\r')
    version_line=$(sed -n '4p' "$file" | tr -d '\r')
    updated_line=$(sed -n '5p' "$file" | tr -d '\r')
    case "$state_line" in state=*) IN_STATE=${state_line#state=} ;; *) return 1 ;; esac
    case "$step_line" in step=*) IN_STEP=${step_line#step=} ;; *) return 1 ;; esac
    case "$version_line" in version=*) IN_VERSION=${version_line#version=} ;; *) return 1 ;; esac
    case "$updated_line" in updated=*) IN_UPDATED=${updated_line#updated=} ;; *) return 1 ;; esac
    case "$IN_STATE" in running | completed | failed) ;; *) return 1 ;; esac
    case "$IN_STEP" in
    precheck | download | verify | extract | guard | swap | done | failed) ;;
    *) return 1 ;;
    esac
    if [ "$IN_VERSION" != '-' ]; then
        valid_version "$IN_VERSION" || return 1
    fi
    case "$IN_UPDATED" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]*) ;;
    *) return 1 ;;
    esac
    return 0
}

install_step_label() {
    case "$1" in
    precheck) printf '%s\n' '检查环境' ;;
    download) printf '%s\n' '下载' ;;
    verify) printf '%s\n' '校验' ;;
    extract) printf '%s\n' '解压' ;;
    guard) printf '%s\n' '设备校验' ;;
    swap) printf '%s\n' '写入' ;;
    done) printf '%s\n' '完成' ;;
    *) printf '%s\n' '失败' ;;
    esac
}

show_install_status() {
    if ! parse_install_state "$INSTALL_STATE"; then
        if [ -e "$INSTALL_STATE" ] || [ -L "$INSTALL_STATE" ]; then
            show '安装：状态记录不可读（不会自动安装）'
        else
            show '安装：尚未开始'
        fi
        screen 8 'Install: not started'
        return 0
    fi
    case "$IN_STATE" in
    running)
        show "安装：进行中（$(install_step_label "$IN_STEP")）"
        screen 8 "Install: running ($IN_STEP)"
        if [ "$IN_STEP" = download ] && [ -f "$OUTDIR/install/txn/package.tar.gz" ] && [ ! -L "$OUTDIR/install/txn/package.tar.gz" ]; then
            download_kib=$(($(wc -c < "$OUTDIR/install/txn/package.tar.gz") / 1024))
            screen 42 "Downloaded: $download_kib KiB"
            show "已下载 ${download_kib} KiB"
        fi
        ;;
    completed)
        show "安装：已完成（版本 ${IN_VERSION}），请返回 KUAL 手动启动墨阅"
        screen 8 "Install: done $IN_VERSION"
        ;;
    failed)
        show "安装：失败（步骤：$(install_step_label "$IN_STEP")），详见 moyue-device-probe/install.log"
        screen 8 'Install: failed'
        ;;
    esac
}

# --- 网络 -------------------------------------------------------------

# 提交报告。报告内容走 --data-binary 读文件，不经命令行传密钥。
post_job() {
    body=$1
    curl_bin=$(find_tool curl) || {
        PARSE_ERROR='设备里没有 curl'
        return 1
    }
    resp=$OUTDIR/.job.tmp.$$
    ( set -C; : > "$resp" ) 2>/dev/null || {
        PARSE_ERROR='无法创建响应文件'
        return 1
    }
    chmod 600 "$resp" 2>/dev/null || true
    code=$("$curl_bin" -q --silent --show-error --fail --connect-timeout 15 --max-time 120 \
        --max-redirs 0 --proto '=https' --header 'Content-Type: text/plain; charset=utf-8' \
        --write-out '%{http_code}' --data-binary "@$body" --output "$resp" "$API") || {
        rm -f "$resp"
        PARSE_ERROR='提交失败，请检查 Wi-Fi 后重试'
        return 1
    }
    if [ "$code" != 200 ]; then
        rm -f "$resp"
        PARSE_ERROR='服务端没有接受这次提交'
        return 1
    fi
    if ! parse_job "$resp"; then
        rm -f "$resp"
        return 1
    fi
    rm -f "$resp"
    return 0
}

# 查询状态。任务密钥只经 curl 标准输入传递，不出现在 argv。
get_status() {
    job_id=$1
    job_key=$2
    curl_bin=$(find_tool curl) || {
        PARSE_ERROR='设备里没有 curl'
        return 1
    }
    resp=$OUTDIR/.status.tmp.$$
    ( set -C; : > "$resp" ) 2>/dev/null || {
        PARSE_ERROR='无法创建响应文件'
        return 1
    }
    chmod 600 "$resp" 2>/dev/null || true
    code=$(printf 'header = "Authorization: Bearer %s"\n' "$job_key" |
        "$curl_bin" -q --config - --silent --show-error --fail --connect-timeout 15 --max-time 30 \
            --max-redirs 0 --proto '=https' \
            --write-out '%{http_code}' --output "$resp" "$API/$job_id") || {
        rm -f "$resp"
        PARSE_ERROR='查询失败，请检查 Wi-Fi 后重试'
        return 1
    }
    if [ "$code" != 200 ]; then
        rm -f "$resp"
        PARSE_ERROR='服务端没有返回任务状态'
        return 1
    fi
    if ! parse_status "$resp"; then
        rm -f "$resp"
        return 1
    fi
    rm -f "$resp"
    return 0
}

# --- 锁 ---------------------------------------------------------------

acquire_lock() {
    if [ -L "$LOCKBASE" ]; then
        return 1
    fi
    mkdir -p "$LOCKBASE" 2>/dev/null || return 1
    if mkdir "$LOCK" 2>/dev/null; then
        now_epoch > "$LOCK/started_at" 2>/dev/null || true
        return 0
    fi
    worker_pid=''
    if [ -f "$LOCK/worker.pid" ]; then
        worker_pid=$(sed -n '1p' "$LOCK/worker.pid" 2>/dev/null | tr -d ' \t\r\n') || worker_pid=''
    fi
    if [ -n "$worker_pid" ]; then
        if kill -0 "$worker_pid" 2>/dev/null; then
            return 1
        fi
    else
        started=''
        if [ -f "$LOCK/started_at" ]; then
            started=$(sed -n '1p' "$LOCK/started_at" 2>/dev/null | tr -d ' \t\r\n') || started=''
        fi
        current=$(now_epoch || true)
        if [ -z "$started" ] || [ -z "$current" ]; then
            return 1
        fi
        if [ $((current - started)) -le 300 ]; then
            return 1
        fi
    fi
    # 上一次任务已经不在了，清理残留锁后重建。
    rm -f "$LOCK/worker.pid" "$LOCK/started_at" 2>/dev/null || true
    rmdir "$LOCK" 2>/dev/null || true
    if mkdir "$LOCK" 2>/dev/null; then
        now_epoch > "$LOCK/started_at" 2>/dev/null || true
        return 0
    fi
    return 1
}

release_lock() {
    [ -n "${LOCK:-}" ] || return 0
    owner=''
    if [ -f "$LOCK/worker.pid" ]; then
        owner=$(sed -n '1p' "$LOCK/worker.pid" 2>/dev/null | tr -d ' \t\r\n') || owner=''
    fi
    if [ -n "$owner" ] && [ "$owner" != "$$" ]; then
        return 0
    fi
    rm -f "$LOCK/worker.pid" "$LOCK/started_at" 2>/dev/null || true
    rmdir "$LOCK" 2>/dev/null || true
}

mark_worker() {
    if [ ! -d "$LOCK" ]; then
        mkdir "$LOCK" 2>/dev/null || return 1
    fi
    printf '%s\n' "$$" > "$LOCK/worker.pid" 2>/dev/null || return 1
    chmod 600 "$LOCK/worker.pid" 2>/dev/null || true
    return 0
}

# 安装锁由 install.sh 持有；这里只判断，避免用户连点两次启动两个安装。
install_lock_busy() {
    [ -d "$INSTALL_LOCK" ] || return 1
    [ ! -L "$INSTALL_LOCK" ] || return 0
    pid=''
    if [ -f "$INSTALL_LOCK/pid" ]; then
        pid=$(sed -n '1p' "$INSTALL_LOCK/pid" 2>/dev/null | tr -d ' \t\r\n') || pid=''
    fi
    if [ -n "$pid" ]; then
        kill -0 "$pid" 2>/dev/null && return 0
        return 1
    fi
    started=''
    if [ -f "$INSTALL_LOCK/started_at" ]; then
        started=$(sed -n '1p' "$INSTALL_LOCK/started_at" 2>/dev/null | tr -d ' \t\r\n') || started=''
    fi
    current=$(now_epoch || true)
    case "$started" in '' | *[!0-9]*) return 1 ;; esac
    case "$current" in '' | *[!0-9]*) return 0 ;; esac
    if [ $((current - started)) -le 300 ]; then
        return 0
    fi
    return 1
}

# --- 状态展示 ---------------------------------------------------------

stage_label() {
    case "$1" in
    package_ready) printf '%s\n' '已就绪' ;;
    waiting_adaptation) printf '%s\n' '等待适配' ;;
    adapting) printf '%s\n' '适配中' ;;
    building) printf '%s\n' '构建中' ;;
    testing) printf '%s\n' '验证中' ;;
    failed) printf '%s\n' '失败' ;;
    *) printf '%s\n' '未知阶段' ;;
    esac
}

show_status() {
    screen 2 "Stage: $ST_STATE"
    screen 4 "Updated: $ST_UPDATED"
    screen 6 "Version: ${ST_VERSION:--} ${ST_VALIDATION}"
    show "阶段：$(stage_label "$ST_STATE")"
    show "进展：$ST_MESSAGE"
    show "更新时间：$ST_UPDATED"
    if [ -n "$ST_VERSION" ]; then
        show "可用版本：$ST_VERSION"
    fi
    if [ -n "$ST_VALIDATION" ]; then
        case "$ST_VALIDATION" in
        preview) show '试用状态：预览版' ;;
        trial) show '试用状态：试用版' ;;
        esac
    fi
    if [ -n "$ST_PACKAGE" ]; then
        show "下载地址：$ST_PACKAGE"
        show "校验值：$ST_SHA256"
    fi
}

write_cache() {
    content=$(printf 'MOYUE-DEVICE-STATUS-1\nstate=%s\nmessage=%s\nupdated=%s\nversion=%s\npackage=%s\nsha256=%s\nvalidation=%s' \
        "$ST_STATE" "$ST_MESSAGE" "$ST_UPDATED" "$ST_VERSION" "$ST_PACKAGE" "$ST_SHA256" "$ST_VALIDATION")
    write_private "$CACHE" "$content"
}

show_cached() {
    [ -f "$CACHE" ] && [ ! -L "$CACHE" ] || return 1
    if ! parse_status "$CACHE"; then
        return 1
    fi
    show '以下是上次记录到的状态：'
    show_status
    return 0
}

write_done() {
    tmp=$OUTDIR/.done.tmp.$$
    ( set -C; : > "$tmp" ) 2>/dev/null || return 1
    printf '%s\n' "$1" > "$tmp" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
    chmod 600 "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$DONE" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
}

# --- 子流程 -----------------------------------------------------------

do_collect() {
    prepare_outdir
    [ -f "$PROBE" ] || fail '找不到 probe.sh'
    screen 2 'Stage: collecting'
    screen 4 'Updated: -'
    screen 6 'Version: -'
    show '正在本机采集设备信息…'
    if ! sh "$PROBE" local; then
        fail '采集失败，没有改动任何已保存的结果'
    fi
    show '已保存到 /mnt/us/moyue-device-probe/report.txt'
    screen 2 'Stage: collected locally'
    show '仅采集到本机，没有联网。'
}

do_submit() {
    prepare_outdir
    [ -f "$PROBE" ] || fail '找不到 probe.sh'
    if ! acquire_lock; then
        show '上一次适配请求还在进行，请先查看适配进度。'
        exit 1
    fi
    rm -f "$DONE" 2>/dev/null || true
    screen 2 'Stage: collecting'
    screen 4 'Updated: -'
    screen 6 'Version: -'
    show '正在本机采集设备信息…'
    if ! sh "$PROBE" local; then
        release_lock
        fail '采集失败，没有提交任何内容'
    fi
    if [ ! -f "$REPORT" ] || [ -L "$REPORT" ]; then
        release_lock
        fail '没有生成设备报告'
    fi
    spawn_worker
    screen 2 'Stage: submitting in background'
    show '采集完成，正在后台提交适配请求。'
    wait_worker || exit 1
}

spawn_worker() {
    setsid_bin=$(find_tool setsid) || setsid_bin=''
    nohup_bin=$(find_tool nohup) || nohup_bin=''
    if [ -n "$setsid_bin" ]; then
        "$setsid_bin" sh "$SELF" __worker "$REPORT" >> "$LOGFILE" 2>&1 < /dev/null &
    elif [ -n "$nohup_bin" ]; then
        "$nohup_bin" sh "$SELF" __worker "$REPORT" >> "$LOGFILE" 2>&1 < /dev/null &
    else
        (trap '' HUP; exec sh "$SELF" __worker "$REPORT") >> "$LOGFILE" 2>&1 < /dev/null &
    fi
}

wait_worker() {
    i=0
    while [ "$i" -lt "$WAIT_SECONDS" ]; do
        if [ -f "$DONE" ]; then
            result=$(sed -n '1p' "$DONE" 2>/dev/null | tr -d ' \t\r\n') || result=''
            case "$result" in
            ok)
                if load_task; then
                    screen 2 'Stage: submitted - check status'
                    show "任务已提交，任务号：$TASK_ID"
                    show '稍后可用「查看适配进度」查询结果。'
                else
                    show '后台提交结束，但没有取得任务凭据。'
                    return 1
                fi
                ;;
            *)
                screen 2 'Stage: submit failed - retry Wi-Fi'
                show '后台提交失败，可在 /mnt/us/moyue-device-probe/task.log 查看原因。'
                show '检查 Wi-Fi 后重新选择「同意上传并请求适配」。'
                return 1
                ;;
            esac
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    show '后台提交仍在进行，稍后用「查看适配进度」查询。'
}

# --- 首次安装 ---------------------------------------------------------

spawn_install_worker() {
    url=$1
    sha=$2
    version=$3
    setsid_bin=$(find_tool setsid) || setsid_bin=''
    nohup_bin=$(find_tool nohup) || nohup_bin=''
    if [ -n "$setsid_bin" ]; then
        "$setsid_bin" sh "$SELF" __install "$url" "$sha" "$version" >> "$INSTLOG" 2>&1 < /dev/null &
    elif [ -n "$nohup_bin" ]; then
        "$nohup_bin" sh "$SELF" __install "$url" "$sha" "$version" >> "$INSTLOG" 2>&1 < /dev/null &
    else
        (trap '' HUP; exec sh "$SELF" __install "$url" "$sha" "$version") \
            >> "$INSTLOG" 2>&1 < /dev/null &
    fi
}

# install 是唯一会下载并写入应用/扩展的入口。查询只在这里做一次，且不使用缓存。
do_install() {
    prepare_outdir
    [ -f "$INSTALLER" ] && [ ! -L "$INSTALLER" ] || fail '找不到安装脚本'
    if ! load_task; then
        show '本机还没有适配任务记录。'
        show '请先选择「同意上传并请求适配」，等服务器给出匹配包。'
        exit 1
    fi
    screen 42 'Install: checking latest status'
    if ! get_status "$TASK_ID" "$TASK_KEY"; then
        fail "安装前查询失败：${PARSE_ERROR}（未安装任何文件）"
    fi
    case "$ST_STATE" in
    package_ready) ;;
    *)
        show "服务器当前状态：$(stage_label "$ST_STATE")"
        show '只有「已就绪」的匹配包才能安装，未改动任何文件。'
        exit 1
        ;;
    esac
    [ -n "$ST_VERSION" ] && [ -n "$ST_PACKAGE" ] && [ -n "$ST_SHA256" ] || \
        fail '服务器没有给出完整版本、下载地址与 SHA-256，未安装'
    valid_version "$ST_VERSION" || fail '服务器版本号无效，未安装'
    is_hex "$ST_SHA256" 64 || fail '服务器 SHA-256 无效，未安装'
    allowed_package "$ST_PACKAGE" || fail '下载地址不是允许的 HTTPS .tar.gz，未安装'
    if install_lock_busy; then
        show '另一个安装正在进行，请用「查看适配进度」查看。'
        exit 1
    fi
    screen 42 "Install: started $ST_VERSION"
    spawn_install_worker "$ST_PACKAGE" "$ST_SHA256" "$ST_VERSION"
    show "已开始下载并安装墨阅 ${ST_VERSION}。"
    show '安装完成后不会自动启动；请用「查看适配进度」确认，再返回 KUAL 手动启动。'
}

do_install_worker() {
    url=$1
    sha=$2
    version=$3
    [ -f "$INSTALLER" ] && [ ! -L "$INSTALLER" ] || {
        say '安装脚本缺失'
        exit 1
    }
    allowed_package "$url" || { say '安装包地址无效'; exit 1; }
    is_hex "$sha" 64 || { say '安装包校验值无效'; exit 1; }
    valid_version "$version" || { say '安装包版本无效'; exit 1; }
    say "安装 worker 开始：$version"
    MOYUE_INSTALL_BACKGROUND=1 sh "$INSTALLER" "$url" "$sha" "$version"
    status=$?
    if [ "$status" -eq 0 ]; then
        say '安装 worker 结束：成功'
    else
        say "安装 worker 结束：失败（${status}）"
    fi
    exit "$status"
}

do_status() {
    prepare_outdir
    show_install_status
    if ! load_task; then
        show '本机还没有适配任务记录。'
        show '请先选择「同意上传并请求适配」。'
        exit 1
    fi
    got=0
    i=0
    while [ "$i" -lt "$REFRESH" ]; do
        if get_status "$TASK_ID" "$TASK_KEY"; then
            write_cache || true
            show_status
            show_install_status
            got=1
            case "$ST_STATE" in
            package_ready) [ "$IN_STATE" = running ] || break ;;
            failed) break ;;
            esac
        else
            screen 2 'Stage: query failed - retry Wi-Fi'
            show "查询失败：$PARSE_ERROR"
            show_cached || true
        fi
        i=$((i + 1))
        if [ "$i" -lt "$REFRESH" ] && [ "$INTERVAL" -gt 0 ]; then
            sleep "$INTERVAL"
        fi
    done
    if [ "$got" != 1 ]; then
        show '没有取得最新状态。任务凭据仍保留在本机，可以稍后重试。'
        exit 1
    fi
    show '适配与安装是两步，这里不会停止正在阅读的程序。'
}

do_worker() {
    report=$1
    mark_worker || true
    say '后台任务开始。'
    if ! post_job "$report"; then
        say "提交失败：$PARSE_ERROR"
        write_done fail || true
        release_lock
        exit 1
    fi
    if ! save_task "$JOB_ID" "$JOB_KEY"; then
        say '任务凭据保存失败。'
        write_done fail || true
        release_lock
        exit 1
    fi
    say "提交成功，任务号 ${JOB_ID}。"
    write_done ok || true
    # 有界轮询：只更新本地缓存，不向屏幕输出，也不伪造进度。
    i=0
    while [ "$i" -lt "$POLLS" ]; do
        if [ "$POLL_INTERVAL" -gt 0 ]; then
            sleep "$POLL_INTERVAL"
        fi
        if get_status "$JOB_ID" "$JOB_KEY"; then
            write_cache || true
            say "状态更新：$ST_STATE"
        else
            say "状态查询失败：$PARSE_ERROR"
        fi
        i=$((i + 1))
    done
    say '后台任务结束。'
    release_lock
}

case "$MODE" in
submit) do_submit ;;
status) do_status ;;
collect) do_collect ;;
install) do_install ;;
worker) do_worker "$REPORT_ARG" ;;
install_worker) do_install_worker "$INSTALL_URL" "$INSTALL_SHA" "$INSTALL_VERSION" ;;
esac
