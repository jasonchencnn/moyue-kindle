#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Host-side test of the KUAL OTA updater and of its packaging.
#
#   sh device/tests/ota.sh
#
# A local HTTP server stands in for me.chen-cn.top; AGENT_TEST_ROOT stands in
# for /, and the test-only MOYUE_OTA_TEST_HTTP / MOYUE_OTA_TEST_CRASH switches
# select the local origin and a crash between the app and extension swaps.
# Nothing outside the temporary directory is touched.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
DEVICE=$(cd "$HERE/.." && pwd)
REPO=$(cd "$DEVICE/.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/moyue-ota-test.XXXXXX")
# The version this checkout carries; the fake app bundle must match it or
# tools/package.sh refuses to package the tree.
WORKSPACE_VERSION=$(sed -n 's/^version = "\([0-9][0-9.]*\)"/\1/p' \
    "$REPO/Cargo.toml" | head -n 1)
failures=0
server=
PORT=

cleanup() {
    if [ -n "$server" ]; then
        kill "$server" 2> /dev/null
        wait "$server" 2> /dev/null
    fi
    for p in $(cat "$WORK"/*/var/tmp/*.pid 2> /dev/null); do kill "$p" 2> /dev/null; done
    if [ "${KEEP:-0}" = 1 ]; then
        printf 'work kept at %s\n' "$WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

check() {
    if eval "$2"; then
        printf '  ok    %s\n' "$1"
    else
        printf '  FAIL  %s\n' "$1"
        sed -n '1,8p' "$WORK/out" 2> /dev/null | sed 's/^/        /' >&2
        failures=$((failures + 1))
    fi
}

# stubs: pidof says nothing is running unless FAKE_RUNNING=1; eips and sleep
# keep the output and the stop wait short.
mkdir -p "$WORK/bin"
cat > "$WORK/bin/pidof" << 'EOF'
#!/bin/sh
[ "${FAKE_RUNNING:-0}" = 1 ] || exit 1
echo 4242
EOF
cat > "$WORK/bin/eips" << 'EOF'
#!/bin/sh
echo "$*" >> "$AGENT_TEST_ROOT/eips.log"
exit 0
EOF
cat > "$WORK/bin/sleep" << 'EOF'
#!/bin/sh
exit 0
EOF
cat > "$WORK/bin/sync" << 'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$WORK/bin/pidof" "$WORK/bin/eips" "$WORK/bin/sleep" "$WORK/bin/sync"
TEST_PATH="$WORK/bin:$PATH"

fake_app() {
    dir=$1
    version=$2
    mkdir -p "$dir/data/css" "$dir/licenses"
    cat > "$dir/aristotle" << 'EOF'
#!/bin/sh
echo "fake aristotle"
EOF
    chmod 755 "$dir/aristotle"
    for f in run-aristotle.sh takeover.sh open-kual.sh; do
        printf '#!/bin/sh\nexit 0\n' > "$dir/$f"
        chmod 755 "$dir/$f"
    done
    printf 'fake source\n' > "$dir/SOURCE.txt"
    printf '%s\n' "$version" > "$dir/VERSION"
    printf 'body {}\n' > "$dir/data/css/epub.css"
    printf 'tests\n' > "$dir/licenses/README.txt"
}

make_device() {
    r=$1
    mkdir -p "$r/proc" "$r/etc" "$r/mnt/us/extensions" "$r/mnt/us/aristotle" \
        "$r/mnt/us/.aristotle" "$r/mnt/us/documents" "$r/mnt/us/fonts" "$r/mnt/us/screensavers" "$r/var/tmp"
    printf 'G000LN0123456789\n' > "$r/proc/usid"
    printf 'Kindle 5.16.2.1.1 (test)\n' > "$r/etc/prettyversion.txt"
    cp -R "$DEVICE/extension" "$r/mnt/us/extensions/aristotle"
    # The fake device runs the previous release, whatever the tree's own
    # extension version is: rewrite the copied config so both halves agree.
    sed -i '' 's|<version>[0-9.]*</version>|<version>0.3.1</version>|' \
        "$r/mnt/us/extensions/aristotle/config.xml" 2>/dev/null \
        || sed -i 's|<version>[0-9.]*</version>|<version>0.3.1</version>|' \
        "$r/mnt/us/extensions/aristotle/config.xml"
    fake_app "$r/mnt/us/aristotle" 0.3.1
    printf 'login and settings\n' > "$r/mnt/us/.aristotle/reader.json"
    printf 'book bytes\n' > "$r/mnt/us/documents/book.epub"
    printf 'user font\n' > "$r/mnt/us/fonts/user.ttf"
    printf 'user screensaver\n' > "$r/mnt/us/screensavers/a.png"
    printf 'ssh-ed25519 test\n' > "$r/mnt/us/extensions/aristotle/authorized_keys"
    mkdir -p "$r/mnt/us/extensions/aristotle/diagnostics"
    printf 'https://example.test/moyue-diagnostics\n' > "$r/mnt/us/extensions/aristotle/diagnostics/endpoint.txt"
    printf 'legacy-token\n' > "$r/mnt/us/extensions/aristotle/diagnostics/token.txt"
}

# make_pkg OUT VERSION [MUTATION]
make_pkg() {
    out=$1 version=$2 mutation=${3:-none}
    PKG_OUT=$out PKG_VERSION=$version PKG_MUTATION=$mutation EXT_SRC="$DEVICE/extension" \
        python3 - "$WORK" << 'PY'
import os, sys, tarfile
from pathlib import Path
work = Path(sys.argv[1])
out = Path(os.environ["PKG_OUT"])
version = os.environ["PKG_VERSION"]
mutation = os.environ["PKG_MUTATION"]
ext_src = Path(os.environ["EXT_SRC"])
tree = out.with_suffix(".tree")
if tree.exists():
    import shutil; shutil.rmtree(tree)
app = tree / "aristotle"
ext = tree / "extensions" / "aristotle"
app.mkdir(parents=True)
ext.parent.mkdir(parents=True)
(app / "data" / "css").mkdir(parents=True)
(app / "licenses").mkdir()
(app / "aristotle").write_text("#!/bin/sh\necho fake\n")
os.chmod(app / "aristotle", 0o755)
for f in ("run-aristotle.sh", "takeover.sh", "open-kual.sh"):
    (app / f).write_text("#!/bin/sh\nexit 0\n")
    os.chmod(app / f, 0o755)
(app / "SOURCE.txt").write_text("fake source\n")
(app / "VERSION").write_text(version + "\n")
(app / "data" / "css" / "epub.css").write_text("body {}\n")
(app / "licenses" / "README.txt").write_text("licenses\n")
import shutil
shutil.copytree(ext_src, ext)
cfg = ext / "config.xml"
import re
cfg.write_text(re.sub(r"<version>[0-9.]+</version>", f"<version>{version}</version>", cfg.read_text(), count=1))
if mutation == "version-mismatch":
    (app / "VERSION").write_text("9.9.9\n")
elif mutation == "symlink":
    os.symlink("/etc/passwd", app / "evil-link")
elif mutation == "unexpected":
    (tree / "documents").mkdir()
    (tree / "documents" / "book.epub").write_text("book\n")
stamp = 1700000000
def add(t, path, arc):
    if path.is_symlink():
        ti = tarfile.TarInfo(arc)
        ti.type = tarfile.SYMTYPE
        ti.linkname = os.readlink(path)
    elif path.is_dir():
        ti = tarfile.TarInfo(arc + "/")
        ti.type = tarfile.DIRTYPE
        ti.mode = 0o755
    else:
        ti = tarfile.TarInfo(arc)
        ti.size = path.stat().st_size
        ti.mode = path.stat().st_mode & 0o777
        with path.open("rb") as f:
            data = f.read()
    ti.mtime = stamp
    ti.uid = ti.gid = 0
    ti.uname = ti.gname = ""
    if path.is_dir():
        t.addfile(ti)
    elif path.is_symlink():
        t.addfile(ti)
    else:
        import io
        t.addfile(ti, io.BytesIO(data))
with tarfile.open(out, "w:gz", format=tarfile.GNU_FORMAT) as t:
    for d, dirs, files in os.walk(tree):
        dirs.sort(); files.sort()
        for n in dirs:
            p = Path(d) / n
            add(t, p, str(p.relative_to(tree)))
        for n in files:
            p = Path(d) / n
            add(t, p, str(p.relative_to(tree)))
    if mutation == "traversal":
        ti = tarfile.TarInfo("../evil.txt")
        data = b"escaped\n"
        ti.size = len(data); ti.mode = 0o644; ti.mtime = stamp
        import io
        t.addfile(ti, io.BytesIO(data))
PY
}

digest_file() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# manifest NAME VERSION DEVICE PKG SHA SIZE [CHANNEL], package URL under the
# local server.  The channel defaults to the fake device's own channel.
manifest() {
    name=$1 version=$2 device=$3 pkg=$4 sha=$5 size=$6 channel=${7:-0LN-5.16.2.1.1}
    mkdir -p "$WORK/http"
    {
        echo "MOYUE-OTA-1"
        echo "product=moyue-preview"
        echo "channel=$channel"
        echo "version=$version"
        echo "device=$device"
        echo "firmware=5.16"
        echo "package=http://127.0.0.1:$PORT/$pkg"
        echo "sha256=$sha"
        echo "size=$size"
        echo "source=http://127.0.0.1:$PORT/source.tar.gz"
        echo "source-sha256=$sha"
        echo "source-size=$size"
    } > "$WORK/http/$name"
}

run_update() {
    r=$1 name=$2 channel=${3:-}
    AGENT_TEST_ROOT=$r \
        ARISTOTLE_EXT_DIR=$r/mnt/us/extensions/aristotle \
        ARISTOTLE_APP_DIR=$r/mnt/us/aristotle \
        MOYUE_OTA_DIR=$r/mnt/us/.aristotle/ota \
        MOYUE_OTA_MANIFEST_URL="http://127.0.0.1:$PORT/$name" \
        MOYUE_OTA_ALLOWED_PREFIX="http://127.0.0.1:$PORT/" \
        MOYUE_OTA_TEST_HTTP=1 \
        MOYUE_OTA_CHANNEL="$channel" \
        PATH="$TEST_PATH" \
        sh "$r/mnt/us/extensions/aristotle/bin/ota-update.sh" install
    return $?
}

# Start the local server after the first package is ready.
make_pkg "$WORK/http/pkg-0.3.2.tar.gz" 0.3.2
printf 'source\n' > "$WORK/http/source.tar.gz"
cat > "$WORK/server.py" << 'PY'
import http.server, os, sys
root, port_file = sys.argv[1], sys.argv[2]
os.chdir(root)
class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass
httpd = http.server.HTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as f:
    f.write(str(httpd.server_address[1]))
httpd.serve_forever()
PY
python3 "$WORK/server.py" "$WORK/http" "$WORK/port" &
server=$!
i=0
while [ ! -s "$WORK/port" ] && [ $i -lt 50 ]; do sleep 0.1 2>/dev/null || sleep 1; i=$((i + 1)); done
PORT=$(cat "$WORK/port")
[ -n "$PORT" ] || { echo "no local server" >&2; exit 1; }

PKG="$WORK/http/pkg-0.3.2.tar.gz"
SHA=$(digest_file "$PKG")
SIZE=$(wc -c < "$PKG" | tr -d " ")
manifest equal.txt 0.3.1 0LN pkg-0.3.2.tar.gz "$SHA" "$SIZE"
manifest older.txt 0.3.0 0LN pkg-0.3.2.tar.gz "$SHA" "$SIZE"
manifest wrong-device.txt 0.3.2 0PP pkg-0.3.2.tar.gz "$SHA" "$SIZE"
manifest bad-digest.txt 0.3.2 0LN pkg-0.3.2.tar.gz "$(printf '%064d' 0)" "$SIZE"
manifest bad-size.txt 0.3.2 0LN pkg-0.3.2.tar.gz "$SHA" $((SIZE + 1))
manifest good.txt 0.3.2 0LN pkg-0.3.2.tar.gz "$SHA" "$SIZE"

# Each failing case starts from a fresh fake device.
fresh() {
    rm -rf "$WORK/dev"
    make_device "$WORK/dev"
}

printf '\nOTA manifest and version checks\n'
fresh
mkdir -p "$WORK/dev/var/tmp/moyue-ota.lock"
run_update "$WORK/dev" equal.txt > "$WORK/out" 2>&1
check "another run's lock is not removed" \
    "[ $? = 1 ] && grep -q '另一个更新正在进行' $WORK/out && [ -d $WORK/dev/var/tmp/moyue-ota.lock ]"
fresh
run_update "$WORK/dev" equal.txt > "$WORK/out" 2>&1
check "same version: no download, nothing changed" \
    "[ $? = 0 ] && grep -q '已是最新版本 0.3.1' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.1 ]"
fresh
run_update "$WORK/dev" older.txt > "$WORK/out" 2>&1
check "older server version: refuses downgrade" \
    "[ $? = 0 ] && grep -q '不降级' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.1 ]"
fresh
run_update "$WORK/dev" wrong-device.txt > "$WORK/out" 2>&1
check "different device: refused" "[ $? = 1 ] && grep -q '不是给设备 0LN' $WORK/out"
fresh
sed -i '' 's|<version>0.3.1</version>|<version>0.3</version>|' "$WORK/dev/mnt/us/extensions/aristotle/config.xml" 2>/dev/null || sed -i 's|<version>0.3.1</version>|<version>0.3</version>|' "$WORK/dev/mnt/us/extensions/aristotle/config.xml"
printf '0.3\n' > "$WORK/dev/mnt/us/aristotle/VERSION"
run_update "$WORK/dev" good.txt > "$WORK/out" 2>&1
check "malformed installed version fails closed" \
    "[ $? = 1 ] && grep -q '已安装版本格式无效' $WORK/out"
fresh
rm "$WORK/dev/mnt/us/extensions/aristotle/guard.sh"
run_update "$WORK/dev" good.txt > "$WORK/out" 2>&1
check "missing guard fails with a clear error" \
    "[ $? = 1 ] && grep -q '缺少设备守卫' $WORK/out"
fresh
run_update "$WORK/dev" missing.txt > "$WORK/out" 2>&1
check "no channel for the device: reports no update, nothing changed" \
    "[ $? = 0 ] && grep -q '暂未提供在线更新' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.1 ]"

printf '\nFirmware range channel\n'
fresh
mkdir -p "$WORK/http"
cp "$WORK/http/good.txt" "$WORK/http/range.txt"
sed -i '' 's|^firmware=.*|firmware=5.16|' "$WORK/http/range.txt" 2>/dev/null \
    || sed -i 's|^firmware=.*|firmware=5.16|' "$WORK/http/range.txt"
sed -i '' 's|^channel=.*|channel=0LN-5.16|' "$WORK/http/range.txt" 2>/dev/null \
    || sed -i 's|^channel=.*|channel=0LN-5.16|' "$WORK/http/range.txt"
run_update "$WORK/dev" range.txt 0LN-5.16 > "$WORK/out" 2>&1
check "a firmware-range channel updates the device" \
    "[ $? = 0 ] && grep -q '更新完成：0.3.2' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.2 ]"

printf '\nPackage integrity and archive checks\n'
fresh
run_update "$WORK/dev" bad-digest.txt > "$WORK/out" 2>&1
check "bad SHA-256: refused before stopping the app" \
    "[ $? = 1 ] && grep -q 'SHA-256 校验失败' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.1 ]"
fresh
run_update "$WORK/dev" bad-size.txt > "$WORK/out" 2>&1
check "bad size: refused" "[ $? = 1 ] && grep -q '大小不符' $WORK/out"

for mutation in traversal symlink unexpected version-mismatch; do
    make_pkg "$WORK/http/pkg-$mutation.tar.gz" 0.3.2 "$mutation"
    msha=$(digest_file "$WORK/http/pkg-$mutation.tar.gz")
    msize=$(wc -c < "$WORK/http/pkg-$mutation.tar.gz" | tr -d " ")
    manifest "$mutation.txt" 0.3.2 0LN "pkg-$mutation.tar.gz" "$msha" "$msize"
    fresh
    run_update "$WORK/dev" "$mutation.txt" > "$WORK/out" 2>&1
    rc=$?
    case $mutation in
    traversal) want='路径穿越' ;;
    symlink) want='符号链接' ;;
    unexpected) want='不允许的路径' ;;
    version-mismatch) want='版本与清单不一致' ;;
    esac
    check "$mutation: refused" "[ $rc = 1 ] && grep -q '$want' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.1 ]"
done
check "traversal wrote nothing outside the staging area" "[ ! -e $WORK/evil.txt ]"

printf '\nSuccessful update and user data\n'
fresh
run_update "$WORK/dev" good.txt > "$WORK/out" 2>&1
check "update succeeds" "[ $? = 0 ] && grep -q '更新完成：0.3.2' $WORK/out"
check "app replaced and versioned" \
    "[ -f $WORK/dev/mnt/us/aristotle/VERSION ] && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.2 ]"
check "extension replaced and versioned" \
    "grep -q '<version>0.3.2</version>' $WORK/dev/mnt/us/extensions/aristotle/config.xml"
check "OTA action and diagnostics remain in the new extension" \
    "[ -f $WORK/dev/mnt/us/extensions/aristotle/bin/ota-update.sh ] && [ -f $WORK/dev/mnt/us/extensions/aristotle/diagnostics/diagnostics.sh ]"
check "book, progress/login, settings, screensaver and font kept" \
    "[ \"\$(cat $WORK/dev/mnt/us/documents/book.epub)\" = 'book bytes' ] && [ \"\$(cat $WORK/dev/mnt/us/.aristotle/reader.json)\" = 'login and settings' ] && [ \"\$(cat $WORK/dev/mnt/us/fonts/user.ttf)\" = 'user font' ] && [ \"\$(cat $WORK/dev/mnt/us/screensavers/a.png)\" = 'user screensaver' ]"
check "SSH authorized_keys kept across the extension swap" \
    "grep -q 'ssh-ed25519 test' $WORK/dev/mnt/us/extensions/aristotle/authorized_keys"
check "legacy diagnostics credentials survive in diagnostics/" \
    "[ \"\$(cat $WORK/dev/mnt/us/extensions/aristotle/diagnostics/token.txt)\" = legacy-token ] && [ \"\$(cat $WORK/dev/mnt/us/extensions/aristotle/diagnostics/endpoint.txt)\" = 'https://example.test/moyue-diagnostics' ]"
check "backup and journal cleaned after success" \
    "[ ! -e $WORK/dev/mnt/us/.aristotle/ota/backup ] && [ ! -e $WORK/dev/mnt/us/.aristotle/ota/journal ]"
check "permanent independent recovery entry stays installed" \
    "[ -f $WORK/dev/mnt/us/extensions/moyue-ota-recovery/config.xml ] && [ -f $WORK/dev/mnt/us/extensions/moyue-ota-recovery/bin/recover.sh ]"

printf '\nRollback and interruption\n'
make_pkg "$WORK/http/pkg-0.3.3.tar.gz" 0.3.3
sha3=$(digest_file "$WORK/http/pkg-0.3.3.tar.gz")
size3=$(wc -c < "$WORK/http/pkg-0.3.3.tar.gz" | tr -d " ")
manifest rollback.txt 0.3.3 0LN pkg-0.3.3.tar.gz "$sha3" "$size3"
fresh
mkdir -p "$WORK/dev/mnt/us/extensions/moyue-ota-recovery"
chmod 500 "$WORK/dev/mnt/us/extensions"
run_update "$WORK/dev" rollback.txt > "$WORK/out" 2>&1
status=$?
chmod 700 "$WORK/dev/mnt/us/extensions"
check "failed extension swap: old app and extension restored" \
    "[ $status = 1 ] && grep -q '已恢复旧版本' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.1 ] && grep -q '<version>0.3.1</version>' $WORK/dev/mnt/us/extensions/aristotle/config.xml"
check "failed swap left no journal or backup" \
    "[ ! -e $WORK/dev/mnt/us/.aristotle/ota/journal ] && [ ! -e $WORK/dev/mnt/us/.aristotle/ota/backup ]"

fresh
MOYUE_OTA_TEST_CRASH=app run_update "$WORK/dev" good.txt > "$WORK/out" 2>&1
check "simulated power loss left a journal and the new app" \
    "[ -e $WORK/dev/mnt/us/.aristotle/ota/journal ] && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.2 ]"
rm -rf "$WORK/dev/var/tmp/moyue-ota.lock"
MOYUE_OTA_TEST_CRASH= run_update "$WORK/dev" good.txt > "$WORK/out" 2>&1
check "next run recovers the interrupted swap and finishes" \
    "[ $? = 0 ] && grep -q '已恢复更新前的版本' $WORK/out && grep -q '更新完成：0.3.2' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.2 ] && grep -q '<version>0.3.2</version>' $WORK/dev/mnt/us/extensions/aristotle/config.xml"

fresh
mkdir -p "$WORK/dev/mnt/us/extensions/moyue-ota-recovery"
chmod 500 "$WORK/dev/mnt/us/extensions"
MOYUE_OTA_TEST_ROLLBACK_FAIL=1 run_update "$WORK/dev" good.txt > "$WORK/out" 2>&1
status=$?
chmod 700 "$WORK/dev/mnt/us/extensions"
check "failed rollback keeps backup and journal, reports failure" \
    "[ $status = 1 ] && grep -q '回滚文件保留' $WORK/out && [ -d $WORK/dev/mnt/us/.aristotle/ota/backup/app ] && [ -f $WORK/dev/mnt/us/.aristotle/ota/journal ]"

fresh
MOYUE_OTA_TEST_CRASH=ext MOYUE_OTA_TEST_ROLLBACK_FAIL= run_update "$WORK/dev" good.txt > "$WORK/out" 2>&1
check "power loss after extension rename leaves the recovery extension" \
    "[ ! -d $WORK/dev/mnt/us/extensions/aristotle ] && [ -f $WORK/dev/mnt/us/extensions/moyue-ota-recovery/bin/recover.sh ]"
rm -rf "$WORK/dev/var/tmp/moyue-ota.lock"
MOYUE_OTA_TEST_CRASH= MOYUE_OTA_TEST_ROLLBACK_FAIL= \
    AGENT_TEST_ROOT=$WORK/dev ARISTOTLE_EXT_DIR=$WORK/dev/mnt/us/extensions/aristotle \
    ARISTOTLE_APP_DIR=$WORK/dev/mnt/us/aristotle \
    MOYUE_OTA_DIR=$WORK/dev/mnt/us/.aristotle/ota \
    sh "$WORK/dev/mnt/us/extensions/moyue-ota-recovery/bin/recover.sh" > "$WORK/out" 2>&1
check "recovery entry restores both trees after extension-boundary power loss" \
    "[ $? = 0 ] && grep -q '已恢复更新前的版本' $WORK/out && [ \"\$(cat $WORK/dev/mnt/us/aristotle/VERSION)\" = 0.3.1 ] && grep -q '<version>0.3.1</version>' $WORK/dev/mnt/us/extensions/aristotle/config.xml"
check "eips status and errors are ASCII" \
    "[ -s $WORK/dev/eips.log ] && ! LC_ALL=C grep -q '[^ -~]' $WORK/dev/eips.log"

printf '\nPackaging includes diagnostics and no credentials\n'
mkdir -p "$WORK/bundle/aristotle/licenses"
fake_app "$WORK/bundle/aristotle" "$WORKSPACE_VERSION"
tar -czf "$WORK/fake-app.tar.gz" -C "$WORK/bundle" aristotle
APP_BUNDLE="$WORK/fake-app.tar.gz" sh "$REPO/tools/package.sh" tree "$WORK/pkgtree" > "$WORK/out" 2>&1
check "package.sh tree includes the workspace VERSION" \
    "[ $? = 0 ] && [ \"\$(cat $WORK/pkgtree/aristotle/VERSION)\" = \"$WORKSPACE_VERSION\" ]"
check "package.sh tree includes diagnostics and OTA menu" \
    "[ -f $WORK/pkgtree/extensions/aristotle/diagnostics/diagnostics.sh ] && grep -q '诊断日志' $WORK/pkgtree/extensions/aristotle/menu.json && grep -q '检查更新' $WORK/pkgtree/extensions/aristotle/menu.json"
check "package tree has no endpoint.txt or token.txt" \
    "[ -z \"\$(find $WORK/pkgtree -name endpoint.txt -o -name token.txt)\" ]"
check "package tree has no credentials in diagnostics config" \
    "[ ! -e $WORK/pkgtree/extensions/aristotle/diagnostics/.gitignore ] || ! grep -q 'https://' $WORK/pkgtree/extensions/aristotle/diagnostics/.gitignore"
APP_BUNDLE="$WORK/fake-app.tar.gz" sh "$REPO/tools/package.sh" zip "$WORK/pkg.zip" > "$WORK/out" 2>&1
check "package.sh zip (normal USB packaging) includes diagnostics" \
    "[ $? = 0 ] && unzip -Z1 $WORK/pkg.zip | grep -q '^extensions/aristotle/diagnostics/diagnostics.sh$' && unzip -Z1 $WORK/pkg.zip | grep -q '^aristotle/VERSION$'"
APP_BUNDLE="$WORK/fake-app.tar.gz" sh "$REPO/tools/package.sh" ota "$WORK/pkg.tar.gz" > "$WORK/out" 2>&1
check "package.sh ota builds a tar" "[ $? = 0 ] && [ -s $WORK/pkg.tar.gz ]"
check "OTA tar has no top-level extensions directory entry" \
    "[ -z \"\$(tar -tzf $WORK/pkg.tar.gz | grep -x 'extensions/')\" ]"
check "OTA tar has only files and directories" \
    "[ -z \"\$(tar -tvzf $WORK/pkg.tar.gz | grep -v '^[-d]')\" ]"
check "OTA tar carries diagnostics and version" \
    "tar -tzf $WORK/pkg.tar.gz | grep -q '^extensions/aristotle/diagnostics/diagnostics.sh$' && tar -tzf $WORK/pkg.tar.gz | grep -q '^aristotle/VERSION$'"

printf '\n%d failure(s)\n' "$failures"
[ "$failures" = 0 ]
