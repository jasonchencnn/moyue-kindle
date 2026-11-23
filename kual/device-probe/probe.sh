#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# MOYUE-DIAGNOSTICS-1 device probe: local read-only collection plus an
# explicit upload choice. Usage: probe.sh local | probe.sh upload.
#
# The report is built in a staging file beside report.txt and renamed into
# place only after every section has been written. A failed run therefore
# leaves the previous report untouched. Only the device code is extracted
# from /proc/usid; the serial number is never written to the report.
#
# AGENT_TEST_ROOT stands in for / on the host. In that mode uname, date,
# fbset, the tool probes and the extension credentials are resolved below
# the test root only, so a test can never report the host kernel or host tools.
set -eu
umask 077

R=${AGENT_TEST_ROOT:-}
MODE=${1:-local}
case "$MODE" in
local | upload) ;;
*) printf '%s\n' 'Probe failed: use local or upload' >&2; exit 1 ;;
esac

if [ -n "$R" ]; then
    BASE=$R/mnt/us/extensions/moyue-device-probe
else
    BASE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
fi
STORE=$R/mnt/us
OUTDIR=$STORE/moyue-device-probe
OUT=$OUTDIR/report.txt
LOCKBASE=$R/var/tmp
LOCK=$LOCKBASE/moyue-device-probe.lock
TMP=$OUTDIR/.report.tmp.$$
STAGE=$OUTDIR/.report.stage.$$
RESPONSE=$OUTDIR/.upload.tmp.$$
# The service accepts at most 32 KiB per report (MAX_REQUEST in
# scripts/device-service.py). 30 KiB leaves room for the truncation marker.
MAX_TOTAL=30720
BODY_MAX=$((MAX_TOTAL - 128))
ITEM_MAX=384
MODULE_MAX=40
DMESG_MAX=40

say() {
    printf '%s\n' "$*"
    # Tests must not call a host screen tool; only the real device gets eips.
    if [ -z "$R" ] && [ "${MOYUE_DEVICE_CENTER:-0}" != 1 ]; then
        eips 0 2 "$*" 2>/dev/null || true
    fi
}

fail() {
    say "Probe failed: $*"
    exit 1
}

cleanup() {
    rm -f "$TMP" "$STAGE" "$RESPONSE"
    rmdir "$LOCK" 2>/dev/null || true
}

# Resolve a command without consulting the host PATH in test mode.
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

# Omit overlong fields so a byte limit cannot split a UTF-8 character.
clip_line() {
    limit=${1:-$ITEM_MAX}
    value=$(LC_ALL=C head -c "$((limit + 1))")
    bytes=$(printf '%s' "$value" | wc -c)
    if [ "$bytes" -gt "$limit" ]; then
        printf '[value exceeds %s bytes]' "$limit"
    else
        printf '%s' "$value" | LC_ALL=C tr '\r\n\t' '   ' |
            LC_ALL=C tr -d '\033\177' | LC_ALL=C sed 's/[[:space:]]*$//'
    fi
}

read_text() {
    path=$1
    label=$2
    limit=${3:-$ITEM_MAX}
    if [ -L "$path" ]; then
        printf '%s: [symlink ignored]\n' "$label"
    elif [ -f "$path" ] && [ -r "$path" ]; then
        value=$(clip_line "$limit" < "$path")
        [ -n "$value" ] || value='[empty]'
        printf '%s: %s\n' "$label" "$value"
    else
        printf '%s: [missing]\n' "$label"
    fi
}

# Strip the shapes that would carry a credential, a full serial or a book
# path out of kernel and driver text. Nothing here loads or writes anything.
redact_line() {
    LC_ALL=C sed \
        -e 's/[0-9a-fA-F]\{32,\}/[long-hex-redacted]/g' \
        -e 's/\([0-9a-fA-F]\{2\}:\)\{5\}[0-9a-fA-F]\{2\}/[mac-redacted]/g' \
        -e 's/[B9G][A-Z0-9]\{13,23\}/[serial-redacted]/g' \
        -e 's/[A-Za-z0-9._%+-]\{1,\}@[A-Za-z0-9.-]\{1,\}\.[A-Za-z]\{2,\}/[email-redacted]/g' \
        -e 's#/[^ "]*\.\(epub\|txt\|pdf\|mobi\|azw3\|fb2\)\b#[book-path-redacted]#g'
}

# Print the last component of a symlink target: the driver or module name.
link_basename() {
    target=$(readlink "$1" 2>/dev/null) || target=
    [ -n "$target" ] || target='[unreadable link]'
    printf '%s\n' "${target##*/}"
}

DATE_BIN=$(find_tool date) || DATE_BIN=
UNAME_BIN=$(find_tool uname) || UNAME_BIN=
FBSET_BIN=$(find_tool fbset) || FBSET_BIN=

if [ -L "$OUTDIR" ]; then
    fail 'output folder is a symlink'
fi
if [ -e "$OUTDIR" ] && [ ! -d "$OUTDIR" ]; then
    fail 'output path is not a directory'
fi
mkdir -p "$OUTDIR" 2>/dev/null || fail 'cannot create output folder'
if [ ! -d "$OUTDIR" ] || [ -L "$OUTDIR" ]; then
    fail 'output folder is not a safe directory'
fi
if [ -L "$OUT" ]; then
    fail 'report.txt is a symlink'
fi
if [ -e "$OUT" ] && [ ! -f "$OUT" ]; then
    fail 'report.txt is not a regular file'
fi

if [ -L "$LOCKBASE" ]; then
    fail 'lock folder is a symlink'
fi
mkdir -p "$LOCKBASE" 2>/dev/null || fail 'cannot create lock folder'
if [ ! -d "$LOCKBASE" ] || [ -L "$LOCKBASE" ]; then
    fail 'lock folder is not a safe directory'
fi
mkdir "$LOCK" 2>/dev/null || fail 'another probe is running'
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

( set -C; : > "$STAGE" ) 2>/dev/null || fail 'cannot create staging file'
chmod 600 "$STAGE" 2>/dev/null || fail 'cannot set staging permissions'

{
    printf '%s\n' 'MOYUE-DIAGNOSTICS-1'
    printf '%s\n' 'report-kind: device-probe-v1'

    now=''
    if [ -n "$DATE_BIN" ]; then
        now=$("$DATE_BIN" -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null | clip_line 64) || now=
    fi
    [ -n "$now" ] || now='[unavailable: date command missing]'
    printf 'UTC: %s\n' "$now"

    kernel='[unavailable: uname command missing]'
    if [ -n "$UNAME_BIN" ]; then
        kernel=$("$UNAME_BIN" -srm 2>/dev/null | clip_line 192) || kernel=
        [ -n "$kernel" ] || kernel='[unavailable: uname returned nothing]'
    fi
    printf 'uname: %s\n' "$kernel"

    printf '\n--- usid ---\n'
    serial_file=$R/proc/usid
    if [ -L "$serial_file" ]; then
        # New Kindle firmware aliases this proc entry to the IDME serial node.
        case $(readlink "$serial_file" 2>/dev/null) in
        /proc/idme/serial | idme/serial)
            serial_file=$R/proc/idme/serial
            [ ! -L "$R/proc/idme" ] && [ ! -L "$serial_file" ] || serial_file=
            ;;
        *) serial_file= ;;
        esac
    fi
    if [ -z "$serial_file" ]; then
        printf 'device code: [unavailable: /proc/usid is a symlink]\n'
    elif [ ! -f "$serial_file" ]; then
        printf 'device code: [missing /proc/usid]\n'
    else
        serial=$(head -c 96 "$serial_file" 2>/dev/null | tr -d '\r' | head -n 1)
        case "$serial" in
        '')
            printf 'device code: [unavailable: /proc/usid is empty]\n'
            ;;
        B* | 9*)
            code=$(printf '%s' "$serial" | cut -c3-4)
            if [ "${#code}" -eq 2 ]; then
                printf 'device code: %s (old scheme)\n' "$code"
            else
                printf 'device code: [unavailable: serial too short]\n'
            fi
            ;;
        *)
            code=$(printf '%s' "$serial" | cut -c4-6)
            if [ "${#code}" -eq 3 ]; then
                printf 'device code: %s (new scheme)\n' "$code"
            else
                printf 'device code: [unavailable: serial too short]\n'
            fi
            ;;
        esac
        unset serial code
    fi

    printf '\n--- firmware ---\n'
    read_text "$R/etc/prettyversion.txt" 'prettyversion' 192
    read_text "$R/etc/version.txt" 'version.txt' 192

    printf '\n--- cpu (whitelist) ---\n'
    if [ -L "$R/proc/cpuinfo" ]; then
        printf '[symlink ignored]\n'
    elif [ -f "$R/proc/cpuinfo" ]; then
        awk -F: '
            /^(Hardware|Processor|model name|Features|CPU architecture|CPU implementer|CPU part)[ \t]*:/ {
                if (++n > 24) exit
                key = $1
                val = $2
                sub(/^[ \t]+/, "", key)
                sub(/[ \t]+$/, "", key)
                sub(/^[ \t]+/, "", val)
                sub(/[ \t]+$/, "", val)
                gsub(/[[:cntrl:]]/, " ", val)
                if (length(val) > 320) val = "[long value omitted]"
                print key ": " val
            }' "$R/proc/cpuinfo"
    else
        printf '[missing /proc/cpuinfo]\n'
    fi

    printf '\n--- memory ---\n'
    if [ -f "$R/proc/meminfo" ] && [ ! -L "$R/proc/meminfo" ]; then
        mem=$(awk '
            /^MemTotal:/ { print "MemTotal: " $2 " " $3 }
            /^MemAvailable:/ { available = 1; print "MemAvailable: " $2 " " $3 }
            /^MemFree:/ { free = $2 " " $3 }
            END { if (!available && free != "") print "MemAvailable (MemFree fallback): " free }
        ' "$R/proc/meminfo")
        if [ -n "$mem" ]; then
            printf '%s\n' "$mem"
        else
            printf '[no MemTotal/MemAvailable fields]\n'
        fi
    else
        printf '[missing /proc/meminfo]\n'
    fi

    printf '\n--- loader/libc (names only, not executed) ---\n'
    for pattern in 'ld-*' 'libc.so*' 'libc-*.so' 'arm-linux-gnueabihf/libc.so*'; do
        found=0
        for path in "$R"/lib/$pattern; do
            if [ -e "$path" ] || [ -L "$path" ]; then
                found=1
                rel=${path#"$R"}
                if [ -L "$path" ]; then
                    kind='symlink'
                elif [ -f "$path" ]; then
                    kind='file'
                else
                    kind='other'
                fi
                printf '%s (%s)\n' "$rel" "$kind"
            fi
        done
        [ "$found" = 1 ] || printf 'missing /lib/%s\n' "$pattern"
    done

    printf '\n--- tools (existence only) ---\n'
    for name in fbset eips lipc-get-prop lipc-set-prop fbgrab; do
        if tool=$(find_tool "$name"); then
            printf '%s: present\n' "$name"
        else
            printf '%s: missing\n' "$name"
        fi
    done

    printf '\n--- framebuffer sysfs ---\n'
    fb_count=0
    for fb in "$R"/sys/class/graphics/fb*; do
        [ -d "$fb" ] || continue
        fb_count=$((fb_count + 1))
        if [ "$fb_count" -gt 4 ]; then
            printf '[more framebuffer nodes omitted]\n'
            break
        fi
        printf '[%s]\n' "${fb##*/}"
        for attr in name virtual_size bits_per_pixel stride rotate grayscale modes; do
            read_text "$fb/$attr" "  $attr"
        done
    done
    [ "$fb_count" -gt 0 ] || printf '[no framebuffer sysfs node]\n'

    printf '\n--- fbset (optional, read-only) ---\n'
    if [ -n "$FBSET_BIN" ]; then
        if fbset_out=$("$FBSET_BIN" -i 2>/dev/null); then
            fbset_out=$(printf '%s' "$fbset_out" | clip_line 1024)
            [ -n "$fbset_out" ] || fbset_out='[no output]'
            printf '%s\n' "$fbset_out"
        else
            printf '[fbset -i failed; ioctl-mode details unknown]\n'
        fi
    else
        printf '[not run: fbset missing; ioctl-mode details unknown; sysfs alone cannot confirm EPDC ABI or waveform]\n'
    fi

    printf '\n--- input devices (I/N/H/B only) ---\n'
    if [ -f "$R/proc/bus/input/devices" ] && [ ! -L "$R/proc/bus/input/devices" ]; then
        awk '
            /^[INHB]:/ {
                line = $0
                gsub(/\r/, "", line)
                gsub(/[[:cntrl:]]/, " ", line)
                if (length(line) > 320) line = "[long input line omitted]"
                print line
                if (++n >= 40) exit
            }' "$R/proc/bus/input/devices"
    else
        printf '[missing /proc/bus/input/devices]\n'
    fi

    printf '\n--- sysfs input (name/capabilities only) ---\n'
    input_count=0
    for ev in "$R"/sys/class/input/event*; do
        [ -d "$ev" ] || continue
        input_count=$((input_count + 1))
        if [ "$input_count" -gt 8 ]; then
            printf '[more event nodes omitted]\n'
            break
        fi
        printf '[%s]\n' "${ev##*/}"
        read_text "$ev/device/name" '  name' 256
        for cap in ev abs key; do
            read_text "$ev/device/capabilities/$cap" "  capabilities/$cap" 256
        done
    done
    [ "$input_count" -gt 0 ] || printf '[no sysfs input event node]\n'

    printf '\n--- backlight ---\n'
    backlight_count=0
    for bl in "$R"/sys/class/backlight/*; do
        [ -d "$bl" ] || continue
        backlight_count=$((backlight_count + 1))
        if [ "$backlight_count" -gt 8 ]; then
            printf '[more backlight nodes omitted]\n'
            break
        fi
        printf '[%s]\n' "${bl##*/}"
        for attr in type max_brightness brightness; do
            read_text "$bl/$attr" "  $attr" 128
        done
    done
    [ "$backlight_count" -gt 0 ] || printf '[no backlight node]\n'

    printf '\n--- power supply ---\n'
    power_count=0
    for ps in "$R"/sys/class/power_supply/*; do
        [ -d "$ps" ] || continue
        power_count=$((power_count + 1))
        if [ "$power_count" -gt 8 ]; then
            printf '[more power supply nodes omitted]\n'
            break
        fi
        printf '[%s]\n' "${ps##*/}"
        for attr in type status capacity; do
            read_text "$ps/$attr" "  $attr" 128
        done
    done
    [ "$power_count" -gt 0 ] || printf '[no power supply node]\n'

    printf '\n--- framebuffer driver (sysfs links) ---\n'
    driver_count=0
    for fb in "$R"/sys/class/graphics/fb*; do
        [ -d "$fb" ] || continue
        driver_count=$((driver_count + 1))
        [ "$driver_count" -le 4 ] || break
        printf '[%s]\n' "${fb##*/}"
        if [ -L "$fb/device/driver" ]; then
            printf '  driver link: %s\n' "$(link_basename "$fb/device/driver")"
            if [ -L "$fb/device/driver/module" ]; then
                printf '  driver module link: %s\n' "$(link_basename "$fb/device/driver/module")"
            else
                printf '  driver module link: [not exposed: driver is built into the kernel]\n'
            fi
        else
            printf '  driver link: [not exposed]\n'
            printf '  driver module link: [not exposed: driver is built into the kernel]\n'
        fi
        read_text "$fb/device/modalias" '  modalias' 192
    done
    [ "$driver_count" -gt 0 ] || printf '[no framebuffer sysfs node]\n'

    printf '\n--- kernel modules (proc) ---\n'
    if [ -f "$R/proc/modules" ] && [ ! -L "$R/proc/modules" ]; then
        module_hits=$(LC_ALL=C grep -Eic 'mxcfb|epdc|framebuffer|wacom|touch|input|evdev|ads7846|goodix|cyttsp|pen|eink|panel|lcd|dsi|snvs' "$R/proc/modules" 2>/dev/null || true)
        case "$module_hits" in
        '' | *[!0-9]*) module_hits=0 ;;
        esac
        if [ "$module_hits" -gt 0 ]; then
            LC_ALL=C grep -Ei 'mxcfb|epdc|framebuffer|wacom|touch|input|evdev|ads7846|goodix|cyttsp|pen|eink|panel|lcd|dsi|snvs' "$R/proc/modules" 2>/dev/null |
                head -n "$MODULE_MAX" |
                redact_line |
                LC_ALL=C awk '{ if (length($0) > 192) print "[value exceeds 192 bytes]"; else print }' || true
            [ "$module_hits" -le "$MODULE_MAX" ] || printf '[more module entries omitted: %s matched]\n' "$module_hits"
        else
            printf '[no display/input/touch module listed in /proc/modules]\n'
        fi
    else
        printf '[missing /proc/modules]\n'
    fi

    printf '\n--- kernel modules (sysfs) ---\n'
    module_count=0
    for mod in "$R"/sys/module/*; do
        [ -d "$mod" ] || continue
        name=${mod##*/}
        case "$name" in
        *mxcfb* | *epdc* | *fb* | *wacom* | *touch* | *input* | *evdev* | *ads7846* | *goodix* | *cyttsp* | *pen* | *eink* | *panel* | *lcd* | *dsi* | *snvs* | *imx*) ;;
        *) continue ;;
        esac
        module_count=$((module_count + 1))
        [ "$module_count" -le "$MODULE_MAX" ] || break
        state='[missing]'
        if [ -f "$mod/initstate" ] && [ ! -L "$mod/initstate" ]; then
            state=$(clip_line 64 < "$mod/initstate")
        fi
        if [ -f "$R/proc/modules" ] && LC_ALL=C grep -q "^$name " "$R/proc/modules" 2>/dev/null; then
            printf '%s (loaded module, initstate: %s)\n' "$name" "$state"
        else
            printf '%s (built into the kernel: no /proc/modules entry, initstate: %s)\n' "$name" "$state"
        fi
    done
    [ "$module_count" -gt 0 ] || printf '[no matching module node under /sys/module]\n'

    printf '\n--- device tree ---\n'
    for prop in model compatible; do
        path=$R/proc/device-tree/$prop
        if [ -L "$path" ]; then
            printf '%s: [symlink ignored]\n' "$prop"
        elif [ -f "$path" ]; then
            # The property holds NUL separated strings; convert the separators
            # and drop the remaining control bytes before printing.
            value=$(LC_ALL=C tr '\000' '\n' < "$path" 2>/dev/null |
                LC_ALL=C tr -d '\000-\010\013\014\016-\037\177' |
                grep -v '^$' | head -n 8 |
                LC_ALL=C awk 'BEGIN { ORS = "" } NR > 1 { printf ", " } { if (length($0) > 192) print "[value exceeds 192 bytes]"; else print $0 }')
            if [ -n "$value" ]; then
                printf '%s: %s\n' "$prop" "$value"
            else
                printf '%s: [empty]\n' "$prop"
            fi
        else
            printf '%s: [missing]\n' "$prop"
        fi
    done

    printf '\n--- dmesg (display/input/touch only) ---\n'
    DMESG_BIN=$(find_tool dmesg) || DMESG_BIN=
    if [ -z "$DMESG_BIN" ]; then
        printf '[not run: dmesg missing; /dev/kmsg is not read because it blocks]\n'
    elif ! "$DMESG_BIN" >/dev/null 2>&1; then
        printf '[dmesg failed: not permitted on this device]\n'
    else
        dmesg_lines=$("$DMESG_BIN" 2>/dev/null | tail -n 400 |
            LC_ALL=C grep -Ei 'mxcfb|epdc|framebuffer|fb0|eink|e-ink|panel|waveform|wacom|touch|touchscreen|input|evdev|ads7846|goodix|cyttsp|pen|snvs' |
            head -n "$DMESG_MAX" || true)
        if [ -n "$dmesg_lines" ]; then
            printf '%s\n' "$dmesg_lines" |
                redact_line |
                LC_ALL=C awk '{ if (length($0) > 192) print "[value exceeds 192 bytes]"; else print }'
            printf '[only the last 400 kernel messages were scanned]\n'
        else
            printf '[no display/input/touch line in the last 400 kernel messages]\n'
        fi
    fi

    printf '\n--- koreader runtime ---\n'
    KO_DIR=$R/mnt/us/koreader
    if [ -d "$KO_DIR" ] && [ ! -L "$KO_DIR" ]; then
        printf 'runtime: present\n'
        version_found=''
        for candidate in git-rev version version.log; do
            [ -f "$KO_DIR/$candidate" ] && [ ! -L "$KO_DIR/$candidate" ] || continue
            value=$(clip_line 96 < "$KO_DIR/$candidate")
            [ -n "$value" ] || continue
            version_found=1
            printf 'version (%s): %s\n' "$candidate" "$value"
            break
        done
        [ -n "$version_found" ] || printf 'version: [not found: no git-rev or version file]\n'
        for name in koreader.sh luajit reader.lua; do
            if [ -f "$KO_DIR/$name" ]; then
                printf '%s: present\n' "$name"
            else
                printf '%s: missing\n' "$name"
            fi
        done
        if [ -d "$KO_DIR/plugins/readest.koplugin" ]; then
            printf 'readest plugin: present\n'
        else
            printf 'readest plugin: not installed\n'
        fi
        if [ -f "$KO_DIR/crash.log" ]; then
            printf 'crash log: present (content not collected: it may name books)\n'
        else
            printf 'crash log: none\n'
        fi
    else
        printf 'runtime: [missing /mnt/us/koreader]\n'
    fi

    printf '\n--- boundaries ---\n'
    printf 'collection only: no /dev/fb write, no module load, no sysfs write, no event stream, no network\n'
    printf 'kernel evidence only: no driver load or unload was attempted, built-in drivers are reported as such\n'
} >> "$STAGE"

size=$(wc -c < "$STAGE" | tr -d ' \t')
case "$size" in
'' | *[!0-9]*) fail 'cannot size report' ;;
esac

( set -C; : > "$TMP" ) 2>/dev/null || fail 'cannot create temporary report'
chmod 600 "$TMP" 2>/dev/null || fail 'cannot set report permissions'
if [ "$size" -gt "$BODY_MAX" ]; then
    # Drop the last line, which may end in a partial UTF-8 character.
    LC_ALL=C head -c "$BODY_MAX" "$STAGE" | LC_ALL=C sed '$d' > "$TMP" || fail 'cannot truncate report'
    printf '\n[report truncated: total limit %s bytes]\n' "$MAX_TOTAL" >> "$TMP"
else
    cat "$STAGE" > "$TMP" || fail 'cannot stage report'
fi
[ -s "$TMP" ] || fail 'empty report'
chmod 600 "$TMP" 2>/dev/null || fail 'cannot set report permissions'
mv -f "$TMP" "$OUT" || fail 'cannot replace report'

say 'Saved: /mnt/us/moyue-device-probe/report.txt'

if [ "$MODE" = local ]; then
    say 'Local only: no network access was made.'
    exit 0
fi

[ -f "$BASE/endpoint.txt" ] && [ ! -L "$BASE/endpoint.txt" ] ||
    fail 'upload configuration missing; report.txt kept'
[ -f "$BASE/token.txt" ] && [ ! -L "$BASE/token.txt" ] ||
    fail 'upload configuration missing; report.txt kept'
ENDPOINT=$(head -c 512 "$BASE/endpoint.txt" | head -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
TOKEN=$(head -c 256 "$BASE/token.txt" | head -n 1 | tr -d '\r' | sed 's/[[:space:]]*$//')
case "$ENDPOINT" in
https://*) ;;
*) fail 'HTTPS endpoint required; report.txt kept' ;;
esac
case "$ENDPOINT" in
*[!a-zA-Z0-9:/._-]*) fail 'invalid endpoint; report.txt kept' ;;
esac
case "$TOKEN" in
'' | *[!a-zA-Z0-9_-]*) fail 'invalid upload token; report.txt kept' ;;
esac
[ "${#TOKEN}" -ge 32 ] || fail 'invalid upload token; report.txt kept'
CURL_BIN=$(find_tool curl) || CURL_BIN=
[ -n "$CURL_BIN" ] || fail 'curl missing; report.txt kept'

( set -C; : > "$RESPONSE" ) 2>/dev/null || fail 'cannot create upload response file'
chmod 600 "$RESPONSE" 2>/dev/null || fail 'cannot secure upload response file'
say 'Uploading report (up to 45 seconds)...'
# The token travels on stdin, never in argv. -q ignores user curl config;
# no -L and --max-redirs 0 refuse redirects; --proto allows HTTPS only.
if ! HTTP_CODE=$(printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" |
    "$CURL_BIN" -q --config - --silent --show-error --fail --connect-timeout 10 --max-time 45 \
        --max-redirs 0 --proto '=https' --header 'Content-Type: text/plain' \
        --write-out '%{http_code}' --data-binary "@$OUT" --output "$RESPONSE" "$ENDPOINT"); then
    fail 'upload failed; report.txt kept. Check Wi-Fi or send the report manually'
fi
[ "$HTTP_CODE" = 200 ] || fail 'unexpected server response; report.txt kept'
RECEIPT=$(cat "$RESPONSE")
case "$RECEIPT" in
'' | *[!0-9a-f]*) fail 'unexpected server response; report.txt kept' ;;
esac
[ "${#RECEIPT}" -eq 16 ] || fail 'unexpected server response; report.txt kept'
say "Uploaded. Report ID: $RECEIPT"
