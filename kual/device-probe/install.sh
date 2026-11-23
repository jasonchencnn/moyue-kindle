#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# 墨阅首次安装器：只接受适配服务给出的公开 HTTPS 包地址。
#
# 用法：sh install.sh URL SHA256 VERSION
#
# 安全边界：
#   - URL 固定为 https://me.chen-cn.top/moyue/downloads/*.tar.gz，不跟随跳转；
#   - 首次连接 Oracle 独立端口，保留域名证书校验；失败后经原入口续传；
#   - 下载有连接/总超时和 64 MiB 上限，SHA-256 校验通过后才解压；
#   - 解压前逐行检查 tar 名单：只允许 aristotle/ 和 extensions/aristotle/，
#     拒绝绝对路径、穿越、反斜杠、控制字符、重复项和链接/特殊文件；
#   - 解压到 /mnt/us/moyue-device-probe/install，与目标同一文件系统；
#   - 校验包内应用、数据、启动器、配置和 VERSION，应用/扩展版本必须等于服务端版本；
#   - 用包内 aristotle/guard.sh 校验机型和固件，绝不接受 ARISTOTLE_FORCE；
#   - 只做首次安装：已有 /mnt/us/aristotle 或 extensions/aristotle 时拒绝；
#   - 两次 rename 前写 journal；中断后下次运行把本次装出的目录移回暂存区，
#     不删除来源不明的目录；回滚失败时保留 journal 与暂存数据。
#
# 不读取 task.txt、不碰 .aristotle、不改 root、不启动应用、不停止 KUAL。
set -u
umask 077

R=${AGENT_TEST_ROOT:-}
if [ -z "$R" ]; then
    PATH=/usr/sbin:/sbin:$PATH
    export PATH
fi
BASE=$R/mnt/us/moyue-device-probe
STORE=$R/mnt/us
APP=$R/mnt/us/aristotle
EXT=$R/mnt/us/extensions/aristotle
EXTBASE=$R/mnt/us/extensions
INSTALL=$BASE/install
TXN=$INSTALL/txn
ROOT=$TXN/root
SAPP=$ROOT/aristotle
SEXT=$ROOT/extensions/aristotle
PKG=$TXN/package.tar.gz
JOURNAL=$INSTALL/journal.txt
STATE=$BASE/install.txt
LOG=$BASE/install.log
LOCK=$R/var/tmp/moyue-device-install.lock
MAX_PACKAGE=67108864
URL=${1:-}
SHA=${2:-}
VER=${3:-}
STEP=precheck
LOCK_HELD=0
RECOVERED=0
EIPS=
TAR=
DF=

say() {
    printf '%s\n' "$*"
}

log() {
    [ -d "$BASE" ] || return 0
    printf '%s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || printf '1970-01-01T00:00:00Z')" "$*" \
        >> "$LOG" 2>/dev/null || true
}

# 只在开始和结束时写屏幕底部，不按步骤反复覆盖阅读画面。
screen() {
    [ "${MOYUE_INSTALL_BACKGROUND:-0}" = 1 ] && return 0
    [ -n "$EIPS" ] || return 0
    text=$(printf '%s' "$2" | LC_ALL=C tr -cd ' -~' | cut -c 1-58)
    "$EIPS" 0 "$1" "$(printf '%-58s' "$text")" 2>/dev/null || true
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

is_sha() {
    case "$1" in '' | *[!0-9a-f]*) return 1 ;; esac
    [ ${#1} -eq 64 ]
}

allowed_url() {
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

sha256_file() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v openssl > /dev/null 2>&1; then
        openssl dgst -sha256 "$1" | sed 's/.*= //'
    else
        return 1
    fi
}

now_string() {
    date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || printf '1970-01-01T00:00:00Z'
}

# install.txt 只保存状态，不保存 URL、SHA、任务号或密钥。
write_state() {
    state=$1
    step=$2
    [ ! -L "$STATE" ] || return 1
    if [ -e "$STATE" ] && [ ! -f "$STATE" ]; then
        return 1
    fi
    state_version=$VER
    valid_version "$state_version" || state_version=-
    tmp=$BASE/.install.txt.tmp.$$
    ( set -C; : > "$tmp" ) 2>/dev/null || return 1
    chmod 600 "$tmp" 2>/dev/null || true
    printf 'MOYUE-DEVICE-INSTALL-1\nstate=%s\nstep=%s\nversion=%s\nupdated=%s\n' \
        "$state" "$step" "$state_version" "$(now_string)" > "$tmp" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
    mv -f "$tmp" "$STATE" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
    return 0
}

begin_step() {
    STEP=$1
    log "step $STEP"
    write_state running "$STEP" || {
        log "cannot write install state"
        say '安装失败：无法写入安装状态'
        exit 1
    }
}

fail() {
    log "failed at $STEP: $*"
    [ "$LOCK_HELD" != 1 ] || write_state failed "$STEP" || true
    say "安装失败：$*"
    screen 42 "Install: failed ($STEP)"
    exit 1
}

# 还没开始改动文件就拒绝：保留上一次 completed 状态，不写成 failed。
refuse() {
    log "refused at $STEP: $*"
    say "安装未开始：$*"
    screen 42 "Install: not started"
    exit 1
}

cleanup() {
    if [ "$LOCK_HELD" = 1 ]; then
        rm -f "$LOCK/pid" "$LOCK/started_at" 2>/dev/null || true
        rmdir "$LOCK" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

lock_owner_alive() {
    [ -d "$LOCK" ] || return 1
    [ ! -L "$LOCK" ] || return 1
    pid=
    [ -f "$LOCK/pid" ] && pid=$(sed -n '1p' "$LOCK/pid" 2>/dev/null | tr -d ' \t\r\n')
    [ -n "$pid" ] || return 1
    kill -0 "$pid" 2>/dev/null
}

acquire_lock() {
    [ -L "$LOCK" ] && fail '安装锁是符号链接'
    mkdir -p "$R/var/tmp" 2>/dev/null || fail '无法创建锁目录'
    if mkdir "$LOCK" 2>/dev/null; then
        printf '%s\n' "$$" > "$LOCK/pid" 2>/dev/null || {
            rmdir "$LOCK" 2>/dev/null || true
            fail '无法写入安装锁'
        }
        date -u +%s > "$LOCK/started_at" 2>/dev/null || true
        chmod 600 "$LOCK/pid" 2>/dev/null || true
        LOCK_HELD=1
        return 0
    fi
    [ -d "$LOCK" ] && [ ! -L "$LOCK" ] || fail '安装锁无效'
    if lock_owner_alive; then
        fail '另一个安装正在进行'
    fi
    pid=
    [ -f "$LOCK/pid" ] && pid=$(sed -n '1p' "$LOCK/pid" 2>/dev/null | tr -d ' \t\r\n')
    if [ -z "$pid" ]; then
        started=
        [ -f "$LOCK/started_at" ] && started=$(sed -n '1p' "$LOCK/started_at" 2>/dev/null | tr -d ' \t\r\n')
        if [ -z "$started" ]; then
            started=$(stat -c '%Y' "$LOCK" 2>/dev/null || stat -f '%m' "$LOCK" 2>/dev/null || true)
        fi
        current=$(date -u +%s 2>/dev/null || true)
        case "$started" in '' | *[!0-9]*) started= ;; esac
        case "$current" in '' | *[!0-9]*) current= ;; esac
        if [ -z "$started" ] || [ -z "$current" ] || [ $((current - started)) -le 300 ]; then
            fail '另一个安装正在启动'
        fi
    fi
    rm -f "$LOCK/pid" "$LOCK/started_at" 2>/dev/null || true
    rmdir "$LOCK" 2>/dev/null || fail '无法清理残留安装锁'
    mkdir "$LOCK" 2>/dev/null || fail '无法创建安装锁'
    printf '%s\n' "$$" > "$LOCK/pid" 2>/dev/null || fail '无法写入安装锁'
    date -u +%s > "$LOCK/started_at" 2>/dev/null || true
    chmod 600 "$LOCK/pid" 2>/dev/null || true
    LOCK_HELD=1
}

prepare_paths() {
    [ -L "$STORE" ] && fail '用户存储是符号链接'
    [ -d "$STORE" ] || fail '找不到用户存储 /mnt/us'
    [ -L "$BASE" ] && fail '适配目录是符号链接'
    if [ -e "$BASE" ] && [ ! -d "$BASE" ]; then
        fail '适配目录不是目录'
    fi
    mkdir -p "$BASE" 2>/dev/null || fail '无法创建适配目录'
    chmod 700 "$BASE" 2>/dev/null || true
    [ -L "$STATE" ] && fail '安装状态是符号链接'
    [ -L "$LOG" ] && fail '安装日志是符号链接'
    if [ -e "$LOG" ] && [ ! -f "$LOG" ]; then
        fail '安装日志不是普通文件'
    fi
    [ -L "$INSTALL" ] && fail '安装暂存目录是符号链接'
    if [ -e "$INSTALL" ] && [ ! -d "$INSTALL" ]; then
        fail '安装暂存路径不是目录'
    fi
    mkdir -p "$INSTALL" 2>/dev/null || fail '无法创建安装暂存目录'
    chmod 700 "$INSTALL" 2>/dev/null || true
    if [ -e "$EXTBASE" ] || [ -L "$EXTBASE" ]; then
        [ -L "$EXTBASE" ] && fail 'KUAL 扩展目录是符号链接'
        [ -d "$EXTBASE" ] || fail 'KUAL 扩展路径不是目录'
    fi
}

write_marker() {
    target=$1
    [ -d "$target" ] && [ ! -L "$target" ] || return 1
    [ -f "$target/.moyue-install" ] && [ -L "$target/.moyue-install" ] && return 1
    tmp=$target/.moyue-install.tmp.$$
    printf 'MOYUE-INSTALL-1\nversion=%s\nsha256=%s\n' "$VER" "$SHA" > "$tmp" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
    chmod 600 "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$target/.moyue-install" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
}

# 只把带本次版本标记的目录当作自己的产物。
is_ours() {
    dir=$1
    want=$2
    [ -d "$dir" ] && [ ! -L "$dir" ] || return 1
    [ -f "$dir/.moyue-install" ] && [ ! -L "$dir/.moyue-install" ] || return 1
    [ "$(sed -n '1p' "$dir/.moyue-install" 2>/dev/null | tr -d '\r')" = 'MOYUE-INSTALL-1' ] || return 1
    got=$(sed -n 's/^version=//p' "$dir/.moyue-install" 2>/dev/null | head -n 1 | tr -d '\r')
    got_sha=$(sed -n 's/^sha256=//p' "$dir/.moyue-install" 2>/dev/null | head -n 1 | tr -d '\r')
    [ "$got" = "$want" ] && [ "$got_sha" = "${3:-$SHA}" ]
}

write_journal() {
    phase=$1
    [ ! -L "$JOURNAL" ] || return 1
    tmp=$INSTALL/.journal.tmp.$$
    printf 'MOYUE-DEVICE-INSTALL-TXN-1\nversion=%s\nsha256=%s\nphase=%s\n' \
        "$VER" "$SHA" "$phase" > "$tmp" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
    chmod 600 "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$JOURNAL" 2>/dev/null || {
        rm -f "$tmp"
        return 1
    }
    sync 2>/dev/null || true
}

parse_journal() {
    [ -f "$JOURNAL" ] && [ ! -L "$JOURNAL" ] || return 1
    [ "$(wc -l < "$JOURNAL" | tr -d ' \t')" = 4 ] || return 1
    [ "$(sed -n '1p' "$JOURNAL" | tr -d '\r')" = 'MOYUE-DEVICE-INSTALL-TXN-1' ] || return 1
    jver=$(sed -n 's/^version=//p' "$JOURNAL" | head -n 1 | tr -d '\r')
    jsha=$(sed -n 's/^sha256=//p' "$JOURNAL" | head -n 1 | tr -d '\r')
    jphase=$(sed -n 's/^phase=//p' "$JOURNAL" | head -n 1 | tr -d '\r')
    valid_version "$jver" || return 1
    is_sha "$jsha" || return 1
    case "$jphase" in staged | app-moved | ext-moved) ;; *) return 1 ;; esac
    J_VER=$jver
    J_SHA=$jsha
    J_PHASE=$jphase
    return 0
}

# 回滚只把带本次版本标记的目录移回暂存区；来源不明时不删除。
rollback_partial() {
    if [ -e "$APP" ] || [ -L "$APP" ]; then
        is_ours "$APP" "$VER" || return 1
        [ ! -e "$SAPP" ] && [ ! -L "$SAPP" ] || return 1
        mkdir -p "$ROOT" 2>/dev/null || return 1
        mv "$APP" "$SAPP" 2>/dev/null || return 1
    fi
    if [ -e "$EXT" ] || [ -L "$EXT" ]; then
        is_ours "$EXT" "$VER" || return 1
        [ ! -e "$SEXT" ] && [ ! -L "$SEXT" ] || return 1
        mkdir -p "$ROOT/extensions" 2>/dev/null || return 1
        mv "$EXT" "$SEXT" 2>/dev/null || return 1
    fi
    sync 2>/dev/null || true
    return 0
}

clean_transaction() {
    rm -f "$JOURNAL" 2>/dev/null || true
    rm -rf "$TXN" 2>/dev/null || true
    sync 2>/dev/null || true
}

# 上次中断恢复：两个目录都在且都是本次产物则视为完成；只出现一个则移回暂存区。
recover() {
    [ -L "$JOURNAL" ] && fail '安装记录是符号链接，拒绝继续'
    [ -L "$TXN" ] && fail '暂存目录是符号链接'
    [ -L "$ROOT" ] && fail '解压目录是符号链接'
    if [ -f "$JOURNAL" ]; then
        parse_journal || fail "安装记录损坏，保留在 $JOURNAL"
        if [ -d "$APP" ] && [ -d "$EXT" ] && is_ours "$APP" "$J_VER" "$J_SHA" && is_ours "$EXT" "$J_VER" "$J_SHA"; then
            log 'previous transaction completed; cleaning up'
            write_state completed done || true
            clean_transaction
            RECOVERED=1
            return 0
        fi
        if { [ -e "$APP" ] || [ -L "$APP" ]; } && ! is_ours "$APP" "$J_VER" "$J_SHA"; then
            fail '应用目录不是本次安装创建的，保留安装记录与暂存文件'
        fi
        if { [ -e "$EXT" ] || [ -L "$EXT" ]; } && ! is_ours "$EXT" "$J_VER" "$J_SHA"; then
            fail '扩展目录不是本次安装创建的，保留安装记录与暂存文件'
        fi
        if [ -e "$APP" ] || [ -L "$APP" ]; then
            [ ! -e "$SAPP" ] && [ ! -L "$SAPP" ] || fail '暂存区已有应用目录，保留安装记录'
            mkdir -p "$ROOT" 2>/dev/null || fail '无法创建恢复暂存目录'
            mv "$APP" "$SAPP" 2>/dev/null || fail '无法把上次应用目录移回暂存区'
        fi
        if [ -e "$EXT" ] || [ -L "$EXT" ]; then
            [ ! -e "$SEXT" ] && [ ! -L "$SEXT" ] || fail '暂存区已有扩展目录，保留安装记录'
            mkdir -p "$ROOT/extensions" 2>/dev/null || fail '无法创建恢复暂存目录'
            mv "$EXT" "$SEXT" 2>/dev/null || fail '无法把上次扩展目录移回暂存区'
        fi
        log 'rolled back an interrupted installation'
        clean_transaction
    fi
    if [ -e "$TXN" ] || [ -L "$TXN" ]; then
        [ -L "$TXN" ] && fail '暂存目录是符号链接'
        rm -rf "$TXN" 2>/dev/null || fail '无法清理旧暂存目录'
    fi
}

preflight_targets() {
    if [ -e "$APP" ] || [ -L "$APP" ]; then
        refuse '已存在 /mnt/us/aristotle；首次安装不覆盖现有应用'
    fi
    if [ -e "$EXT" ] || [ -L "$EXT" ]; then
        refuse '已存在 extensions/aristotle；首次安装不覆盖现有扩展'
    fi
}

check_not_running() {
    mode=${1:-}
    PIDOF=$(command -v pidof 2>/dev/null) || fail '缺少 pidof，无法确认墨阅未运行'
    if "$PIDOF" aristotle > /dev/null 2>&1; then
        if [ "$mode" = refuse ]; then
            refuse '墨阅正在运行；请先返回 Kindle 系统再安装'
        fi
        fail '墨阅正在运行；请先返回 Kindle 系统再安装'
    fi
}

validate_name() {
    name=$1
    [ -n "$name" ] || fail '压缩包包含空路径'
    case "$name" in /* | *\\*) fail "压缩包包含绝对路径或反斜杠：$name" ;; esac
    case "/$name/" in
    *'/../'* | *'/./'*) fail "压缩包包含路径穿越：$name" ;;
    esac
    case "$name" in *'//'*) fail "压缩包包含空路径段：$name" ;; esac
    noise=$(printf '%s' "$name" | LC_ALL=C tr -d '\001-\037\177')
    [ "$noise" = "$name" ] || fail '压缩包路径含控制字符'
    norm=${name%/}
    [ -n "$norm" ] || fail '压缩包包含空路径'
    case "$norm" in
    aristotle | extensions/aristotle) ;;
    aristotle/* | extensions/aristotle/*) ;;
    *) fail "压缩包包含不允许的路径：$name" ;;
    esac
    if grep -F -x -q -e "$norm" "$TXN/seen" 2>/dev/null; then
        fail "压缩包包含重复路径：$name"
    fi
    printf '%s\n' "$norm" >> "$TXN/seen" || fail '无法记录压缩包路径'
}

check_archive() {
    TAR=$(command -v tar 2>/dev/null) || fail '缺少 tar'
    LC_ALL=C "$TAR" -tzf "$PKG" > "$TXN/names" 2>/dev/null || fail '压缩包无法读取'
    [ -s "$TXN/names" ] || fail '压缩包为空'
    : > "$TXN/seen" || fail '无法创建路径检查文件'
    while IFS= read -r name; do
        validate_name "$name"
    done < "$TXN/names"
    LC_ALL=C "$TAR" -tvzf "$PKG" > "$TXN/types" 2>/dev/null || fail '压缩包无法读取'
    names_count=$(wc -l < "$TXN/names" | tr -d ' \t')
    types_count=$(wc -l < "$TXN/types" | tr -d ' \t')
    [ "$names_count" = "$types_count" ] || fail '压缩包类型列表不完整'
    while IFS= read -r line; do
        case "$line" in
        -*) ;;
        d*) ;;
        *) fail '压缩包包含符号链接、硬链接或特殊文件' ;;
        esac
        case "$line" in
        *' link to '* | *' -> '*) fail '压缩包包含链接条目' ;;
        esac
    done < "$TXN/types"
}

check_stage() {
    [ -d "$SAPP" ] && [ ! -L "$SAPP" ] || fail '包内缺少应用目录'
    [ -d "$SEXT" ] && [ ! -L "$SEXT" ] || fail '包内缺少扩展目录'
    [ -f "$SAPP/aristotle" ] && [ ! -L "$SAPP/aristotle" ] && [ -x "$SAPP/aristotle" ] \
        || fail '包内缺少可执行的应用二进制'
    [ -d "$SAPP/data" ] && [ ! -L "$SAPP/data" ] || fail '包内缺少应用数据目录'
    for f in run-aristotle.sh takeover.sh open-kual.sh guard.sh VERSION; do
        [ -f "$SAPP/$f" ] && [ ! -L "$SAPP/$f" ] || fail "包内缺少 aristotle/$f"
    done
    [ -x "$SAPP/run-aristotle.sh" ] || fail '包内启动脚本不可执行'
    [ -x "$SAPP/takeover.sh" ] || fail '包内接管脚本不可执行'
    for f in config.xml menu.json guard.sh bin/aristotle-start.sh bin/aristotle-stop.sh; do
        [ -f "$SEXT/$f" ] && [ ! -L "$SEXT/$f" ] || fail "包内缺少 extensions/aristotle/$f"
    done
    [ -x "$SEXT/bin/aristotle-start.sh" ] || fail '包内扩展启动器不可执行'
    app_version=$(sed -n '1p' "$SAPP/VERSION" 2>/dev/null | tr -d '\r')
    [ "$app_version" = "$VER" ] || fail "包内应用版本 ${app_version:-missing} 与服务端 $VER 不一致"
    ext_version=$(sed -n 's:.*<version>\([0-9][0-9.]*\)</version>.*:\1:p' "$SEXT/config.xml" 2>/dev/null | head -n 1)
    [ "$ext_version" = "$VER" ] || fail "包内扩展版本 ${ext_version:-missing} 与服务端 $VER 不一致"
    write_marker "$SAPP" || fail '无法写入应用安装标记'
    write_marker "$SEXT" || fail '无法写入扩展安装标记'
}

check_disk() {
    DF=$(command -v df 2>/dev/null) || fail '缺少 df'
    size_kb=$(($(wc -c < "$PKG" | tr -d ' ') / 1024))
    free_kb=$("$DF" -Pk "$BASE" 2>/dev/null | awk 'NR==2 {print $4}')
    case "$free_kb" in '' | *[!0-9]*) fail '无法读取可用空间' ;; esac
    need_kb=$((size_kb * 3 + 16384))
    [ "$free_kb" -ge "$need_kb" ] || fail "可用空间不足（需要 ${need_kb} KiB）"
    base_dev=$("$DF" -Pk "$BASE" 2>/dev/null | awk 'NR==2 {print $1}')
    store_dev=$("$DF" -Pk "$STORE" 2>/dev/null | awk 'NR==2 {print $1}')
    [ -n "$base_dev" ] && [ "$base_dev" = "$store_dev" ] || fail '暂存目录与用户存储不在同一文件系统'
}

download_once() {
    "$CURL" -q --fail --silent --show-error --http1.1 --proto '=https' --max-redirs 0 \
        --connect-timeout 30 --max-time 1800 --max-filesize "$MAX_PACKAGE" \
        "$@" --continue-at - --output "$PKG" "$URL"
}

download_and_verify() {
    begin_step download
    CURL=$(command -v curl 2>/dev/null) || fail '缺少 curl'
    mkdir -p "$TXN" 2>/dev/null || fail '无法创建下载目录'
    attempt=0
    downloaded=0
    # 只替换连接目标，URL、Host、SNI 和证书校验仍使用原域名。
    if download_once --connect-to me.chen-cn.top:443:132.226.36.218:8443 \
        --connect-timeout 8 --speed-limit 1024 --speed-time 30; then
        downloaded=1
    else
        log "direct download failed (curl $?); falling back to existing entry"
    fi
    while [ "$downloaded" = 0 ] && [ "$attempt" -lt 3 ]; do
        if download_once; then
            downloaded=1
            break
        fi
        attempt=$((attempt + 1))
        log "download attempt $attempt failed; retrying from saved bytes"
    done
    [ "$downloaded" = 1 ] || fail '下载安装包失败，请检查 Wi-Fi 后重试'
    [ -f "$PKG" ] && [ ! -L "$PKG" ] || fail '下载的安装包无效'
    size=$(wc -c < "$PKG" | tr -d ' \t')
    case "$size" in '' | *[!0-9]*) fail '安装包大小无效' ;; esac
    [ "$size" -gt 0 ] && [ "$size" -le "$MAX_PACKAGE" ] || fail '安装包超出 64 MiB 上限'
    check_disk

    begin_step verify
    actual=$(sha256_file "$PKG") || fail '缺少 SHA-256 工具'
    [ "$actual" = "$SHA" ] || fail '安装包 SHA-256 校验失败'
    check_archive
    mkdir -p "$ROOT" 2>/dev/null || fail '无法创建解压目录'
    "$TAR" -xzf "$PKG" -C "$ROOT" 2>/dev/null || fail '解压安装包失败'
    if [ -n "$(find "$ROOT" -type l 2>/dev/null)" ]; then
        fail '解压后出现符号链接'
    fi
    if [ -n "$(find "$ROOT" ! -type f ! -type d 2>/dev/null)" ]; then
        fail '解压后出现特殊文件'
    fi
    find "$ROOT" -type f -exec chmod a-s {} \; 2>/dev/null || true
    check_stage
}

main() {
    [ $# -eq 3 ] || fail '用法：install.sh URL SHA256 VERSION'
    valid_version "$VER" || fail '服务端版本号无效'
    is_sha "$SHA" || fail '服务端 SHA-256 无效'
    allowed_url "$URL" || fail '下载地址不是允许的 HTTPS .tar.gz'

    EIPS=$(command -v eips 2>/dev/null) || EIPS=
    prepare_paths
    acquire_lock
    recover
    if [ "$RECOVERED" = 1 ]; then
        say '上次安装已完成，无需重复安装。'
        exit 0
    fi
    preflight_targets
    check_not_running refuse
    begin_step precheck
    download_and_verify

    begin_step guard
    unset ARISTOTLE_FORCE
    . "$SAPP/guard.sh"
    device_guard || fail '设备型号或固件不受支持，未改动任何文件'
    check_not_running

    begin_step swap
    write_journal staged || fail '无法写入安装记录'
    if ! mv "$SAPP" "$APP" 2>/dev/null; then
        if rollback_partial; then
            clean_transaction
            fail '无法写入应用目录，已回滚'
        fi
        fail '无法写入应用目录，回滚失败；安装记录与暂存文件保留'
    fi
    write_journal app-moved || {
        if rollback_partial; then
            clean_transaction
            fail '无法更新安装记录，已回滚'
        fi
        fail '无法更新安装记录，回滚失败；安装记录与暂存文件保留'
    }
    if [ "${MOYUE_INSTALL_TEST_CRASH:-}" = app ] && [ -n "$R" ]; then
        sync 2>/dev/null || true
        kill -KILL $$
    fi

    mkdir -p "$EXTBASE" 2>/dev/null || {
        if rollback_partial; then
            clean_transaction
            fail '无法创建 KUAL 扩展目录，已回滚'
        fi
        fail '无法创建 KUAL 扩展目录，回滚失败；安装记录与暂存文件保留'
    }
    if ! mv "$SEXT" "$EXT" 2>/dev/null; then
        if rollback_partial; then
            clean_transaction
            fail '无法写入扩展目录，已回滚'
        fi
        fail '无法写入扩展目录，回滚失败；安装记录与暂存文件保留'
    fi
    write_journal ext-moved || true
    sync 2>/dev/null || true

    if ! is_ours "$APP" "$VER" || ! is_ours "$EXT" "$VER" || \
        [ "$(sed -n '1p' "$APP/VERSION" 2>/dev/null | tr -d '\r')" != "$VER" ] || \
        [ "$(sed -n 's:.*<version>\([0-9][0-9.]*\)</version>.*:\1:p' "$EXT/config.xml" 2>/dev/null | head -n 1)" != "$VER" ]; then
        if rollback_partial; then
            clean_transaction
            fail '安装后校验失败，已回滚'
        fi
        fail '安装后校验失败，回滚失败；安装记录与暂存文件保留'
    fi

    write_state completed done || {
        log 'completed but state write failed; keeping journal'
        say '安装完成，但状态记录失败；下次进入会恢复记录。'
        exit 1
    }
    rm -rf "$TXN" 2>/dev/null || true
    sync 2>/dev/null || true
    rm -f "$JOURNAL" 2>/dev/null || true
    screen 42 "Install: done $VER"
    say "安装完成：${VER}。可从书库入口或 KUAL 启动墨阅。"
    exit 0
}

main "$@"
