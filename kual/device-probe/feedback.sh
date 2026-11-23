#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# 墨阅适配中心反馈客户端（Kindle 端）
#
# 用法：
#   feedback.sh link         取得反馈链接（令牌只写本机私有文件）
#   feedback.sh send 分类    提交一次分类反馈
#   feedback.sh logs         上传脱敏后的应用与中心日志
#
# 约定：
#   - 任务凭据复用 /mnt/us/moyue-device-probe/task.txt。密钥只经 curl 标准输入
#     传递，不出现在命令行参数、URL、结果文件和日志里。
#   - 反馈令牌只写进本机私有文件 feedback-link.txt（0600），供扫码使用；结果
#     文件里只有状态与回执号，没有令牌，也没有密钥。
#   - 日志只取白名单来源的有限尾部，上传前按行脱敏，不含书正文、账号字段和
#     完整序列号。
#   - 接口地址固定；只有测试根允许用 MOYUE_DEVICE_API 覆盖。
#   - 响应只按固定 JSON 形状逐字段校验，不 source、不 eval、不执行。
set -eu
umask 077

R=${AGENT_TEST_ROOT:-}
MODE=${1:-}
CATEGORY=${2:-}
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUTDIR=$R/mnt/us/moyue-device-probe
TASK=$OUTDIR/task.txt
REPORT=$OUTDIR/report.txt
LINKFILE=$OUTDIR/feedback-link.txt
RESULT=$OUTDIR/feedback-result.txt
LOCKBASE=$R/var/tmp
LOCK=$LOCKBASE/moyue-feedback.lock
API=https://me.chen-cn.top/moyue/device
PAGE=https://me.chen-cn.top/moyue/feedback.html
LOG_LIMIT=$((240 * 1024))
LOG_TAIL=400
LINE_MAX=512

case "$MODE" in
link | send | logs) ;;
*)
    printf '%s\n' '用法：feedback.sh link | feedback.sh send 分类 | feedback.sh logs' >&2
    exit 1
    ;;
esac

if [ -n "$R" ] && [ -n "${MOYUE_DEVICE_API:-}" ]; then
    API=$MOYUE_DEVICE_API
fi
case "$API" in
https://*) ;;
*) printf '%s\n' '反馈服务地址必须使用 HTTPS' >&2; exit 1 ;;
esac
case "$API" in
*[!a-zA-Z0-9:/._-]*) printf '%s\n' '反馈服务地址含非法字符' >&2; exit 1 ;;
esac

is_hex() {
    case "$1" in
    '' | *[!0-9a-f]*) return 1 ;;
    esac
    [ "${#1}" -eq "$2" ]
}

say() {
    printf '%s\n' "$*"
}

fail() {
    say "反馈失败：$*"
    save_result failed "$1"
    exit 1
}

# --- 私有文件 ---------------------------------------------------------

write_private() {
    target=$1
    content=$2
    [ ! -L "$target" ] || {
        printf '%s\n' "$target 是符号链接，拒绝写入" >&2
        return 1
    }
    [ ! -e "$target" ] || [ -f "$target" ] || {
        printf '%s\n' "$target 不是普通文件" >&2
        return 1
    }
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

# 结果文件只记状态，不含令牌和密钥。
save_result() {
    state=$1
    note=$2
    note=$(printf '%s' "$note" | LC_ALL=C tr '\r\n' '  ')
    write_private "$RESULT" "MOYUE-FEEDBACK-RESULT-1
kind=$MODE
state=$state
receipt=${RECEIPT:-}
message=$note" || true
}

# --- 任务凭据 ---------------------------------------------------------

TASK_ID=''
TASK_KEY=''

load_task() {
    [ -f "$TASK" ] && [ ! -L "$TASK" ] || return 1
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

# --- 反馈链接 ---------------------------------------------------------

# 只接受固定域名下、片段为 <32hex>.<64hex> 的链接。
parse_link() {
    fragment=${1##*\#}
    case "$1" in
    "$PAGE"\#*) ;;
    *) return 1 ;;
    esac
    id=${fragment%%.*}
    token=${fragment#*.}
    [ "$id" != "$fragment" ] || return 1
    [ "$token" != "$fragment" ] || return 1
    is_hex "$id" 32 || return 1
    is_hex "$token" 64 || return 1
    LINK_ID=$id
    LINK_TOKEN=$token
    return 0
}

read_link() {
    [ -f "$LINKFILE" ] && [ ! -L "$LINKFILE" ] || return 1
    header=$(sed -n '1p' "$LINKFILE" | tr -d '\r')
    [ "$header" = 'MOYUE-FEEDBACK-LINK-1' ] || return 1
    url_line=$(sed -n '2p' "$LINKFILE" | tr -d '\r')
    case "$url_line" in url=*) ;; *) return 1 ;; esac
    LINK_URL=${url_line#url=}
    parse_link "$LINK_URL" || return 1
    [ "$LINK_ID" = "$TASK_ID" ] || return 1
    return 0
}

# --- JSON 读取 --------------------------------------------------------
# 只接受服务端约定的固定形状，绝不 source、绝不 eval。

json_url() {
    file=$1
    [ -f "$file" ] || return 1
    lines=$(wc -l < "$file" | tr -d ' \t')
    [ "$lines" -le 1 ] || return 1
    line=$(head -n 1 "$file" | tr -d '\r')
    value=$(printf '%s' "$line" |
        sed -n 's/^{"url"[[:space:]]*:[[:space:]]*"\([^"]*\)"}$/\1/p')
    [ -n "$value" ] || return 1
    printf '%s\n' "$value"
}

json_receipt() {
    file=$1
    [ -f "$file" ] || return 1
    lines=$(wc -l < "$file" | tr -d ' \t')
    [ "$lines" -le 1 ] || return 1
    line=$(head -n 1 "$file" | tr -d '\r')
    value=$(printf '%s' "$line" |
        sed -n 's/^{"ok"[[:space:]]*:[[:space:]]*true,[[:space:]]*"receipt"[[:space:]]*:[[:space:]]*"\([^"]*\)"}$/\1/p')
    [ -n "$value" ] || value=$(printf '%s' "$line" |
        sed -n 's/^{"receipt"[[:space:]]*:[[:space:]]*"\([^"]*\)",[[:space:]]*"ok"[[:space:]]*:[[:space:]]*true}$/\1/p')
    [ -n "$value" ] || return 1
    is_hex "$value" 32 || return 1
    printf '%s\n' "$value"
}

# --- 网络 -------------------------------------------------------------

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

# 密钥经 curl 标准输入传递；URL 里没有密钥，也没有令牌。
request() {
    method=$1
    url=$2
    content_type=$3
    body=${4:-}
    CURL_BIN=$(find_tool curl) || {
        fail '设备里没有 curl'
    }
    resp=$OUTDIR/.feedback-resp.$$
    ( set -C; : > "$resp" ) 2>/dev/null || fail '无法创建响应文件'
    chmod 600 "$resp" 2>/dev/null || true
    set -- -q --config - --silent --show-error --connect-timeout 15 --max-time 60 \
        --max-redirs 0 --proto '=https' -X "$method" \
        --header "Content-Type: $content_type" --write-out '%{http_code}' \
        --output "$resp"
    if [ -n "$body" ]; then
        set -- "$@" --data-binary "@$body"
    else
        set -- "$@" --data-binary ''
    fi
    set -- "$@" "$url"
    code=$({
        if [ "$url" != "$API/feedback" ]; then
            printf 'header = "Authorization: Bearer %s"\n' "$TASK_KEY"
        fi
    } | "$CURL_BIN" "$@") || {
        rm -f "$resp"
        fail '网络请求失败，请检查 Wi-Fi 后重试'
    }
    RESPONSE=$resp
    HTTP_CODE=$code
    return 0
}

# --- 日志 -------------------------------------------------------------

# 按行脱敏：命中敏感字段的行整行替换，其余行去掉长十六进制、MAC、序列号和书路径。
redact_stream() {
    while IFS= read -r line; do
        if printf '%s' "$line" |
            LC_ALL=C grep -Eiq '(authorization|bearer|password|token|api[_-]?key|account|email|serial|usid|ssid|psk)|(^|[^A-Za-z])key[[:space:]]*[=:]'; then
            printf '%s\n' '[private field redacted]'
            continue
        fi
        printf '%s\n' "$line" | LC_ALL=C sed \
            -e 's/[0-9a-fA-F]\{32,\}/[long-hex-redacted]/g' \
            -e 's/\([0-9a-fA-F]\{2\}:\)\{5\}[0-9a-fA-F]\{2\}/[mac-redacted]/g' \
            -e 's/[B9G][A-Z0-9]\{13,23\}/[serial-redacted]/g' \
            -e 's#/[^ "]*\.epub#[book-path-redacted]#g' \
            -e 's#/[^ "]*\.txt#[book-path-redacted]#g' \
            -e 's#/[^ "]*\.pdf#[book-path-redacted]#g' \
            -e 's#/[^ "]*\.mobi#[book-path-redacted]#g' \
            -e 's#/[^ "]*\.azw3#[book-path-redacted]#g' \
            -e 's#/[^ "]*\.fb2#[book-path-redacted]#g'
    done
}

# 白名单日志来源：墨阅应用日志、扩展日志与适配中心自身日志。
log_sources() {
    printf '%s\n' \
        "$R/mnt/us/.aristotle/app.log" \
        "$R/mnt/us/.aristotle/app.log.1" \
        "$R/mnt/us/.aristotle/ui.log" \
        "$R/mnt/us/.aristotle/ui.log.1" \
        "$R/var/tmp/aristotle.log" \
        "$OUTDIR/task.log" \
        "$OUTDIR/install.log" \
        "$OUTDIR/center-launch.log" \
        "$OUTDIR/center-runtime.log" \
        "$OUTDIR/center-action.log"
}

collect_logs() {
    target=$1
    : > "$target"
    chmod 600 "$target" 2>/dev/null || true
    used=$(wc -c < "$target" | tr -d ' \t')
    for source in $(log_sources); do
        [ -f "$source" ] && [ ! -L "$source" ] || continue
        printf '===== %s =====\n' "${source#$R/}" >> "$target"
        tail -n "$LOG_TAIL" "$source" 2>/dev/null |
            redact_stream |
            LC_ALL=C awk -v max="$LINE_MAX" '{ if (length($0) > max) print "[value exceeds " max " bytes]"; else print }' >> "$target"
    done
    used=$(wc -c < "$target" | tr -d ' \t')
    if [ "$used" -gt "$LOG_LIMIT" ]; then
        LC_ALL=C head -c "$LOG_LIMIT" "$target" | LC_ALL=C sed '$d' > "$target.trimmed"
        mv -f "$target.trimmed" "$target"
        printf '%s\n' '[日志已截断：只上传前 240 KiB]' >> "$target"
    fi
}

# --- 反馈消息 ---------------------------------------------------------

# 设备端不提供自由输入：反馈正文只由白名单的设备属性拼出。
build_message() {
    code=$(sed -n 's/^device code: \([0-9A-Za-z]*\) .*/\1/p' "$REPORT" 2>/dev/null | head -n 1)
    firmware=$(sed -n 's/^prettyversion: Kindle \([0-9.]*\).*/\1/p' "$REPORT" 2>/dev/null | head -n 1)
    message='Kindle 适配中心试装验证反馈'
    [ -n "$code" ] && message="$message
设备代码：$code"
    [ -n "$firmware" ] && message="$message
固件：$firmware"
    source=$R/mnt/us/aristotle/SOURCE.txt
    if [ -f "$source" ] && [ ! -L "$source" ]; then
        build=$(head -c 2048 "$source" | sed -n 's/^Build \([a-zA-Z0-9._-]*\)$/\1/p' | head -n 1)
        [ -z "$build" ] || message="$message
已安装构建：$build"
    fi
    printf '%s' "$message"
}

json_escape() {
    printf '%s' "$1" | LC_ALL=C sed \
        -e 's/\\/\\\\/g' \
        -e 's/"/\\"/g' \
        -e 's/\t/\\t/g' |
        LC_ALL=C awk 'BEGIN { ORS = "" } NR > 1 { printf "\\n" } { print }'
}

# --- 主流程 -----------------------------------------------------------

mkdir -p "$OUTDIR" 2>/dev/null || fail '无法创建输出目录'
[ ! -L "$OUTDIR" ] && [ -d "$OUTDIR" ] || fail '输出目录不是安全目录'
chmod 700 "$OUTDIR" 2>/dev/null || true
mkdir -p "$LOCKBASE" 2>/dev/null || fail '无法创建锁目录'
if ! mkdir "$LOCK" 2>/dev/null; then
    owner=$(cat "$LOCK/pid" 2>/dev/null || true)
    case "$owner" in
    '' | *[!0-9]*) fail '另一个反馈操作正在进行' ;;
    esac
    kill -0 "$owner" 2>/dev/null && fail '另一个反馈操作正在进行'
    rm -f "$LOCK/pid"
    rmdir "$LOCK" && mkdir "$LOCK" || fail '无法取得反馈锁'
fi
printf '%s\n' "$$" > "$LOCK/pid"
trap 'rm -f "$LOCK/pid"; rmdir "$LOCK" 2>/dev/null || :' EXIT
trap 'exit 1' HUP INT TERM

RECEIPT=''
RESPONSE=''
HTTP_CODE=''

case "$MODE" in
link)
    load_task || fail '没有找到设备任务凭据，请先上传设备信息'
    request POST "$API/jobs/$TASK_ID/feedback-link" 'text/plain; charset=utf-8' ''
    [ "$HTTP_CODE" = 200 ] || {
        rm -f "$RESPONSE"
        fail '服务器没有返回反馈链接（HTTP '"$HTTP_CODE"'）'
    }
    url=$(json_url "$RESPONSE") || {
        rm -f "$RESPONSE"
        fail '反馈链接响应格式不对'
    }
    rm -f "$RESPONSE"
    parse_link "$url" || fail '反馈链接的域名或令牌格式不对'
    [ "$LINK_ID" = "$TASK_ID" ] || fail '反馈链接与当前任务不一致'
    write_private "$LINKFILE" "MOYUE-FEEDBACK-LINK-1
url=$url" || fail '无法保存反馈链接'
    save_result completed '反馈链接已取得'
    say '反馈链接已取得，可以用手机扫码填写详细反馈。'
    ;;
send)
    case "$CATEGORY" in
    works | white_screen | touch | layout | launch | other) ;;
    *) fail '反馈分类不在约定范围' ;;
    esac
    load_task || fail '没有找到设备任务凭据，请先上传设备信息'
    read_link || fail '还没有反馈链接，请先取得反馈链接'
    message=$(build_message)
    body=$OUTDIR/.feedback-body.$$
    ( set -C; : > "$body" ) 2>/dev/null || fail '无法创建请求正文'
    chmod 600 "$body" 2>/dev/null || true
    printf '{"id":"%s","token":"%s","category":"%s","message":"%s"}' \
        "$LINK_ID" "$LINK_TOKEN" "$CATEGORY" "$(json_escape "$message")" > "$body"
    payload=$(wc -c < "$body" | tr -d ' \t')
    [ "$payload" -le 16384 ] || {
        rm -f "$body"
        fail '反馈正文超过接口上限'
    }
    request POST "$API/feedback" 'application/json' "$body"
    rm -f "$body"
    [ "$HTTP_CODE" = 201 ] || {
        rm -f "$RESPONSE"
        [ "$HTTP_CODE" != 401 ] || rm -f "$LINKFILE"
        fail '服务器没有接受这次反馈（HTTP '"$HTTP_CODE"'）'
    }
    RECEIPT=$(json_receipt "$RESPONSE") || {
        rm -f "$RESPONSE"
        fail '反馈回执格式不对'
    }
    rm -f "$RESPONSE"
    save_result completed '反馈已提交'
    say "反馈已提交，回执 $RECEIPT"
    ;;
logs)
    load_task || fail '没有找到设备任务凭据，请先上传设备信息'
    body=$OUTDIR/.feedback-logs.$$
    ( set -C; : > "$body" ) 2>/dev/null || fail '无法创建日志正文'
    collect_logs "$body"
    size=$(wc -c < "$body" | tr -d ' \t')
    [ "$size" -gt 0 ] || {
        rm -f "$body"
        fail '没有可上传的日志'
    }
    [ "$size" -le 262144 ] || {
        rm -f "$body"
        fail '日志超过接口上限'
    }
    request POST "$API/jobs/$TASK_ID/logs" 'text/plain; charset=utf-8' "$body"
    rm -f "$body"
    [ "$HTTP_CODE" = 201 ] || {
        rm -f "$RESPONSE"
        fail '服务器没有接受这些日志（HTTP '"$HTTP_CODE"'）'
    }
    RECEIPT=$(json_receipt "$RESPONSE") || {
        rm -f "$RESPONSE"
        fail '日志回执格式不对'
    }
    rm -f "$RESPONSE"
    save_result completed "日志已上传（${size} 字节）"
    say "日志已上传，回执 $RECEIPT"
    ;;
esac
