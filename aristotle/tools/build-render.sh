#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the workspace members that link Plato (crates/aristotle-render and
# the reader app, crates/aristotle) and assemble their bundle:
#
#   $OUT/aristotle-render      static binary: the renderer CLI
#   $OUT/aristotle             static binary: the reader app
#   $OUT/data/fonts/           the fonts Plato's engine loads unconditionally
#   $OUT/data/serif/           Literata, the body and chrome font
#                              (fonts cut to $FONT_SUBSET, see below)
#   $OUT/data/css/            Plato's default stylesheets
#   $OUT/data/hyphenation-patterns/   Liang patterns ($HYPH_LANGS)
#   $OUT/licenses/             the licences of everything above
#                              (tools/bundle-licenses.py)
#   $OUT/SOURCE.txt            where the corresponding source is
#
# usage: tools/build-render.sh [device|host]
#
#   device  armv7-unknown-linux-musleabihf, for the PW4 (default)
#   host    x86_64-unknown-linux-musl, for preview PNGs on the PC
#
# The engine the EPUBs are laid out with: ENGINE=crengine (the device
# build's default) links crengine-ng and its libraries statically into the
# binaries; ENGINE=plato leaves them out. The reader picks the engine at
# runtime (ARISTOTLE_EPUB_ENGINE=plato), so both are in the package.
#
# Needs the static C libraries of a tools/build-plato.sh run for the same
# target: PLATO_BUILD (default $ARISTOTLE_CACHE/plato-build/main for device,
# .../host-x86_64 for host; the host one only needs PLATO_LIBS_ONLY=1).
# The crengine build also needs the cross sysroot, which
# tools/engine-sysroot.sh builds (tools/build-engine.sh does the same).
# OUT defaults to $ARISTOTLE_CACHE/render/<device|host> (ARISTOTLE_CACHE comes
# from tools/env.sh); the device tarball goes to BUNDLE (default
# $ARISTOTLE_CACHE/render/aristotle-render-device.tar.gz)
# and the reader app's to APP_BUNDLE (.../aristotle-device.tar.gz).
# CARGO_CMD=clippy|test runs that cargo command instead of building.
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
. "$REPO/tools/env.sh"
WHAT=${1:-device}
case $WHAT in
    device) TARGET=armv7-unknown-linux-musleabihf PFX=arm DEFAULT_BUILD=main ;;
    host) TARGET=x86_64-unknown-linux-musl PFX=x86_64 DEFAULT_BUILD=host-x86_64 ;;
    *) echo "usage: $0 [device|host]" >&2; exit 1 ;;
esac
# The engine: Plato's own, or crengine-ng (ADR-011). The device package
# ships crengine-ng; the host build cannot link it yet, it would need an
# x86_64-musl sysroot of its own (docs/tasks/BUILD-002/notes.md).
ENGINE=${ENGINE-}
case $WHAT in
    device) ENGINE=${ENGINE:-crengine} ;;
    host) ENGINE=${ENGINE:-plato} ;;
esac
case $ENGINE in
    plato|crengine) ;;
    *) echo "ENGINE must be plato or crengine" >&2; exit 1 ;;
esac
ENGINE_ROOT=$(cd "$REPO/.." && pwd)
# The reader app opens the book on a worker thread, so the engine's
# Document trait has to be Send (Plato's is; the readest engine's is not
# yet). Until engine/src has that bound the app is built with Plato's
# engine and only the renderer links crengine-ng; the app's crengine path
# is the blocker in docs/tasks/BUILD-002/notes.md.
APP_ENGINE=${APP_ENGINE-}
if [ -z "$APP_ENGINE" ]; then
    if [ "$ENGINE" = crengine ] &&
       ! grep -q '^pub trait Document: Send' "$ENGINE_ROOT/engine/src/lib.rs"; then
        APP_ENGINE=plato
        echo "note: the app's crengine path needs a Send bound on the engine's Document" >&2
        echo "      trait (engine/src/lib.rs); the app is built with Plato's engine." >&2
        echo "      The renderer still links crengine-ng. docs/tasks/BUILD-002/notes.md" >&2
    else
        APP_ENGINE=$ENGINE
    fi
fi
case $APP_ENGINE in
    plato|crengine) ;;
    *) echo "APP_ENGINE must be plato or crengine" >&2; exit 1 ;;
esac
B=${PLATO_BUILD:-$ARISTOTLE_CACHE/plato-build/$DEFAULT_BUILD}
OUT=${OUT:-$ARISTOTLE_CACHE/render/$WHAT}
TOOLS=${ARISTOTLE_TOOLS:-$ARISTOTLE_CACHE/tools}
# Hyphenation pattern sets to ship: Polish, English, Ancient Greek and Latin
# by default. Plato loads every
# file present when the first paragraph is hyphenated, so fewer files means a
# faster first page. Text in a language without patterns is not hyphenated.
HYPH_LANGS=${HYPH_LANGS:-pl en-us en-gb grc la}
# FONT_SUBSET=latin-greek (default) cuts the fonts to Latin and Greek;
# FONT_SUBSET=none ships them as they come.
FONT_SUBSET=${FONT_SUBSET:-latin-greek}
case $FONT_SUBSET in
    latin-greek|none) ;;
    *) echo "FONT_SUBSET must be latin-greek or none" >&2; exit 1 ;;
esac

if [ ! -f "$B/plato/libs/libmupdf.a" ]; then
    echo "no Plato C libraries in $B/plato/libs" >&2
    echo "run: PLATO_TARGET=$TARGET PLATO_LIBS_ONLY=1 tools/build-plato.sh, or set PLATO_BUILD" >&2
    exit 1
fi

# The engine's C libraries and the cross sysroot they are linked through.
# tools/engine-sysroot.sh is the same library tools/build-engine.sh uses,
# so the sysroot is built once, not once per script.
ENGINE_SRC=""
CRENGINE_DIR=""
if [ "$ENGINE" = crengine ]; then
    if [ "$WHAT" != device ]; then
        echo "ENGINE=crengine needs the armv7 cross sysroot; the $WHAT build has none" >&2
        echo "see docs/tasks/BUILD-002/notes.md" >&2
        exit 1
    fi
    CRENGINE_DIR=$ENGINE_ROOT/engine/vendor/koreader-crengine
    if [ ! -f "$CRENGINE_DIR/CMakeLists.txt" ]; then
        echo "no koreader-crengine sources in $CRENGINE_DIR" >&2
        echo "run: engine/fetch.sh" >&2
        exit 1
    fi
    . "$REPO/tools/engine-sysroot.sh"
    engine_sysroot_build
fi

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$ARISTOTLE_CACHE/target-render}

SRC="$REPO/third_party/plato"
if [ ! -f "$SRC/crates/core/Cargo.toml" ]; then
    echo "third_party/plato is not checked out; run: git submodule update --init" >&2
    exit 1
fi

# plato-core's build.rs (the fork's "core: lib dirs and host musl") links the
# archives from these directories (libmupdf_wrapper.a sits in
# target/mupdf_wrapper/Kindle in older builds).
export PLATO_LIB_DIRS="$B/plato/libs:$B/plato/target/mupdf_wrapper/Kindle"
# zstd-sys (the reader app's page snapshots) compiles C with the cc crate:
# use Zig too.
T_=$(printf %s "$TARGET" | tr - _)
export "CC_$T_=$B/bin/$PFX-cc" "CXX_$T_=$B/bin/$PFX-c++" "AR_$T_=$B/bin/$PFX-ar" "CFLAGS_$T_=-O2"

# CARGO_CMD=clippy or CARGO_CMD=test runs that instead, in the same
# environment, and skips the bundle (tests need the host target). Clippy
# checks every target and fails on warnings, as for the other members.
# CARGO_FLAGS adds options, such as --locked in CI.
CARGO_CMD=${CARGO_CMD:-build}
# APP_FEATURES: the reader app's optional features, comma-separated. The
# default, tls, gives progress sync and catalogs HTTPS (rustls and ring,
# whose C goes through the same Zig wrapper); APP_FEATURES= builds plain
# HTTP only, about 0.6 MB smaller (docs/library.md, "Network").
APP_FEATURES=${APP_FEATURES-tls}
# RENDER_FEATURES: the renderer's optional features. The crengine feature
# lays the EPUBs out with crengine-ng instead of Plato's engine; it comes
# with the engine's own C libraries, which the sysroot above provides.
RENDER_FEATURES=${RENDER_FEATURES-}
[ "$ENGINE" = crengine ] && RENDER_FEATURES="crengine${RENDER_FEATURES:+,$RENDER_FEATURES}"
# The app's own features: the crengine feature of the app crate turns the
# renderer's on as well.
APP_FEATURE_LIST=$APP_FEATURES
[ "$APP_ENGINE" = crengine ] && APP_FEATURE_LIST="crengine${APP_FEATURE_LIST:+,$APP_FEATURE_LIST}"

# cargo PACKAGES FEATURES: one build of the given packages with the given
# features. The features are pkg/feature, one list for both crates.
cargo_build() {
    cargo_pkgs=$1
    cargo_feats=$2
    set -- --config "$REPO/.cargo/config.toml" --release --target "$TARGET" \
        --manifest-path "$REPO/Cargo.toml" $cargo_pkgs
    if [ -n "$cargo_feats" ]; then
        set -- "$@" --features "$cargo_feats"
    fi
    # shellcheck disable=SC2086 # CARGO_FLAGS is a list of options
    if [ "$CARGO_CMD" = clippy ]; then
        cargo clippy "$@" --all-targets ${CARGO_FLAGS:-} -- -D warnings
    else
        cargo "$CARGO_CMD" "$@" ${CARGO_FLAGS:-}
    fi
}

# The features of each crate, as cargo wants them.
render_feats=""
for f in $(printf %s "$RENDER_FEATURES" | tr ',' ' '); do
    [ -n "$f" ] || continue
    render_feats="${render_feats:+$render_feats,}aristotle-render/$f"
done
app_feats=""
for f in $(printf %s "$APP_FEATURE_LIST" | tr ',' ' '); do
    [ -n "$f" ] || continue
    app_feats="${app_feats:+$app_feats,}aristotle/$f"
done

# The renderer and the app are built together when they use the same
# engine. When they do not, they need two builds: cargo unifies a
# package's features across the packages that depend on it, so one build
# would give the app the renderer's crengine feature too.
if [ "$ENGINE" = "$APP_ENGINE" ]; then
    both_feats=$render_feats
    [ -z "$app_feats" ] || both_feats="${both_feats:+$both_feats,}$app_feats"
    cargo_build "-p aristotle-render -p aristotle" "$both_feats"
else
    cargo_build "-p aristotle-render" "$render_feats"
    cargo_build "-p aristotle" "$app_feats"
fi
[ "$CARGO_CMD" = build ] || exit 0

DL="$TOOLS/downloads"
mkdir -p "$DL"
# fetch NAME URL SHA256: download once into $DL, checked.
fetch() {
    if ! sha256_matches "$3" "$DL/$1"; then
        curl -fL --retry 5 --retry-all-errors --retry-delay 2 -o "$DL/$1.part" "$2"
        sha256_matches "$3" "$DL/$1.part" || {
            echo "checksum mismatch: $1" >&2
            exit 1
        }
        mv "$DL/$1.part" "$DL/$1"
    fi
}

# The data directory is assembled from scratch, so nothing from an earlier
# configuration stays behind.
rm -rf "$OUT/data"
mkdir -p "$OUT/data/fonts" "$OUT/data/serif" "$OUT/data/css" "$OUT/data/hyphenation-patterns"
cp "$CARGO_TARGET_DIR/$TARGET/release/aristotle-render" "$OUT/aristotle-render"
cp "$CARGO_TARGET_DIR/$TARGET/release/aristotle" "$OUT/aristotle"

# Fonts. With FONT_SUBSET=latin-greek, Literata (the body font) and Noto Sans
# (CSS sans-serif) keep Latin with all its extensions, Greek with polytonic,
# combining marks, punctuation, super- and subscripts, currency, letterlike
# symbols, number forms, arrows, mathematical operators, geometric shapes and
# the f-ligatures, with all OpenType features; Cyrillic and the rest go.
# Literata keeps its hinting, which the reader app's UI uses (it maps these
# files instead of compiling the font in). Libertinus Serif, the
# engine's own serif family, is opened unconditionally but never drawn,
# because Book replaces the serif family with Literata, so it ships as a stub
# that only maps the space. Source Code Pro and Parisienne are shipped
# unmodified: their licences reserve their names for the original fonts.
# Delius is small already. Subsetting runs fontTools from its pure-Python
# wheel, pinned like the other downloads.
FONTTOOLS=fonttools-4.60.1-py3-none-any.whl
fetch "$FONTTOOLS" \
    "https://files.pythonhosted.org/packages/c7/93/0dd45cd283c32dea1545151d8c3637b4b8c53cdb3a625aeb2885b184d74d/$FONTTOOLS" \
    906306ac7afe2156fcf0042173d6ebbb05416af70f6b370967b47f8f00103bbb
TEXT_UNICODES=U+0000-036F,U+0370-03FF,U+1E00-1FFF,U+2000-22FF,U+25A0-25FF,U+FB00-FB06
# subset SRC DST UNICODES [OPTION...]. The output keeps the source's mtime,
# and copies keep theirs, so the app's snapshot key (sizes and mtimes of
# data/) only changes when the data does.
subset() {
    if [ "$FONT_SUBSET" = none ]; then
        cp -p "$1" "$2"
        return
    fi
    _src=$1 _dst=$2 _uni=$3
    shift 3
    PYTHONPATH="$DL/$FONTTOOLS" python3 -m fontTools.subset "$_src" --output-file="$_dst" \
        --unicodes="$_uni" --layout-features='*' --name-IDs='*' --name-languages='*' \
        --notdef-outline "$@"
    touch -r "$_src" "$_dst"
}
for f in LibertinusSerif-Regular.otf LibertinusSerif-Italic.otf LibertinusSerif-Bold.otf \
         LibertinusSerif-BoldItalic.otf; do
    subset "$SRC/fonts/$f" "$OUT/data/fonts/$f" U+0020
done
for f in NotoSans-Regular.ttf NotoSans-Italic.ttf NotoSans-Bold.ttf NotoSans-BoldItalic.ttf; do
    subset "$SRC/fonts/$f" "$OUT/data/fonts/$f" "$TEXT_UNICODES"
done
# The rest of what plato-core's default_fonts() opens; without any one of
# them the engine has no fonts at all.
for f in SourceCodeVariable-Roman.otf SourceCodeVariable-Italic.otf Parisienne-Regular.ttf \
         Delius-Regular.ttf; do
    cp -p "$SRC/fonts/$f" "$OUT/data/fonts/"
done
for f in "$REPO/crates/aristotle-render/assets/fonts/"Literata-*.ttf; do
    subset "$f" "$OUT/data/serif/$(basename "$f")" "$TEXT_UNICODES"
done
cp -p "$REPO/crates/aristotle-render/assets/fonts/Literata-OFL.txt" "$OUT/data/serif/"
# The Greek koronis (U+1FBD), which Greek texts use as the elision mark,
# drawn as the apostrophe (U+2019) instead of a half-em spacing breathing.
PYTHONPATH="$DL/$FONTTOOLS" python3 "$REPO/tools/font-koronis.py" \
    "$OUT/data/serif/"Literata-*.ttf "$OUT/data/fonts/"NotoSans-*.ttf
cp -p "$SRC/css/epub.css" "$SRC/css/html.css" "$OUT/data/css/"

# Glyph fallback for Plato's engine when MuPDF has no Noto fonts (the fork's
# "core: mupdf tofu fallback"): the file data/fonts/DejaVuSans-Symbols.ttf
# is the font Plato falls back to for every script the body font does not
# cover, whatever it holds. It holds DejaVu Sans cut to arrows, mathematical
# operators, box drawing, geometric shapes, miscellaneous symbols up to the music signs, dingbats up
# to the checkmarks, crosses and stars, and the white squares (about 105 KB
# unhinted), and Noto Sans Coptic from MuPDF's tree (a file of its own).
# Neither licence reserves the names used here.
#
# Chinese needs a font of its own: Noto Sans SC (SIL OFL 1.1), the pan-CJK
# font of the Noto family, cut to the blocks a Chinese book uses. It goes
# into the same file, so a Chinese book shows Chinese instead of boxes.
# tools/font-cjk.py merges the two: the CFF font becomes a TrueType one
# (fontTools merges fonts of one kind only) and the symbol font is scaled
# to its units per em. The name records of the merged font are Noto's, so
# the licence notice quotes the right copyright.
DEJAVU=dejavu-fonts-ttf-2.37
fetch "$DEJAVU.tar.bz2" \
    "https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/$DEJAVU.tar.bz2" \
    fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7
if [ ! -f "$DL/$DEJAVU/ttf/DejaVuSans.ttf" ]; then
    tar -xjf "$DL/$DEJAVU.tar.bz2" -C "$DL" "$DEJAVU/ttf/DejaVuSans.ttf" "$DEJAVU/LICENSE"
fi
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
subset "$DL/$DEJAVU/ttf/DejaVuSans.ttf" "$WORK/DejaVuSans-Symbols.ttf" \
    U+0250-02FF,U+1D00-1DBF,U+2190-21FF,U+2200-22C5,U+2500-25FF,U+2600-266F,U+26A0-26A1,U+2700-2775,U+2B00-2B2F \
    --no-hinting
cp -p "$DL/$DEJAVU/LICENSE" "$OUT/data/fonts/DejaVuSans-LICENSE.txt"
NOTO="$B/plato/thirdparty/mupdf/resources/fonts/noto"
cp -p "$NOTO/NotoSansCoptic-Regular.otf" "$OUT/data/fonts/"
cp -p "$NOTO/COPYING" "$OUT/data/fonts/Noto-OFL.txt"

# The Chinese font: the radicals, the punctuation, the kana and the
# bopomofo, the unified ideographs (the simplified and the traditional
# forms alike), the compatibility ideographs and forms, and the fullwidth
# forms. Extension A and the supplementary planes stay out: they are rare
# names, and they cost megabytes. Basic Latin stays in: crengine refuses to
# register a font without a space glyph, and this is the CJK font it gets.
NOTO_CJK=NotoSansSC-Regular.otf
fetch "$NOTO_CJK" \
    "https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/SubsetOTF/SC/$NOTO_CJK" \
    faa6c9df652116dde789d351359f3d7e5d2285a2b2a1f04a2d7244df706d5ea9
fetch "NotoSansSC-OFL.txt" \
    "https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/LICENSE" \
    6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2
subset "$DL/$NOTO_CJK" "$WORK/$NOTO_CJK" \
    U+0020-007E,U+2E80-2EFF,U+3000-303F,U+3040-30FF,U+3100-312F,U+4E00-9FFF,U+F900-FAFF,U+FE30-FE4F,U+FF00-FFEF
PYTHONPATH="$DL/$FONTTOOLS" python3 "$REPO/tools/font-cjk.py" \
    "$OUT/data/fonts/DejaVuSans-Symbols.ttf" "$WORK/$NOTO_CJK" "$WORK/DejaVuSans-Symbols.ttf"
touch -r "$DL/$NOTO_CJK" "$OUT/data/fonts/DejaVuSans-Symbols.ttf"
cp -p "$DL/NotoSansSC-OFL.txt" "$OUT/data/fonts/NotoSansSC-OFL.txt"

# Plato's hyphenation patterns are prebuilt (kl-hyphenate bincode) and only
# published in its release archive; take them from the release that matches
# the kl-hyphenate version plato-core pins.
ZIP=plato-0.9.45.zip
fetch "$ZIP" "https://github.com/baskerville/plato/releases/download/0.9.45/$ZIP" \
    d89b828ff02ae2c835e14476be58b5475c612310a101dae00ad367935cece8cb
for lang in $HYPH_LANGS; do
    unzip -o -q -j "$DL/$ZIP" "hyphenation-patterns/$lang.standard.bincode" \
        -d "$OUT/data/hyphenation-patterns"
done

# Built-in StarDict dictionaries (e.g. MIT-licensed xinhua-zh-zh) cached in
# $ARISTOTLE_CACHE/dicts/ are bundled into data/dicts/ so fresh installs work
# offline out of the box.
if [ -d "$ARISTOTLE_CACHE/dicts" ]; then
    rm -rf "$OUT/data/dicts"
    mkdir -p "$OUT/data/dicts"
    cp -R "$ARISTOTLE_CACHE/dicts/." "$OUT/data/dicts/"
fi

# Licences. The patterns' authors and licences are in the headers of their
# hyph-utf8 master files, at the commit kl-hyphenate's README names; the
# bundle ships those files.
HYPH_REV=49706f9cfa97f6ead26b473ec10d23d5a651318a
HYPH_DL=hyph-utf8-49706f9
mkdir -p "$DL/$HYPH_DL"
for lang in $HYPH_LANGS; do
    case $lang in
        pl) sum=83ddbbd21bc06a44bc4f4a4c49c38782e809cff5f026b764c044daa29e818843 ;;
        en-us) sum=f4ffcd96c5cbc886bdad23f95dcae8edc3cd3620eae62f7946eceda97c4e68f8 ;;
        en-gb) sum=e95bb4ab350f620c41c231c4785d7bdb4f91949d3e631b30e08b3ead3c340a52 ;;
        grc) sum=dee55cf442ba4f4c5655622571ee796bede243a65ef2286858b28db7f1b40d78 ;;
        la) sum=84e0be1877beaf6d1d7a83d5639fb65a0e0645cd2447b0dbccfa5e63565abdfd ;;
        *) echo "no pinned hyph-utf8 notice for $lang; add its SHA-256 here" >&2; exit 1 ;;
    esac
    fetch "$HYPH_DL/hyph-$lang.tex" \
        "https://raw.githubusercontent.com/hyphenation/tex-hyphen/$HYPH_REV/hyph-utf8/tex/generic/hyph-utf8/patterns/tex/hyph-$lang.tex" \
        "$sum"
done
rm -rf "$OUT/licenses" "$OUT/SOURCE.txt"
DJVU_LINKED=0
[ ! -f "$B/plato/libs/libdjvulibre.a" ] || DJVU_LINKED=1
# The engine's libraries (crengine-ng and the four it needs) only have
# notices when the build links them; ENGINE_SRC and CRENGINE_DIR are empty
# otherwise.
PYTHONPATH="$DL/$FONTTOOLS" APP_FEATURES="$APP_FEATURES" python3 "$REPO/tools/bundle-licenses.py" \
    "$OUT" "$REPO" "$B/plato" "$TARGET" "$TOOLS/zig" "$DL/$HYPH_DL" "$DJVU_LINKED" \
    "$ENGINE_SRC" "$CRENGINE_DIR"

file "$OUT/aristotle-render" "$OUT/aristotle"
ls -l "$OUT/aristotle-render" "$OUT/aristotle"
du -sh "$OUT/data"

# The bundles put their files under "aristotle/". GNU tar renames the members
# with --transform; bsdtar (macOS) does the same with -s.
bundle_tar() {
    _out=$1
    shift
    if tar --version 2>/dev/null | grep -q 'GNU tar'; then
        tar -czf "$_out" -C "$OUT" --transform 's,^\.,aristotle,' "$@"
    else
        tar -czf "$_out" -C "$OUT" -s '|^\./|aristotle/|' "$@"
    fi
}

# Device bundles: binary plus data, rebuilt from what was just built so an
# upload can never carry a stale binary (it once did). One per binary,
# because both together unpack to about 60 MB and /var on the device is a
# 64 MB tmpfs. Unpack streamed:
#   ssh kindle 'tar -xzf - -C /var/tmp' < "$ARISTOTLE_CACHE/render/aristotle-render-device.tar.gz"
# The reader app's bundle (aristotle/ with the app, data/ and
# run-aristotle.sh) is meant for the userstore; see docs/reader.md:
#   ssh kindle 'tar -xzf - -C /mnt/us' < "$ARISTOTLE_CACHE/render/aristotle-device.tar.gz"
if [ "$WHAT" = device ]; then
    BUNDLE=${BUNDLE:-$ARISTOTLE_CACHE/render/aristotle-render-device.tar.gz}
    bundle_tar "$BUNDLE.part" ./aristotle-render ./data ./licenses ./SOURCE.txt
    mv -f "$BUNDLE.part" "$BUNDLE"
    APP_BUNDLE=${APP_BUNDLE:-$ARISTOTLE_CACHE/render/aristotle-device.tar.gz}
    cp "$REPO/device/app/run-aristotle.sh" "$REPO/device/app/takeover.sh" "$REPO/device/app/open-kual.sh" \
        "$REPO/device/extension/guard.sh" "$OUT/"
    bundle_tar "$APP_BUNDLE.part" ./aristotle ./data ./run-aristotle.sh ./takeover.sh \
        ./open-kual.sh ./guard.sh ./licenses ./SOURCE.txt
    mv -f "$APP_BUNDLE.part" "$APP_BUNDLE"
    ls -l "$BUNDLE" "$APP_BUNDLE"
fi
