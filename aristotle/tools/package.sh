#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Package Aristotle. This is the one definition of what goes on the Kindle's
# USB drive; the release workflow, CI and tools/host/install-extensions.sh
# all use it.
#
# usage: tools/package.sh tree DIR
#        tools/package.sh zip [FILE]
#        tools/package.sh source [FILE]
#        tools/package.sh version
#
#   tree    Lays out DIR (which must not exist yet) as the root of the drive:
#             DIR/aristotle/             the app bundle of tools/build-render.sh
#                                        device (APP_BUNDLE), with licenses/
#                                        and SOURCE.txt; left out if not built
#             DIR/extensions/aristotle/  the KUAL extension (device/extension/
#                                        and takeover.sh)
#   zip     The same tree as one zip that users unpack to the root of the
#           drive; the app is required. Default FILE:
#           $ARISTOTLE_CACHE/dist/$PRODUCT-$VERSION.zip
#   source  The corresponding source of a release: the files of HEAD,
#           third_party/plato at the commit HEAD records, the reading
#           engine (when this tree has one: the engine crate, crengine-ng's
#           sources and the four C libraries it needs), and REVISION,
#           C-SOURCES.txt and ENGINE-SOURCES.txt at the top. Default FILE:
#           $ARISTOTLE_CACHE/dist/$PRODUCT-$VERSION-source.tar.gz
#   version Prints VERSION.
#
# VERSION is workspace.package.version in Cargo.toml. PRODUCT is the release
# file name stem (default moyue-preview, override with PRODUCT=). APP_BUNDLE
# defaults to $ARISTOTLE_CACHE/render/aristotle-device.tar.gz. File times in
# the zip and the tarball are the commit's (or SOURCE_DATE_EPOCH), so the same
# inputs give the same bytes.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
. "$REPO/tools/env.sh"
APP_BUNDLE=${APP_BUNDLE:-$ARISTOTLE_CACHE/render/aristotle-device.tar.gz}
PRODUCT=${PRODUCT:-moyue-preview}
VERSION=$(python3 -c 'import sys, tomllib
print(tomllib.load(open(sys.argv[1], "rb"))["workspace"]["package"]["version"])' "$REPO/Cargo.toml")
EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$REPO" log -1 --format=%ct 2>/dev/null || echo 315532800)}

usage() {
    echo "usage: $0 tree DIR | zip [FILE] | source [FILE] | version" >&2
    exit 1
}

# lay_out DIR [app-required]
lay_out() {
    [ ! -e "$1" ] || { echo "$1 exists; give a new directory" >&2; exit 1; }
    if [ -s "$APP_BUNDLE" ]; then
        mkdir -p "$1"
        tar -xzf "$APP_BUNDLE" -C "$1"
        for f in aristotle run-aristotle.sh open-kual.sh SOURCE.txt licenses/README.txt; do
            [ -e "$1/aristotle/$f" ] || { echo "$APP_BUNDLE has no aristotle/$f" >&2; exit 1; }
        done
        echo "app: $APP_BUNDLE"
    elif [ "${2:-}" = app-required ]; then
        echo "no app bundle at $APP_BUNDLE; run tools/build-render.sh device or set APP_BUNDLE" >&2
        exit 1
    else
        echo "app: not built ($APP_BUNDLE), left out"
    fi
    mkdir -p "$1/extensions/aristotle"
    cp -R "$REPO/device/extension/." "$1/extensions/aristotle/"
    cp "$REPO/device/app/takeover.sh" "$REPO/device/app/open-kual.sh" "$1/extensions/aristotle/"
}

work() {
    WORK=$(mktemp -d "$TMPDIR/package.XXXXXX")
    trap 'rm -rf "$WORK"' EXIT
}

case ${1:-} in
    version)
        echo "$VERSION"
        ;;
    tree)
        [ $# -eq 2 ] || usage
        lay_out "$2"
        ;;
    zip)
        [ $# -le 2 ] || usage
        OUT=${2:-$ARISTOTLE_CACHE/dist/$PRODUCT-$VERSION.zip}
        mkdir -p "$(dirname "$OUT")"
        work
        lay_out "$WORK/root" app-required
        # Sorted entries, fixed times, Unix modes kept (the binary and the
        # scripts stay executable for unzip on a computer).
        python3 - "$WORK/root" "$WORK/out.zip" "$EPOCH" <<'EOF'
import os, sys, time, zipfile
root, out, epoch = sys.argv[1], sys.argv[2], int(sys.argv[3])
stamp = time.gmtime(max(epoch, 315532800))[:6]
entries = []
for d, dirs, files in os.walk(root):
    entries += [os.path.relpath(os.path.join(d, n), root) + "/" for n in dirs]
    entries += [os.path.relpath(os.path.join(d, n), root) for n in files]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for rel in sorted(entries):
        path = os.path.join(root, rel)
        info = zipfile.ZipInfo(rel, stamp)
        info.external_attr = (os.stat(path).st_mode & 0xFFFF) << 16
        if rel.endswith("/"):
            info.external_attr |= 0x10
            z.writestr(info, b"")
        else:
            info.compress_type = zipfile.ZIP_DEFLATED
            with open(path, "rb") as f:
                z.writestr(info, f.read())
EOF
        mv -f "$WORK/out.zip" "$OUT"
        ls -l "$OUT"
        ;;
    source)
        [ $# -le 2 ] || usage
        OUT=${2:-$ARISTOTLE_CACHE/dist/$PRODUCT-$VERSION-source.tar.gz}
        mkdir -p "$(dirname "$OUT")"
        P=$PRODUCT-$VERSION
        COMMIT=$(git -C "$REPO" rev-parse HEAD)
        # Where this tree sits in its repository: the root of Aristotle's own
        # repository, a subtree in the readest repository. Git's paths are
        # relative to the repository's root, not to this directory.
        TREE=$(git -C "$REPO" rev-parse --show-prefix)
        TREE=${TREE%/}
        if [ -z "$TREE" ]; then
            PLATO_REF=HEAD:third_party/plato
        else
            PLATO_REF=HEAD:$TREE/third_party/plato
        fi
        PLATO=$(git -C "$REPO" rev-parse "$PLATO_REF")
        TAG=$(git -C "$REPO" describe --tags --exact-match HEAD 2>/dev/null || true)
        git -C "$REPO/third_party/plato" cat-file -e "$PLATO^{commit}" 2>/dev/null || {
            echo "third_party/plato lacks commit $PLATO; run: git submodule update --init" >&2
            exit 1
        }
        if [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]; then
            echo "note: uncommitted changes are not in the tarball; it packs HEAD ($COMMIT)" >&2
        fi
        work
        # Everything is unpacked into one directory and packed again, so
        # the tarball is a single archive with sorted names, no owner and
        # the commit's time, on GNU tar and on bsdtar alike.
        SRC=$WORK/src
        mkdir -p "$SRC"

        # This tree's files. In the readest repository Aristotle is a
        # subtree and the build needs the top-level workspace: its manifests
        # and the crates the device binaries link (engine, platform and the
        # other members Cargo expects). Keep the relative paths of the device
        # build, but leave out documentation media and test data.
        if [ -z "$TREE" ]; then
            PLATO_SRC=$SRC/third_party/plato
            git -C "$REPO" archive --format=tar HEAD -- . | tar -x -C "$SRC"
        else
            TOP=$(git -C "$REPO" rev-parse --show-toplevel)
            PLATO_SRC=$SRC/$TREE/third_party/plato
            git -C "$TOP" archive --format=tar HEAD -- \
                Cargo.toml Cargo.lock \
                core eink engine platform simulator \
                "$TREE" licenses |
                tar -x -C "$SRC"
        fi

        # Plato, at the commit HEAD records.
        mkdir -p "$PLATO_SRC"
        git -C "$REPO/third_party/plato" archive --format=tar "$PLATO" |
            tar -x -C "$PLATO_SRC"

        # The reading engine, when this tree has one. In the readest
        # repository the engine crate sits next to Aristotle and the
        # binaries are built from it; Aristotle's own repository has no
        # engine, and its binaries use Plato's engine only.
        #
        # What goes in: the engine crate (the Rust side), crengine-ng's own
        # sources (GPL-2.0) and the four C libraries crengine-ng needs
        # (FriBiDi, libunibreak, utf8proc, zstd). The GPL and the LGPL
        # oblige the package to ship them, and the tarballs are the ones
        # tools/engine-sysroot.sh downloads and checks.
        ENGINE_ROOT=$(cd "$REPO/.." && pwd)
        ENGINE_COMMIT=""
        CRENGINE_COMMIT=""
        if [ -f "$ENGINE_ROOT/engine/Cargo.toml" ]; then
            ENGINE_COMMIT=$(git -C "$ENGINE_ROOT" rev-parse HEAD)
            if [ ! -f "$SRC/engine/Cargo.toml" ]; then
                git -C "$ENGINE_ROOT" archive --format=tar HEAD -- engine |
                    tar -x -C "$SRC"
            fi

            CRENGINE=$ENGINE_ROOT/engine/vendor/koreader-crengine
            if [ ! -d "$CRENGINE/.git" ]; then
                echo "no koreader-crengine sources in $CRENGINE" >&2
                echo "run: engine/fetch.sh" >&2
                exit 1
            fi
            CRENGINE_COMMIT=$(git -C "$CRENGINE" rev-parse HEAD)
            mkdir -p "$SRC/engine/vendor/koreader-crengine"
            git -C "$CRENGINE" archive --format=tar "$CRENGINE_COMMIT" |
                tar -x -C "$SRC/engine/vendor/koreader-crengine"

            # The four C libraries: the tarballs the engine's sysroot
            # downloads, with the digests tools/engine-sysroot.sh pins.
            ENGINE_BUILD=${ENGINE_BUILD:-$ARISTOTLE_CACHE/engine-build}
            mkdir -p "$SRC/third_party/engine-libs"
            sed -n "/^[[:space:]]*done <<'EOF'\$/,/^EOF\$/p" "$REPO/tools/engine-sysroot.sh" |
                sed '1d;$d' > "$WORK/engine-list.txt"
            : > "$WORK/engine-libs.txt"
            while read -r name file url sha; do
                [ -n "$file" ] || continue
                if [ ! -f "$ENGINE_BUILD/downloads/$file" ]; then
                    echo "$ENGINE_BUILD/downloads/$file not found" >&2
                    echo "run: tools/build-render.sh device" >&2
                    exit 1
                fi
                cp "$ENGINE_BUILD/downloads/$file" "$SRC/third_party/engine-libs/$file"
                printf '  %s\n    sha256 %s\n' "$url" "$sha" >> "$WORK/engine-libs.txt"
            done < "$WORK/engine-list.txt"
            {
                echo "The C libraries the reading engine (crengine-ng) needs, as"
                echo "tools/engine-sysroot.sh downloads and checks them (name, URL,"
                echo "SHA-256). Their tarballs are in third_party/engine-libs/."
                echo
                cat "$WORK/engine-libs.txt"
            } > "$SRC/ENGINE-SOURCES.txt"
        fi

        {
            echo "product $PRODUCT"
            echo "version $VERSION"
            [ -z "$TAG" ] || echo "tag $TAG"
            echo "aristotle $COMMIT"
            echo "plato $PLATO"
            [ -z "$ENGINE_COMMIT" ] || echo "engine $ENGINE_COMMIT"
            [ -z "$CRENGINE_COMMIT" ] || echo "koreader-crengine $CRENGINE_COMMIT"
        } > "$SRC/REVISION"
        {
            echo "The C libraries in Aristotle $VERSION, as tools/build-plato.sh downloads"
            echo "and checks them (name, URL, SHA-256). MuPDF gets the patches in"
            echo "tools/mupdf-patches/ and Plato's kobo.patch; DjVuLibre is built only"
            echo "with PLATO_DJVU=1. tools/build-render.sh pins the fonts and hyphenation"
            echo "patterns it adds to the bundle the same way."
            echo
            sed -n "/^done <<'EOF'\$/,/^EOF\$/p" "$REPO/tools/build-plato.sh" | sed '1d;$d' |
                awk '{ print $1 "\n  " $3 "\n  sha256 " $4 }'
        } > "$SRC/C-SOURCES.txt"

        # One tar: sorted names, no owner, the commit's time.
        python3 - "$SRC" "$WORK/out.tar" "$EPOCH" "$P" <<'EOF'
import os, sys, tarfile
root, out, epoch, prefix = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
stamp = max(epoch, 315532800)
names = []
for d, dirs, files in os.walk(root):
    names += [os.path.relpath(os.path.join(d, n), root) for n in dirs]
    names += [os.path.relpath(os.path.join(d, n), root) for n in files]
with tarfile.open(out, "w", format=tarfile.GNU_FORMAT) as t:
    for rel in sorted(names):
        path = os.path.join(root, rel)
        st = os.stat(path)
        info = tarfile.TarInfo(prefix + "/" + rel.replace(os.sep, "/"))
        info.mtime = stamp
        info.mode = st.st_mode & 0o777
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        if os.path.isdir(path):
            info.type = tarfile.DIRTYPE
            t.addfile(info)
        else:
            info.size = st.st_size
            with open(path, "rb") as f:
                t.addfile(info, f)
EOF
        gzip -9n < "$WORK/out.tar" > "$WORK/out.tar.gz"
        mv -f "$WORK/out.tar.gz" "$OUT"
        ls -l "$OUT"
        ;;
    *) usage ;;
esac
