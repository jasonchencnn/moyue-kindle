#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# KUAL > 墨阅 > 检查更新（OTA）: fetch the signed-in-transit release manifest
# from the project's HTTPS host, check the device and version, download the
# package, verify its size and SHA-256, unpack it beside the installed app, and
# swap it in only after the running app has stopped.
#
# The manifest is a strict MOYUE-OTA-1 text file (tools/ota-manifest.py); this
# script never sources or executes anything from the network.  It writes only
# under /mnt/us and /var/tmp.  A failed or interrupted swap is rolled back from
# $OTA/backup on the next run.
#
# AGENT_TEST_ROOT stands in for / in device/tests/ota.sh.  MOYUE_OTA_* and
# ARISTOTLE_APP_DIR/ARISTOTLE_EXT_DIR let that test use a local HTTP server and
# a fake drive; installed devices use the defaults below.
set -u
umask 022

HERE=$(cd "$(dirname "$0")" && pwd)
EXT=${ARISTOTLE_EXT_DIR:-$(dirname "$HERE")}
R=${AGENT_TEST_ROOT:-}
cd "${R:-/}" 2> /dev/null || cd /
# Pivot to a tmpfs copy so $EXT has no open script descriptor while we rename
# $EXT on Kindle's FUSE/vfat userstore (applies to KUAL `install` as well).
SELF_TMP=${MOYUE_OTA_SELF_TMP:-}
if [ "${MOYUE_OTA_PIVOTED:-0}" != 1 ] && [ -f "$0" ]; then
    _pivot_dir="$R/var/tmp/moyue-ota-self.$$"
    if mkdir -p "$_pivot_dir" 2> /dev/null && cp "$0" "$_pivot_dir/ota-update.sh" 2> /dev/null; then
        exec env ARISTOTLE_EXT_DIR="$EXT" MOYUE_OTA_PIVOTED=1 MOYUE_OTA_SELF_TMP="$_pivot_dir" \
            sh "$_pivot_dir/ota-update.sh" "$@"
    fi
    rm -rf "$_pivot_dir" 2> /dev/null || true
fi
APP=${ARISTOTLE_APP_DIR:-$R/mnt/us/aristotle}
OTA=${MOYUE_OTA_DIR:-$R/mnt/us/.aristotle/ota}
LOG=$R/mnt/us/.aristotle/ota.log
LOCK=$R/var/tmp/moyue-ota.lock
OTA_BASE=${MOYUE_OTA_BASE_URL:-https://me.chen-cn.top/moyue/ota/chan}
MAX_MANIFEST=4096
MAX_PACKAGE=67108864
TEST_HTTP=${MOYUE_OTA_TEST_HTTP:-0}
RECOVERY=$R/mnt/us/extensions/moyue-ota-recovery
TMP=
LOCK_HELD=0

# eips on the Kindle does not render UTF-8: keep stdout Chinese for KUAL's
# log, but send only ASCII to the screen.
say() {
    printf '%s\n' "$*"
    eips 0 1 "$(ascii_status "$*")                                        " > /dev/null 2>&1
}

say2() {
    printf '%s\n' "$*"
    eips 0 2 "$(ascii_detail "$*")                                        " > /dev/null 2>&1
}

ascii_status() {
    case "$1" in
    正在检查更新*) echo "Moyue OTA: checking for updates" ;;
    已是最新版本*) echo "Moyue OTA: already up to date" ;;
    已安装的*) echo "Moyue OTA: server is older; no downgrade" ;;
    发现*) echo "Moyue OTA: update found; downloading" ;;
    正在校验本地更新包*) echo "Moyue OTA: verifying handed-over package" ;;
    校验通过*) echo "Moyue OTA: SHA-256 OK; unpacking" ;;
    正在停止墨阅*) echo "Moyue OTA: stopping Moyue" ;;
    更新完成*) echo "Moyue OTA: update complete; tap Start Moyue" ;;
    更新失败*) echo "Moyue OTA: failed; see .aristotle/ota.log" ;;
    检测到上次*) echo "Moyue OTA: recovering an interrupted update" ;;
    已恢复*) echo "Moyue OTA: previous version restored" ;;
    上次更新已完成*) echo "Moyue OTA: cleanup after a completed update" ;;
    上次更新未改动*) echo "Moyue OTA: no application files changed" ;;
    无法恢复*) echo "Moyue OTA: restore failed; backup kept" ;;
    *回滚文件保留*) echo "Moyue OTA: restore failed; backup kept" ;;
    *) echo "Moyue OTA: working; see .aristotle/ota.log" ;;
    esac
}

ascii_detail() {
    case "$1" in
    详见*) echo "Details: .aristotle/ota.log" ;;
    点*启动*) echo "Tap Start Moyue to run the new version" ;;
    *) echo "Details: .aristotle/ota.log" ;;
    esac
}

log() {
    [ -d "$R/mnt/us/.aristotle" ] || mkdir -p "$R/mnt/us/.aristotle" 2> /dev/null || true
    printf '%s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2> /dev/null || echo unknown)" "$*" \
        >> "$LOG" 2> /dev/null || true
}

fail() {
    log "error: $*"
    say "更新失败：$*"
    say2 "详见 .aristotle/ota.log"
    exit 1
}

cleanup() {
    [ -z "$TMP" ] || rm -rf "$TMP"
    [ -z "$SELF_TMP" ] || rm -rf "$SELF_TMP"
    [ "$LOCK_HELD" = 1 ] && rmdir "$LOCK" 2> /dev/null || true
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

# Installed version, from the extension (written by every package) and the app's
# VERSION file (0.3.1 and later).  Both must agree when both exist.
installed_version() {
    iv_ext=
    iv_app=
    if [ -f "$EXT/config.xml" ] && [ ! -L "$EXT/config.xml" ]; then
        iv_ext=$(sed -n 's:.*<version>\([0-9][0-9.]*\)</version>.*:\1:p' "$EXT/config.xml" | head -n 1)
    fi
    if [ -f "$APP/VERSION" ] && [ ! -L "$APP/VERSION" ]; then
        iv_app=$(sed -n '1p' "$APP/VERSION")
    fi
    case "$iv_ext" in '' | *[!0-9.]*) iv_ext= ;; esac
    case "$iv_app" in '' | *[!0-9.]*) iv_app= ;; esac
    if [ -n "$iv_ext" ] && ! valid_version "$iv_ext"; then
        printf '%s\n' "invalid"
        return 0
    fi
    if [ -n "$iv_app" ] && ! valid_version "$iv_app"; then
        printf '%s\n' "invalid"
        return 0
    fi
    if [ -n "$iv_ext" ] && [ -n "$iv_app" ] && [ "$iv_ext" != "$iv_app" ]; then
        printf '%s\n' "inconsistent"
        return 0
    fi
    if [ -n "$iv_ext" ]; then printf '%s\n' "$iv_ext"; else printf '%s\n' "$iv_app"; fi
}

valid_version() {
    case "$1" in '' | *[!0-9.]* | .* | *. | *..*) return 1 ;; esac
    _old_ifs=$IFS
    IFS=.
    set -- $1
    IFS=$_old_ifs
    [ $# -eq 3 ] || return 1
    for _p in "$@"; do
        case "$_p" in 0 | [1-9]*) ;; *) return 1 ;; esac
        [ ${#_p} -le 3 ] || return 1
    done
    return 0
}

# version_gt A B: true when A is newer than B.
version_gt() {
    _va=$1 _vb=$2
    _old_ifs=$IFS
    IFS=.
    set -- $_va
    _a1=$1 _a2=$2 _a3=$3
    IFS=.
    set -- $_vb
    _b1=$1 _b2=$2 _b3=$3
    IFS=$_old_ifs
    [ "$_a1" -gt "$_b1" ] && return 0
    [ "$_a1" -lt "$_b1" ] && return 1
    [ "$_a2" -gt "$_b2" ] && return 0
    [ "$_a2" -lt "$_b2" ] && return 1
    [ "$_a3" -gt "$_b3" ]
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

# recover: undo a swap that was interrupted after the journal was written.
recover() {
    [ -f "$OTA/journal" ] || return 0
    journal_version=$(sed -n 's/^version=//p' "$OTA/journal" | head -n 1)
    journal_phase=$(sed -n 's/^phase=//p' "$OTA/journal" | head -n 1)
    current=$(installed_version)
    # A swap that reached both trees is complete even if cleanup was
    # interrupted; do not roll a good update back to the old version.
    if [ "$current" = "$journal_version" ] && \
        { [ "$journal_phase" = ext-swapped ] || [ "$journal_phase" = done ]; }; then
        say "上次更新已完成，正在清理。"
        rm -f "$OTA/journal"
        rm -rf "$OTA/backup"
        for _stale in "$OTA"/tmp.*; do
            [ -e "$_stale" ] || continue
            [ "$_stale" = "$TMP" ] || rm -rf "$_stale"
        done
        sync 2> /dev/null || true
        return 0
    fi
    say "检测到上次更新未完成，正在恢复..."
    if [ -d "$OTA/backup/app" ] || [ -d "$OTA/backup/ext" ]; then
        rollback_swap || fail "无法恢复旧版本，回滚文件仍保留在 $OTA/backup"
        say "已恢复更新前的版本。"
    else
        if [ -n "$journal_version" ] && [ "$current" = "$journal_version" ]; then
            say "上次更新已完成，正在清理。"
        else
            say "上次更新未改动应用。"
        fi
    fi
    rm -f "$OTA/journal"
    rm -rf "$OTA/backup"
    for _stale in "$OTA"/tmp.*; do
        [ -e "$_stale" ] || continue
        [ "$_stale" = "$TMP" ] || rm -rf "$_stale"
    done
    sync 2> /dev/null || true
}

rollback_swap() {
    if [ "${MOYUE_OTA_TEST_ROLLBACK_FAIL:-}" = 1 ] && [ -n "$R" ]; then
        return 1
    fi
    if [ -d "$OTA/backup/app" ]; then
        rm -rf "$APP"
        mv "$OTA/backup/app" "$APP" || return 1
    fi
    if [ -d "$OTA/backup/ext" ]; then
        rm -rf "$EXT"
        mv "$OTA/backup/ext" "$EXT" || return 1
    fi
    sync 2> /dev/null || true
    return 0
}

stop_app() {
    command -v pidof > /dev/null 2>&1 || fail "缺少 pidof，无法确认墨阅是否已停止"
    if pidof aristotle > /dev/null 2>&1; then
        say "正在停止墨阅..."
        if [ -f "$EXT/bin/aristotle-stop.sh" ] && [ ! -L "$EXT/bin/aristotle-stop.sh" ]; then
            sh "$EXT/bin/aristotle-stop.sh" > /dev/null 2>&1 || true
        else
            fail "找不到停止脚本，请先手动返回 Kindle 系统"
        fi
    fi
    _i=0
    while pidof aristotle > /dev/null 2>&1 && [ "$_i" -lt 25 ]; do
        sleep 1
        _i=$((_i + 1))
    done
    if pidof aristotle > /dev/null 2>&1; then
        fail "墨阅仍在运行，未改动任何文件"
    fi
}

# parse_manifest FILE: all fields must be present exactly once, with no unknown
# keys, no blank lines and no trailing carriage returns.
parse_manifest() {
    _file=$1
    _bytes=$(wc -c < "$_file" | tr -d " ") || fail "无法读取版本清单"
    [ "$_bytes" -le "$MAX_MANIFEST" ] || fail "版本清单过大"
    unset m_product m_channel m_version m_device m_firmware m_package m_sha256 m_size \
        m_source m_source_sha256 m_source_size
    _header=0
    while IFS= read -r _line; do
        case "$_line" in
        *"$(printf '\r')"*) fail "版本清单格式错误" ;;
        ' '*) fail "版本清单格式错误" ;;
        esac
        if [ "$_header" = 0 ]; then
            [ "$_line" = "MOYUE-OTA-1" ] || fail "不是 MOYUE OTA 版本清单"
            _header=1
            continue
        fi
        [ -n "$_line" ] || fail "版本清单有空行"
        case "$_line" in *=*) ;; *) fail "版本清单字段格式错误" ;; esac
        _key=${_line%%=*}
        _value=${_line#*=}
        [ -n "$_key" ] && [ -n "$_value" ] || fail "版本清单字段为空"
        case "$_key" in *[!a-z0-9-]*) fail "版本清单字段名错误" ;; esac
        case "$_value" in *[!A-Za-z0-9._:/@%+,=\&?-]*) fail "版本清单字段值错误" ;; esac
        case "$_key" in
        product) [ -z "${m_product+x}" ] || fail "版本清单字段重复"; m_product=$_value ;;
        channel) [ -z "${m_channel+x}" ] || fail "版本清单字段重复"; m_channel=$_value ;;
        version) [ -z "${m_version+x}" ] || fail "版本清单字段重复"; m_version=$_value ;;
        device) [ -z "${m_device+x}" ] || fail "版本清单字段重复"; m_device=$_value ;;
        firmware) [ -z "${m_firmware+x}" ] || fail "版本清单字段重复"; m_firmware=$_value ;;
        package) [ -z "${m_package+x}" ] || fail "版本清单字段重复"; m_package=$_value ;;
        sha256) [ -z "${m_sha256+x}" ] || fail "版本清单字段重复"; m_sha256=$_value ;;
        size) [ -z "${m_size+x}" ] || fail "版本清单字段重复"; m_size=$_value ;;
        source) [ -z "${m_source+x}" ] || fail "版本清单字段重复"; m_source=$_value ;;
        source-sha256) [ -z "${m_source_sha256+x}" ] || fail "版本清单字段重复"; m_source_sha256=$_value ;;
        source-size) [ -z "${m_source_size+x}" ] || fail "版本清单字段重复"; m_source_size=$_value ;;
        *) fail "版本清单有未知字段：$_key" ;;
        esac
    done < "$_file"
    [ "$_header" = 1 ] || fail "版本清单为空"
    [ -n "${m_product:-}" ] || fail "版本清单缺少 product"
    [ -n "${m_channel:-}" ] || fail "版本清单缺少 channel"
    [ -n "${m_version:-}" ] || fail "版本清单缺少 version"
    [ -n "${m_device:-}" ] || fail "版本清单缺少 device"
    [ -n "${m_firmware:-}" ] || fail "版本清单缺少 firmware"
    [ -n "${m_package:-}" ] || fail "版本清单缺少 package"
    [ -n "${m_sha256:-}" ] || fail "版本清单缺少 sha256"
    [ -n "${m_size:-}" ] || fail "版本清单缺少 size"
    [ -n "${m_source:-}" ] || fail "版本清单缺少 source"
    [ -n "${m_source_sha256:-}" ] || fail "版本清单缺少 source-sha256"
    [ -n "${m_source_size:-}" ] || fail "版本清单缺少 source-size"

    [ "$m_product" = moyue-preview ] || fail "清单不是墨阅预览版"
    case "$m_channel" in
    '' | *[!A-Za-z0-9.-]*) fail "清单通道名无效" ;;
    esac
    valid_version "$m_version" || fail "清单版本号无效"
    case "$m_sha256" in *[!0-9a-f]* | '') fail "清单 SHA-256 无效" ;; esac
    [ ${#m_sha256} -eq 64 ] || fail "清单 SHA-256 无效"
    case "$m_source_sha256" in *[!0-9a-f]* | '') fail "清单源码 SHA-256 无效" ;; esac
    [ ${#m_source_sha256} -eq 64 ] || fail "清单源码 SHA-256 无效"
    case "$m_size" in '' | *[!0-9]*) fail "清单包大小无效" ;; esac
    case "$m_source_size" in '' | *[!0-9]*) fail "清单源码大小无效" ;; esac
    [ "$m_size" -gt 0 ] && [ "$m_size" -le "$MAX_PACKAGE" ] || fail "清单包大小超出范围"
    case "$m_package" in "$ALLOWED_PREFIX"*) ;; *) fail "包地址不是允许的 HTTPS 路径" ;; esac
    case "$m_source" in "$ALLOWED_PREFIX"*) ;; *) fail "源码地址不是允许的 HTTPS 路径" ;; esac
    case "$m_package" in *' '*) fail "包地址无效" ;; esac
    case "$m_source" in *' '*) fail "源码地址无效" ;; esac
}

# parse_handover FILE: parse the handover.conf written by the in-app OTA
# downloader (crates/aristotle/src/ota.rs).  All 7 fields must appear once,
# with no unknown keys, no blank lines and no trailing carriage returns.
parse_handover() {
    _file=$1
    _bytes=$(wc -c < "$_file" | tr -d " ") || fail "无法读取更新交接文件"
    [ "$_bytes" -le "$MAX_MANIFEST" ] || fail "更新交接文件过大"
    unset m_version m_device m_channel m_firmware m_package m_sha256 m_size
    while IFS= read -r _line; do
        case "$_line" in
        *"$(printf '\r')"*) fail "交接文件格式错误" ;;
        ' '*) fail "交接文件格式错误" ;;
        esac
        [ -n "$_line" ] || fail "交接文件有空行"
        case "$_line" in *=*) ;; *) fail "交接文件字段格式错误" ;; esac
        _key=${_line%%=*}
        _value=${_line#*=}
        [ -n "$_key" ] && [ -n "$_value" ] || fail "交接文件字段为空"
        case "$_key" in *[!a-z0-9-]*) fail "交接文件字段名错误" ;; esac
        case "$_value" in *[!A-Za-z0-9._:/@%+,=\&?-]*) fail "交接文件字段值错误" ;; esac
        case "$_key" in
        version) [ -z "${m_version+x}" ] || fail "交接文件字段重复"; m_version=$_value ;;
        device) [ -z "${m_device+x}" ] || fail "交接文件字段重复"; m_device=$_value ;;
        channel) [ -z "${m_channel+x}" ] || fail "交接文件字段重复"; m_channel=$_value ;;
        firmware) [ -z "${m_firmware+x}" ] || fail "交接文件字段重复"; m_firmware=$_value ;;
        package) [ -z "${m_package+x}" ] || fail "交接文件字段重复"; m_package=$_value ;;
        sha256) [ -z "${m_sha256+x}" ] || fail "交接文件字段重复"; m_sha256=$_value ;;
        size) [ -z "${m_size+x}" ] || fail "交接文件字段重复"; m_size=$_value ;;
        *) fail "交接文件有未知字段：$_key" ;;
        esac
    done < "$_file"
    [ -n "${m_version:-}" ] || fail "交接文件缺少 version"
    [ -n "${m_device:-}" ] || fail "交接文件缺少 device"
    [ -n "${m_channel:-}" ] || fail "交接文件缺少 channel"
    [ -n "${m_firmware:-}" ] || fail "交接文件缺少 firmware"
    [ -n "${m_package:-}" ] || fail "交接文件缺少 package"
    [ -n "${m_sha256:-}" ] || fail "交接文件缺少 sha256"
    [ -n "${m_size:-}" ] || fail "交接文件缺少 size"

    case "$m_channel" in
    '' | *[!A-Za-z0-9.-]*) fail "交接通道名无效" ;;
    esac
    valid_version "$m_version" || fail "交接版本号无效"
    case "$m_sha256" in *[!0-9a-f]* | '') fail "交接 SHA-256 无效" ;; esac
    [ ${#m_sha256} -eq 64 ] || fail "交接 SHA-256 无效"
    case "$m_size" in '' | *[!0-9]*) fail "交接包大小无效" ;; esac
    [ "$m_size" -gt 0 ] && [ "$m_size" -le "$MAX_PACKAGE" ] || fail "交接包大小超出范围"
    _ota_norm=${OTA%/}
    case "$m_package" in
    "$_ota_norm"/*) ;;
    *) fail "交接更新包路径不在更新目录内" ;;
    esac
    _rel=${m_package#"$_ota_norm"/}
    [ -n "$_rel" ] || fail "交接更新包路径无效"
    case "/$_rel/" in
    *"/../"* | *"/./"* | *"//"*) fail "交接更新包路径包含路径穿越" ;;
    esac
}

# validate_name NAME: only the two trees of a package, no traversal and no
# absolute paths.
validate_name() {
    _name=$1
    case "$_name" in */) _name=${_name%/} ;; esac
    [ -n "$_name" ] || fail "压缩包包含空路径"
    case "$_name" in /* | *\\*) fail "压缩包包含绝对路径：$1" ;; esac
    case "/$_name/" in
    *"/../"* | *"/./"*) fail "压缩包包含路径穿越：$1" ;;
    esac
    case "$_name" in *"//"*) fail "压缩包包含路径穿越：$1" ;; esac
    case "$1" in
    aristotle/* | extensions/aristotle/* | extensions | extensions/) ;;
    *) fail "压缩包包含不允许的路径：$1" ;;
    esac
}

# check_archive FILE: list before extraction.  Only regular files and
# directories are accepted; links and device nodes are refused.
check_archive() {
    _pkg=$1
    tar -tzf "$_pkg" > "$TMP/names" 2> /dev/null || fail "压缩包无法读取"
    [ -s "$TMP/names" ] || fail "压缩包为空"
    while IFS= read -r _name; do
        validate_name "$_name"
    done < "$TMP/names"
    tar -tvzf "$_pkg" > "$TMP/types" 2> /dev/null || fail "压缩包无法读取"
    while IFS= read -r _line; do
        case "$_line" in
        -* | d*) ;;
        *) fail "压缩包包含符号链接或特殊文件" ;;
        esac
        case "$_line" in *" -> "*) fail "压缩包包含符号链接" ;; esac
    done < "$TMP/types"
}

check_stage() {
    _s=$1
    [ -x "$_s/aristotle/aristotle" ] || fail "包内缺少应用可执行文件"
    for _f in run-aristotle.sh takeover.sh open-kual.sh SOURCE.txt VERSION data; do
        [ -e "$_s/aristotle/$_f" ] || fail "包内缺少 aristotle/$_f"
    done
    [ -f "$_s/extensions/aristotle/config.xml" ] || fail "包内缺少扩展版本文件"
    [ -f "$_s/extensions/aristotle/menu.json" ] || fail "包内缺少扩展菜单"
    [ -f "$_s/extensions/aristotle/bin/ota-update.sh" ] || fail "包内缺少 OTA 脚本"
    [ -f "$_s/extensions/aristotle/diagnostics/diagnostics.sh" ] || fail "包内缺少诊断脚本"
    _app_version=$(sed -n '1p' "$_s/aristotle/VERSION")
    [ "$_app_version" = "$m_version" ] || fail "包的应用版本与清单不一致"
    _ext_version=$(sed -n 's:.*<version>\([0-9][0-9.]*\)</version>.*:\1:p' \
        "$_s/extensions/aristotle/config.xml" | head -n 1)
    [ "$_ext_version" = "$m_version" ] || fail "包的扩展版本与清单不一致"
    [ -z "$(find "$_s" -type l 2> /dev/null)" ] || fail "解压后出现符号链接"
}

# install_recovery: a permanent, tiny KUAL extension outside the tree the OTA
# swaps.  If power is lost between renaming the old extension away and moving
# the new one in, this entry still starts and restores the backup.
install_recovery() {
    [ ! -L "$RECOVERY" ] || fail "恢复入口是符号链接"
    mkdir -p "$RECOVERY/bin" || fail "无法创建恢复入口"
    cp "$HERE/ota-update.sh" "$RECOVERY/bin/ota-update.sh" || fail "无法写入恢复入口"
    cat > "$RECOVERY/config.xml" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<extension>
  <information>
    <name>墨阅更新恢复</name>
    <version>0.3.1</version>
    <author>墨阅</author>
    <id>moyue-ota-recovery</id>
  </information>
  <menus><menu type="json" dynamic="true">menu.json</menu></menus>
</extension>
EOF
    cat > "$RECOVERY/menu.json" << 'EOF'
{
  "items": [
    {
      "name": "墨阅更新恢复",
      "items": [
        {
          "name": "恢复上次未完成的更新",
          "action": "sh ./bin/recover.sh",
          "exitmenu": false,
          "status": false
        }
      ]
    }
  ]
}
EOF
    cat > "$RECOVERY/bin/recover.sh" << 'EOF'
#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -u
R=${AGENT_TEST_ROOT:-}
EXT=${ARISTOTLE_EXT_DIR:-$R/mnt/us/extensions/aristotle}
exec env ARISTOTLE_EXT_DIR="$EXT" \
    ARISTOTLE_APP_DIR="${ARISTOTLE_APP_DIR:-$R/mnt/us/aristotle}" \
    MOYUE_OTA_DIR="${MOYUE_OTA_DIR:-$R/mnt/us/.aristotle/ota}" \
    sh "$(dirname "$0")/ota-update.sh" recover
EOF
    chmod 755 "$RECOVERY/bin/ota-update.sh" "$RECOVERY/bin/recover.sh"
}

case "${1:-}" in
install | check | handover | recover) ;;
*) fail "用法：ota-update.sh install | handover | recover" ;;
esac

# One update at a time.  The lock is on the tmpfs, so a reboot clears an
# interrupted run's lock.
case "$APP" in '' | / | */.. | */../*) fail "应用目录配置无效" ;; esac
case "$EXT" in '' | / | */.. | */../*) fail "扩展目录配置无效" ;; esac
[ "$APP" != "$EXT" ] || fail "应用与扩展目录相同"
case "${OTA%/}/" in
"${APP%/}"/* | "${EXT%/}"/*) fail "更新目录不能在应用或扩展目录内" ;;
esac
mkdir -p "$R/var/tmp" 2> /dev/null || true
mkdir "$LOCK" 2> /dev/null || fail "另一个更新正在进行"
LOCK_HELD=1
mkdir -p "$OTA" 2> /dev/null || fail "无法创建更新目录"
[ ! -L "$OTA" ] || fail "更新目录是符号链接"
TMP=$OTA/tmp.$$
mkdir -p "$TMP" || fail "无法创建临时目录"

recover
if [ "${1:-}" = recover ]; then
    say "恢复检查完成"
    exit 0
fi

installed=$(installed_version)
if [ -z "$installed" ]; then
    fail "无法确定已安装版本，请先用 USB 安装 0.3.1"
fi
if [ "$installed" = "inconsistent" ]; then
    fail "应用与扩展版本不一致，请先用 USB 重新安装"
fi
if [ "$installed" = "invalid" ]; then
    fail "已安装版本格式无效，请先用 USB 重新安装"
fi

[ -f "$EXT/guard.sh" ] && [ ! -L "$EXT/guard.sh" ] \
    || fail "缺少设备守卫 guard.sh，请重新安装墨阅"
. "$EXT/guard.sh"
_code=$(device_code)
[ -n "$_code" ] || fail "无法从序列号读取设备型号"
_fw=$(firmware_version)
[ -n "$_fw" ] || fail "无法读取固件版本"

if [ "${1:-}" = handover ]; then
    HANDOVER=$OTA/handover.conf
    [ -f "$HANDOVER" ] && [ ! -L "$HANDOVER" ] || fail "未找到更新交接文件"
    mv "$HANDOVER" "$TMP/handover.conf" || fail "无法读取更新交接文件"
    parse_handover "$TMP/handover.conf"

    [ "$m_device" = "$_code" ] \
        || fail "此更新不是给设备 ${_code} 的（清单设备：${m_device}）"
    case "$m_channel" in
    "$_code"-*) ;;
    *) fail "交接通道与设备不符（通道：${m_channel}）" ;;
    esac
    case "$_fw" in
    "$m_firmware" | "$m_firmware".*) ;;
    *) fail "固件 $_fw 与交接要求的 $m_firmware 不符" ;;
    esac

    if ! version_gt "$m_version" "$installed"; then
        rm -f "$m_package" 2> /dev/null || true
        if [ "$m_version" = "$installed" ]; then
            say "已是最新版本 $installed"
        else
            say "已安装的 ${installed} 比服务器上的 ${m_version} 新，不降级"
            say2 "未改动任何文件"
        fi
        exit 0
    fi

    say "正在校验本地更新包..."
    _pkg=$m_package
else
    say "正在检查更新..."
    command -v curl > /dev/null 2>&1 || fail "缺少 curl，无法联网更新"
    proto='=https'
    [ "$TEST_HTTP" = 1 ] && [ -n "$R" ] && proto='=http,https'

    # fetch_manifest: try the channels this device could have, most specific
    # first.  A manifest for an exact firmware wins; shorter firmware prefixes
    # cover the entries that publish a firmware range (the PW4 5.13 one).  A 404
    # moves to the next candidate, any other failure is a real error.
    # Returns 0 with CHANNEL/ALLOWED_PREFIX set, 2 when no channel exists.
    fetch_manifest() {
        _cand=$_fw
        while :; do
            _ch="$_code-$_cand"
            _url=${MOYUE_OTA_MANIFEST_URL:-$OTA_BASE/$_ch/manifest.txt}
            _prefix=${MOYUE_OTA_ALLOWED_PREFIX:-$OTA_BASE/$_ch/}
            curl -q --fail --silent --show-error --proto "$proto" --max-redirs 0 \
                --connect-timeout 10 --max-time 30 --max-filesize "$MAX_MANIFEST" \
                --output "$TMP/manifest.txt" "$_url"
            _rc=$?
            if [ "$_rc" = 0 ]; then
                CHANNEL=$_ch
                MANIFEST_URL=$_url
                ALLOWED_PREFIX=$_prefix
                return 0
            fi
            # 22 is curl's --fail HTTP error: this channel does not exist.
            [ "$_rc" = 22 ] || fail "无法下载版本清单，请检查 Wi-Fi"
            case "$_cand" in
            *.*) _cand=${_cand%.*} ;;
            *) return 2 ;;
            esac
        done
    }

    if [ -n "${MOYUE_OTA_CHANNEL:-}" ]; then
        # An explicit channel (tests, operators) pins this device's manifest.
        CHANNEL=$MOYUE_OTA_CHANNEL
        MANIFEST_URL=${MOYUE_OTA_MANIFEST_URL:-$OTA_BASE/$CHANNEL/manifest.txt}
        ALLOWED_PREFIX=${MOYUE_OTA_ALLOWED_PREFIX:-$OTA_BASE/$CHANNEL/}
        curl -q --fail --silent --show-error --proto "$proto" --max-redirs 0 \
            --connect-timeout 10 --max-time 30 --max-filesize "$MAX_MANIFEST" \
            --output "$TMP/manifest.txt" "$MANIFEST_URL" \
            || fail "无法下载版本清单，请检查 Wi-Fi"
    else
        fetch_manifest
    fi
    case $? in
    0) ;;
    2)
        say "此设备暂未提供在线更新（${_code} / ${_fw}）"
        say2 "未改动任何文件"
        exit 0
        ;;
    *) exit 1 ;;
    esac
    [ -f "$TMP/manifest.txt" ] && [ ! -L "$TMP/manifest.txt" ] || fail "版本清单无效"
    parse_manifest "$TMP/manifest.txt"

    [ "$m_device" = "$_code" ] \
        || fail "此更新不是给设备 ${_code} 的（清单设备：${m_device}）"
    [ "$m_channel" = "$CHANNEL" ] \
        || fail "清单通道与设备不符（清单：${m_channel}）"
    case "$_fw" in
    "$m_firmware" | "$m_firmware".*) ;;
    *) fail "固件 $_fw 与清单要求的 $m_firmware 不符" ;;
    esac

    if ! version_gt "$m_version" "$installed"; then
        if [ "$m_version" = "$installed" ]; then
            say "已是最新版本 $installed"
        else
            say "已安装的 ${installed} 比服务器上的 ${m_version} 新，不降级"
            say2 "未改动任何文件"
        fi
        exit 0
    fi

    say "发现 ${m_version}（当前 ${installed}），正在下载..."
    _pkg=$TMP/package.tar.gz
    curl -q --fail --silent --show-error --proto "$proto" --max-redirs 0 \
        --connect-timeout 10 --max-time 300 --max-filesize "$MAX_PACKAGE" \
        --output "$_pkg" "$m_package" \
        || fail "下载更新包失败，请检查 Wi-Fi 后重试"
fi
[ -f "$_pkg" ] && [ ! -L "$_pkg" ] || fail "更新包无效"
_actual=$(wc -c < "$_pkg" | tr -d " ")
[ "$_actual" = "$m_size" ] || fail "更新包大小不符（${_actual}，应为 ${m_size}）"
_digest=$(sha256_file "$_pkg") || fail "缺少 SHA-256 工具（sha256sum 或 openssl）"
[ "$_digest" = "$m_sha256" ] || fail "更新包 SHA-256 校验失败"
say "校验通过，正在解压..."
check_archive "$_pkg"
mkdir -p "$TMP/root" || fail "无法创建解压目录"
tar -xzf "$_pkg" -C "$TMP/root" || fail "解压更新包失败"
check_stage "$TMP/root"

# Preserve extension-side user files that are not part of a release.  The
# agent key stays at the extension root; DIAG-001 credentials stay in the
# diagnostics directory where the collector reads them (the extension-root
# copies from the first integration are migrated there).
for _f in authorized_keys; do
    if [ -f "$EXT/$_f" ] && [ ! -L "$EXT/$_f" ]; then
        cp -p "$EXT/$_f" "$TMP/root/extensions/aristotle/$_f" 2> /dev/null \
            || cp "$EXT/$_f" "$TMP/root/extensions/aristotle/$_f" \
            || fail "无法保留 $_f"
    fi
done
for _f in endpoint.txt token.txt; do
    _src=
    if [ -f "$EXT/diagnostics/$_f" ] && [ ! -L "$EXT/diagnostics/$_f" ]; then
        _src=$EXT/diagnostics/$_f
    elif [ -f "$EXT/$_f" ] && [ ! -L "$EXT/$_f" ]; then
        _src=$EXT/$_f
    elif [ -f "$R/mnt/us/extensions/moyue-diagnostics/$_f" ] && \
        [ ! -L "$R/mnt/us/extensions/moyue-diagnostics/$_f" ]; then
        _src=$R/mnt/us/extensions/moyue-diagnostics/$_f
    fi
    if [ -n "$_src" ]; then
        cp -p "$_src" "$TMP/root/extensions/aristotle/diagnostics/$_f" 2> /dev/null \
            || cp "$_src" "$TMP/root/extensions/aristotle/diagnostics/$_f" \
            || fail "无法保留 $_f"
    fi
done

[ -d "$APP" ] || fail "未找到 /mnt/us/aristotle，请先用 USB 安装"
[ ! -L "$APP" ] || fail "应用目录是符号链接"
[ -d "$EXT" ] || fail "未找到扩展目录"
[ ! -L "$EXT" ] || fail "扩展目录是符号链接"

install_recovery
stop_app

rm -rf "$OTA/backup"
mkdir -p "$OTA/backup" || fail "无法创建回滚目录"
printf 'version=%s\nphase=installing\n' "$m_version" > "$OTA/journal" || fail "无法写入更新记录"
sync 2> /dev/null || true

_ok=0
if mv "$APP" "$OTA/backup/app"; then
    printf 'version=%s\nphase=app-swapped\n' "$m_version" > "$OTA/journal"
    if mv "$TMP/root/aristotle" "$APP"; then
        if [ "${MOYUE_OTA_TEST_CRASH:-}" = app ] && [ -n "$R" ]; then
            kill -KILL $$
        fi
        if [ -d "$EXT" ]; then
            printf 'version=%s\nphase=ext-moving\n' "$m_version" > "$OTA/journal"
            sync 2> /dev/null || true
        fi
        if [ -d "$EXT" ] && mv "$EXT" "$OTA/backup/ext"; then
            printf 'version=%s\nphase=ext-moved\n' "$m_version" > "$OTA/journal"
            if [ "${MOYUE_OTA_TEST_CRASH:-}" = ext ] && [ -n "$R" ]; then
                kill -KILL $$
            fi
            if mv "$TMP/root/extensions/aristotle" "$EXT"; then
                printf 'version=%s\nphase=ext-swapped\n' "$m_version" > "$OTA/journal"
                _ok=1
            fi
        fi
    fi
fi

if [ "$_ok" != 1 ]; then
    log "install failed after app/extension swap; rolling back"
    if ! rollback_swap; then
        log "rollback failed; keeping $OTA/backup and journal"
        fail "安装失败，旧版本恢复失败；回滚文件保留在 $OTA/backup"
    fi
    rm -f "$OTA/journal"
    rm -rf "$OTA/backup"
    sync 2> /dev/null || true
    fail "安装失败，已恢复旧版本"
fi

_new=$(installed_version)
[ "$_new" = "$m_version" ] || {
    log "installed version check failed; rolling back"
    if ! rollback_swap; then
        log "rollback failed; keeping $OTA/backup and journal"
        fail "安装后版本校验失败，旧版本恢复失败；回滚文件保留在 $OTA/backup"
    fi
    rm -f "$OTA/journal"
    rm -rf "$OTA/backup"
    sync 2> /dev/null || true
    fail "安装后版本校验失败，已恢复旧版本"
}

printf 'version=%s\nphase=done\n' "$m_version" > "$OTA/journal" || true
sync 2> /dev/null || true
rm -rf "$OTA/backup" "$OTA/journal"
if [ "${1:-}" = handover ]; then
    rm -f "$_pkg" 2> /dev/null || true
fi
sync 2> /dev/null || true
log "updated $installed -> $m_version"
say "更新完成：${m_version}"
say2 "点『启动墨阅』即可使用"
exit 0
