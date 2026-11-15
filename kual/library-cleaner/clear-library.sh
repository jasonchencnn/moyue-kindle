#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run after exiting Moyue. AGENT_TEST_ROOT redirects paths in host tests.
set -eu
umask 077
R=${AGENT_TEST_ROOT:-}
STORE=$R/mnt/us
DATA=$STORE/.aristotle
INDEX=$DATA/library.json
LOCK=$DATA/.library-cleaner-lock

say() {
    printf '%s\n' "$*"
    eips 0 2 "$*" 2>/dev/null || true
}

fail() {
    say "Cleanup failed: $*"
    exit 1
}

[ -d "$DATA" ] || fail "default .aristotle folder missing"
mkdir "$LOCK" 2>/dev/null || fail "cleanup lock exists"
trap 'rm -f "$LOCK/library.json" "$LOCK/library.json.bak"; rmdir "$LOCK"' EXIT
trap 'exit 1' HUP INT TERM

# A live Saver could overwrite the reset immediately after this script exits.
if ! command -v pidof >/dev/null 2>&1; then
    fail "pidof missing"
fi
if pidof aristotle >/dev/null 2>&1; then
    fail "exit Moyue to Kindle first"
fi
[ -f "$INDEX" ] || [ -f "$INDEX.bak" ] || fail "library index missing"
[ ! -L "$DATA" ] && [ ! -L "$INDEX" ] && [ ! -L "$INDEX.bak" ] || fail "symlink index"

BACKUP=$STORE/.moyue-library-backup-$(date '+%Y%m%d-%H%M%S')-$$
mkdir "$BACKUP" || fail "cannot create backup"
for source in "$INDEX" "$INDEX.bak"; do
    if [ -e "$source" ]; then
        [ -f "$source" ] || fail "index is not a regular file"
        cp "$source" "$BACKUP/$(basename "$source")" || fail "backup copy failed"
        cmp -s "$source" "$BACKUP/$(basename "$source")" || fail "backup verification failed"
    fi
done
sync || fail "backup sync failed"

# Stage both generations before changing either live file.
printf '%s\n' '{"version":1,"books":[],"removed":[]}' > "$LOCK/library.json" || fail "staging failed"
cp "$LOCK/library.json" "$LOCK/library.json.bak" || fail "staging failed"
sync || fail "staging sync failed"
mv "$LOCK/library.json.bak" "$INDEX.bak" || fail "backup index reset failed"
mv "$LOCK/library.json" "$INDEX" || fail "main index reset failed"
sync || fail "index sync failed"
say "Library cleared. Backup: $(basename "$BACKUP")"
