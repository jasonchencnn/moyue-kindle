#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
umask 077
BASE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
R=${AGENT_TEST_ROOT:-}
OUT=$R/mnt/us/moyue-diagnostics
# Support credentials are deliberately outside the release tree: an OTA or USB
# update replaces extensions/aristotle, but must not delete support access.
# MOYUE_DIAG_CONFIG_DIR may point anywhere for a test or a support build.
CONFIG=${MOYUE_DIAG_CONFIG_DIR:-$R/mnt/us/.aristotle/diagnostics}

say() {
    printf '%s\n' "$*"
    eips 0 2 "$*" 2>/dev/null || true
}
fail() {
    say "Diagnostics failed: $*"
    exit 1
}

case "${1:-}" in collect|upload) ;; *) fail 'use collect or upload' ;; esac
[ ! -L "$OUT" ] || fail 'output folder is a symlink'
mkdir -p "$OUT"
mkdir -p "$R/var/tmp"
# tmpfs clears an interrupted operation's lock on the next reboot.
LOCK=$R/var/tmp/moyue-diagnostics.lock
mkdir "$LOCK" 2>/dev/null || fail 'another collection/upload is running'
trap 'rm -f "$LOCK/report.txt" "$LOCK/response.txt" "$LOCK/receipt.txt"; rmdir "$LOCK"' EXIT
trap 'exit 1' HUP INT TERM

collect() {
    {
        printf 'MOYUE-DIAGNOSTICS-1\n'
        date -u '+UTC %Y-%m-%dT%H:%M:%SZ'
        printf '\n--- kernel ---\n'
        uname -srm
        printf '\n--- memory ---\n'
        grep -E '^(MemTotal|MemFree|MemAvailable|SwapTotal|SwapFree):' "$R/proc/meminfo" || true
        printf '\n--- storage ---\n'
        df -Pk "$R/mnt/us" 2>/dev/null || true
        # Exact paths only. Never walk the library or read account/settings files.
        for path in /etc/version.txt /mnt/us/aristotle/SOURCE.txt \
            /var/tmp/aristotle.log /mnt/us/.aristotle/app.log \
            /mnt/us/.aristotle/app.log.1; do
            printf '\n--- %s ---\n' "$path"
            if [ -f "$R$path" ] && [ ! -L "$R$path" ]; then
                size=$(wc -c < "$R$path")
                tail -c 131072 "$R$path" | awk -v size="$size" '
                    NR == 1 && size > 131072 { next }
                    length($0) > 2048 { print "[long line omitted]"; next }
                    tolower($0) ~ /bearer|token|password|authorization|cookie|secret|api.key/ {
                        print "[credential-related line omitted]"; next
                    }
                    { print }
                '
            else
                printf '[missing or symlink]\n'
            fi
        done
        printf '\n--- installed runtime resources ---\n'
        for path in /mnt/us/aristotle/aristotle /mnt/us/aristotle/data/fonts \
            /mnt/us/aristotle/data/css /mnt/us/aristotle/data/hyphenation-patterns; do
            if [ -e "$R$path" ]; then
                printf 'present %s\n' "$path"
            else
                printf 'missing %s\n' "$path"
            fi
        done
    } > "$LOCK/report.txt"
    mv -f "$LOCK/report.txt" "$OUT/report.txt"
    say 'Saved: moyue-diagnostics/report.txt'
}

if [ "$1" = collect ]; then
    collect
    exit 0
fi

[ -f "$OUT/report.txt" ] && [ ! -L "$OUT/report.txt" ] || fail 'collect logs first'
if [ -f "$CONFIG/endpoint.txt" ] && [ -f "$CONFIG/token.txt" ]; then
    :
elif [ -f "$BASE/endpoint.txt" ] && [ -f "$BASE/token.txt" ]; then
    # DIAG-001 packages kept the credentials beside this script.
    CONFIG=$BASE
elif [ -f "$(dirname "$BASE")/endpoint.txt" ] && [ -f "$(dirname "$BASE")/token.txt" ]; then
    # The main extension keeps the legacy location at the extension root.
    CONFIG=$(dirname "$BASE")
elif [ -f "$R/mnt/us/extensions/moyue-diagnostics/endpoint.txt" ] && \
    [ -f "$R/mnt/us/extensions/moyue-diagnostics/token.txt" ]; then
    # The standalone DIAG-001 extension may still hold the current token.
    CONFIG=$R/mnt/us/extensions/moyue-diagnostics
else
    fail 'upload configuration missing (see diagnostics/使用说明.md)'
fi
[ ! -L "$CONFIG/endpoint.txt" ] && [ ! -L "$CONFIG/token.txt" ] || fail 'upload configuration is a symlink'
ENDPOINT=$(cat "$CONFIG/endpoint.txt")
TOKEN=$(cat "$CONFIG/token.txt")
case "$ENDPOINT" in https://*) ;; *) fail 'HTTPS endpoint required' ;; esac
case "$ENDPOINT" in *[!a-zA-Z0-9:/._-]*) fail 'invalid endpoint' ;; esac
case "$TOKEN" in ''|*[!a-zA-Z0-9_-]*) fail 'invalid upload token' ;; esac
[ "${#TOKEN}" -ge 32 ] || fail 'invalid upload token'
command -v curl >/dev/null 2>&1 || fail 'curl missing; copy report.txt to your computer for upload'
say 'Uploading logs (up to 45 seconds)...'
# Pass the token through stdin so it is absent from the process arguments.
if ! printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" |
    curl -q --config - --silent --show-error --fail --connect-timeout 10 --max-time 45 \
        --proto '=https' --header 'Content-Type: text/plain' \
        --data-binary "@$OUT/report.txt" --output "$LOCK/response.txt" "$ENDPOINT"; then
    fail 'upload failed; report.txt kept. Check Wi-Fi or upload from a computer'
fi
RECEIPT=$(cat "$LOCK/response.txt")
case "$RECEIPT" in ''|*[!0-9a-f]*) fail 'unexpected server response; report.txt kept' ;; esac
[ "${#RECEIPT}" -eq 16 ] || fail 'unexpected server response; report.txt kept'
printf '%s\n' "$RECEIPT" > "$LOCK/receipt.txt"
mv -f "$LOCK/receipt.txt" "$OUT/receipt.txt"
say "Uploaded. Report ID: $RECEIPT"
