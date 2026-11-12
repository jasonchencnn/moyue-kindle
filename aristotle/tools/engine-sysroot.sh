# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Aristotle contributors
# The cross sysroot the reading engine (crengine-ng) links against.
#
# Source this file, do not run it:
#
#   . tools/env.sh
#   . tools/engine-sysroot.sh
#   engine_sysroot_build
#
# The caller sets REPO (the Aristotle subtree directory) and has sourced
# tools/env.sh. The engine's own crate lives in the repository above it
# (engine/, platform/).
#
# Zig is the cross C/C++ compiler (musl, hard float, Cortex-A9 + NEON), the
# same toolchain tools/build-plato.sh uses. The C libraries the device package
# already ships (zlib, libpng, libjpeg, FreeType, HarfBuzz) are reused as
# they are, so a binary that links both engines ends up with one copy of
# each; the symbol analysis is in docs/tasks/ENG-005/notes.md. The four
# libraries Plato does not have (FriBiDi, libunibreak, utf8proc, zstd) are
# cross-built here.
#
# Everything lands in $ENGINE_BUILD (default $ARISTOTLE_CACHE/engine-build):
#   downloads/  the source tarballs, SHA-256 checked
#   src/        the unpacked sources
#   libs/       the static archives this builds
#   sysroot/    usr/include + usr/lib: the cross sysroot the engine links to
#   bin/        the compiler wrappers
#   stamps/     one stamp per finished step, so the build is restartable
#
# engine_sysroot_build exports:
#   INKREAD_ENGINE_CC/CXX/AR/RANLIB   the Zig wrappers
#   INKREAD_ENGINE_SYSROOT             the sysroot to link against
#   INKREAD_ENGINE_TOOLCHAIN           the CMake toolchain file
#   ENGINE_SYSROOT, ENGINE_LIBS, ENGINE_SRC, ENGINE_DOWNLOADS
#
# and the cargo environment the cross build needs (the linker, the static
# Rust flags and the cc crate's compiler).
#
# Every name defined here is prefixed with engine_, so that sourcing the file
# does not disturb the caller: build-render.sh has its own fetch().

# --- The Zig toolchain --------------------------------------------------------

# The official packages are per host; the ones for the host Aristotle was
# built with are reused, so a device build does not download a second
# toolchain. The digests are from https://ziglang.org/download/index.json.
engine_zig_version=0.17.0
case "$(uname -s)/$(uname -m)" in
    Darwin/arm64)  engine_zig_target=aarch64-macos; engine_zig_sha256=b607e9b9234790a008116ae5bdb71c6243b84b9fb42a53a9e70fde41c06c536a ;;
    Darwin/x86_64) engine_zig_target=x86_64-macos; engine_zig_sha256=4f9a1c5269aa17ebda5e6d3c2b89d6cbf36f7d2b22a0306e9ab98f25f95529c6 ;;
    Linux/x86_64)  engine_zig_target=x86_64-linux; engine_zig_sha256=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026 ;;
    Linux/aarch64|Linux/arm64) engine_zig_target=aarch64-linux; engine_zig_sha256=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8 ;;
    *) echo "no official Zig package for $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac
engine_zig_tarball=zig-$engine_zig_target-$engine_zig_version.tar.xz

engine_zig_setup() {
    engine_zz_dir=$1
    engine_zz_dl=$2
    engine_zz_tools=$3
    engine_zz_zig=$engine_zz_tools/zig/zig
    if [ ! -x "$engine_zz_zig" ] || \
       [ "$("$engine_zz_zig" version 2>/dev/null || true)" != "$engine_zig_version" ] || \
       [ "$(cat "$engine_zz_tools/zig/.target" 2>/dev/null || true)" != "$engine_zig_target" ]; then
        echo "=== Zig $engine_zig_version ($engine_zig_target)"
        engine_zz_f=$engine_zz_dl/$engine_zig_tarball
        if [ ! -f "$engine_zz_f" ] || ! sha256_matches "$engine_zig_sha256" "$engine_zz_f"; then
            curl -fL --retry 3 -o "$engine_zz_f.part" \
                "https://ziglang.org/download/$engine_zig_version/$engine_zig_tarball"
            sha256_matches "$engine_zig_sha256" "$engine_zz_f.part" || {
                echo "checksum mismatch: $engine_zig_tarball" >&2
                exit 1
            }
            mv "$engine_zz_f.part" "$engine_zz_f"
        fi
        # A toolchain for another host cannot run here; drop it. One for this
        # host is kept, so a wrong version is not overwritten behind the
        # user's back.
        if [ -e "$engine_zz_tools/zig" ] && \
           [ "$(cat "$engine_zz_tools/zig/.target" 2>/dev/null || true)" != "$engine_zig_target" ]; then
            rm -rf "$engine_zz_tools/zig"
        fi
        [ ! -e "$engine_zz_tools/zig" ] || {
            echo "$engine_zz_tools/zig exists but is not Zig $engine_zig_version; move it away" >&2
            exit 1
        }
        mkdir "$engine_zz_tools/zig"
        tar -xJf "$engine_zz_f" -C "$engine_zz_tools/zig" --strip-components=1
        printf '%s\n' "$engine_zig_target" > "$engine_zz_tools/zig/.target"
    fi
    ENGINE_ZIG=$engine_zz_zig
}

# --- The compiler wrappers ----------------------------------------------------

# Zig only accepts its own CPU names (cortex_a9), so map the GCC spelling
# that the cc crate uses (-mcpu=cortex-a9), and drop the Rust-style
# --target=armv7-... that the cc crate adds when it detects clang. The
# -march=... and -mfpu=... that come with it are dropped as well: Zig's
# target already picks the CPU, and the Kindle wants NEON, not the
# lowest common denominator FPU the cc crate asks for.
engine_wrappers() {
    engine_wr_dir=$1
    engine_wr_zig=$2
    engine_wr_host=$3
    engine_wr_cpu=$4
    for engine_wr_tool in cc c++; do
        engine_wr_w=$engine_wr_dir/arm-$engine_wr_tool
        cat > "$engine_wr_w" <<EOF
#!/bin/sh
for a do
    shift
    case \$a in
        --target=*) continue ;;
        -march=*|-mfpu=*) continue ;;
        -mcpu=*) a=-mcpu=\$(printf %s "\${a#-mcpu=}" | tr - _) ;;
    esac
    set -- "\$@" "\$a"
done
exec "$engine_wr_zig" $engine_wr_tool -target $engine_wr_host $engine_wr_cpu "\$@"
EOF
        chmod +x "$engine_wr_w"
    done
    for engine_wr_tool in ar ranlib; do
        engine_wr_w=$engine_wr_dir/arm-$engine_wr_tool
        printf '#!/bin/sh\nexec "%s" %s "$@"\n' "$engine_wr_zig" "$engine_wr_tool" > "$engine_wr_w"
        chmod +x "$engine_wr_w"
    done
}

# --- The sources --------------------------------------------------------------

# engine_fetch DIR NAME URL SHA256
engine_fetch() {
    engine_ft_f="$1/$2"
    if [ ! -f "$engine_ft_f" ] || ! sha256_matches "$4" "$engine_ft_f"; then
        curl -fL --retry 3 -o "$engine_ft_f.part" "$3"
        sha256_matches "$4" "$engine_ft_f.part" || {
            echo "checksum mismatch: $2" >&2
            exit 1
        }
        mv "$engine_ft_f.part" "$engine_ft_f"
    fi
}

# --- The whole thing ----------------------------------------------------------

# Build the cross sysroot and export the environment the engine's build.rs
# reads. Idempotent: the finished steps leave a stamp, so a second call is
# quick.
engine_sysroot_build() {
    : "${REPO:?set REPO (the Aristotle subtree directory) before engine_sysroot_build}"
    engine_root=$(cd "$REPO/.." && pwd)
    engine_host=arm-linux-musleabihf
    engine_cpu="-mcpu=cortex_a9 -mfpu=neon"

    ENGINE_BUILD=${ENGINE_BUILD:-$ARISTOTLE_CACHE/engine-build}
    engine_tools=${ARISTOTLE_TOOLS:-$ARISTOTLE_CACHE/tools}
    engine_dl=$ENGINE_BUILD/downloads
    engine_src=$ENGINE_BUILD/src
    engine_obj=$ENGINE_BUILD/obj
    engine_libs=$ENGINE_BUILD/libs
    engine_sysroot=$ENGINE_BUILD/sysroot
    engine_jobs=${JOBS:-2}
    # The device package's C libraries, built by tools/build-plato.sh.
    # Read-only.
    engine_plato=${PLATO_BUILD:-$ARISTOTLE_CACHE/plato-build/main}
    engine_plato_libs=$engine_plato/plato/libs
    engine_plato_tp=$engine_plato/plato/thirdparty

    mkdir -p "$engine_dl" "$engine_src" "$engine_obj" "$engine_libs" \
        "$engine_sysroot/usr/include" "$engine_sysroot/usr/lib" \
        "$ENGINE_BUILD/stamps" "$ENGINE_BUILD/bin"
    # Zig's own cache, so the cross builds do not fill the home directory.
    export ZIG_GLOBAL_CACHE_DIR=${ZIG_GLOBAL_CACHE_DIR:-$ENGINE_BUILD/zig-cache}
    export ZIG_LOCAL_CACHE_DIR=$ZIG_GLOBAL_CACHE_DIR

    engine_zig_setup "$ENGINE_BUILD" "$engine_dl" "$engine_tools"
    engine_wrappers "$ENGINE_BUILD/bin" "$ENGINE_ZIG" "$engine_host" "$engine_cpu"
    engine_xcc=$ENGINE_BUILD/bin/arm-cc
    engine_xcxx=$ENGINE_BUILD/bin/arm-c++
    engine_xar=$ENGINE_BUILD/bin/arm-ar
    engine_xranlib=$ENGINE_BUILD/bin/arm-ranlib

    # name file url sha256. The versions are the ones the macOS build links
    # (Homebrew), so the device and the host run the same code.
    while read -r engine_name engine_file engine_url engine_sha; do
        engine_fetch "$engine_dl" "$engine_file" "$engine_url" "$engine_sha"
        if [ ! -f "$ENGINE_BUILD/stamps/src-$engine_name" ]; then
            [ ! -e "$engine_src/$engine_name" ] || {
                echo "$engine_src/$engine_name exists without a stamp; move it away" >&2
                exit 1
            }
            mkdir -p "$engine_src/$engine_name"
            tar -xf "$engine_dl/$engine_file" --strip-components=1 -C "$engine_src/$engine_name"
            touch "$ENGINE_BUILD/stamps/src-$engine_name"
        fi
    done <<'EOF'
fribidi fribidi-1.0.16.tar.xz https://github.com/fribidi/fribidi/releases/download/v1.0.16/fribidi-1.0.16.tar.xz 1b1cde5b235d40479e91be2f0e88a309e3214c8ab470ec8a2744d82a5a9ea05c
unibreak libunibreak-6.1.tar.gz https://github.com/adah1972/libunibreak/releases/download/libunibreak_6_1/libunibreak-6.1.tar.gz cc4de0099cf7ff05005ceabff4afed4c582a736abc38033e70fdac86335ce93f
utf8proc utf8proc-2.11.3.tar.gz https://github.com/JuliaStrings/utf8proc/releases/download/v2.11.3/utf8proc-2.11.3.tar.gz 415189fd2c85cd6ee5ff26af500fa387de9ada1e3e316e93f7338551481d557d
zstd zstd-1.5.7.tar.gz https://github.com/meta/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3
EOF

    # FriBiDi: bidi (RTL) support for crengine-ng.
    if [ ! -f "$ENGINE_BUILD/stamps/fribidi" ]; then
        echo "=== fribidi"
        (cd "$engine_src/fribidi" && ./configure --host=$engine_host --disable-shared --enable-static \
            CC="$engine_xcc" CXX="$engine_xcxx" AR="$engine_xar" RANLIB="$engine_xranlib" CFLAGS="-O2" \
            && make -j"$engine_jobs" -C lib)
        cp "$engine_src/fribidi/lib/.libs/libfribidi.a" "$engine_libs/"
        touch "$ENGINE_BUILD/stamps/fribidi"
    fi

    # libunibreak: line breaking for crengine-ng.
    if [ ! -f "$ENGINE_BUILD/stamps/unibreak" ]; then
        echo "=== unibreak"
        (cd "$engine_src/unibreak" && ./configure --host=$engine_host --disable-shared --enable-static \
            CC="$engine_xcc" AR="$engine_xar" RANLIB="$engine_xranlib" CFLAGS="-O2" \
            && make -j"$engine_jobs")
        cp "$engine_src/unibreak/src/.libs/libunibreak.a" "$engine_libs/"
        touch "$ENGINE_BUILD/stamps/unibreak"
    fi

    # utf8proc: string comparison for crengine-ng. The library is one
    # translation unit (utf8proc.c includes utf8proc_data.c).
    if [ ! -f "$ENGINE_BUILD/stamps/utf8proc" ]; then
        echo "=== utf8proc"
        mkdir -p "$engine_obj/utf8proc"
        (cd "$engine_src/utf8proc" && "$engine_xcc" -O2 -fPIC -DUTF8PROC_STATIC -c utf8proc.c \
            -o "$engine_obj/utf8proc/utf8proc.o")
        "$engine_xar" rcs "$engine_libs/libutf8proc.a" "$engine_obj/utf8proc/utf8proc.o"
        touch "$ENGINE_BUILD/stamps/utf8proc"
    fi

    # zstd: compression of crengine-ng's document cache.
    if [ ! -f "$ENGINE_BUILD/stamps/zstd" ]; then
        echo "=== zstd"
        (cd "$engine_src/zstd/lib" && make -j"$engine_jobs" libzstd.a \
            CC="$engine_xcc" AR="$engine_xar" RANLIB="$engine_xranlib" \
            HAVE_THREAD=0 ZSTD_LEGACY_SUPPORT=0 HAVE_ZLIB=0 CFLAGS="-O2 -fPIC")
        cp "$engine_src/zstd/lib/libzstd.a" "$engine_libs/"
        touch "$ENGINE_BUILD/stamps/zstd"
    fi

    # The C++ runtime: Zig builds libc++/libc++abi for the target on the first
    # C++ link; pick them out of its cache. libunwind and libc come from
    # Rust's self-contained musl. Same trick as tools/build-plato.sh.
    if [ ! -f "$ENGINE_BUILD/stamps/libcxx" ]; then
        echo "=== libc++ runtime"
        mkdir -p "$engine_obj/cxxrt"
        printf 'int main(void){return 0;}\n' > "$engine_obj/cxxrt/main.cpp"
        $engine_xcxx -O2 -static "$engine_obj/cxxrt/main.cpp" -o "$engine_obj/cxxrt/main" \
            --verbose 2>> "$engine_obj/cxxrt/link.log"
        for engine_lib in libc++.a libc++abi.a; do
            engine_path=$(tr ' ' '\n' < "$engine_obj/cxxrt/link.log" | grep "/$engine_lib\$" | tail -n 1)
            [ -n "$engine_path" ] || { echo "could not find $engine_lib from zig" >&2; exit 1; }
            cp "$engine_path" "$engine_libs/$engine_lib"
        done
        touch "$ENGINE_BUILD/stamps/libcxx"
    fi

    # --- The sysroot ---------------------------------------------------------
    # usr/include + usr/lib, the layout CMake's find modules expect for a
    # cross-compiled target. The archives and the headers of the libraries
    # Plato already ships for the device are taken from its build tree, so
    # the engine links the very same FreeType and HarfBuzz as the reader
    # does.
    echo "=== assemble the sysroot"
    [ -d "$engine_plato_libs" ] || {
        echo "$engine_plato_libs not found; run tools/build-plato.sh first" >&2
        exit 1
    }
    engine_inc=$engine_sysroot/usr/include
    engine_lib=$engine_sysroot/usr/lib

    mkdir -p "$engine_inc/freetype2/freetype" "$engine_inc/harfbuzz" "$engine_inc/fribidi"

    # zlib, libpng, libjpeg: headers and archives from Plato's build tree.
    cp -f "$engine_plato_tp/zlib/zlib.h" "$engine_plato_tp/zlib/zconf.h" "$engine_inc/"
    cp -f "$engine_plato_tp/libpng/png.h" "$engine_plato_tp/libpng/pngconf.h" \
          "$engine_plato_tp/libpng/pnglibconf.h" "$engine_inc/"
    cp -f "$engine_plato_tp/libjpeg/jpeglib.h" "$engine_plato_tp/libjpeg/jconfig.h" \
          "$engine_plato_tp/libjpeg/jmorecfg.h" "$engine_plato_tp/libjpeg/jerror.h" "$engine_inc/"
    cp -f "$engine_plato_libs/libz.a" "$engine_plato_libs/libpng16.a" "$engine_plato_libs/libjpeg.a" "$engine_lib/"

    # FreeType: ft2build.h next to the freetype/ directory, the usual layout.
    cp -f "$engine_plato_tp/freetype2/include/ft2build.h" "$engine_inc/freetype2/"
    cp -Rf "$engine_plato_tp/freetype2/include/freetype/." "$engine_inc/freetype2/freetype/"
    cp -f "$engine_plato_libs/libfreetype.a" "$engine_lib/"

    # HarfBuzz: the public headers, all in one directory.
    cp -f "$engine_plato_tp/harfbuzz/src/"*.h "$engine_inc/harfbuzz/"
    cp -f "$engine_plato_libs/libharfbuzz.a" "$engine_lib/"

    # FriBiDi, libunibreak, utf8proc, zstd: built above.
    cp -f "$engine_src/fribidi/lib/"*.h "$engine_src/fribidi/lib/fribidi-config.h" "$engine_inc/fribidi/"
    cp -f "$engine_libs/libfribidi.a" "$engine_lib/"
    cp -f "$engine_src/unibreak/src/linebreak.h" "$engine_src/unibreak/src/linebreakdef.h" \
          "$engine_src/unibreak/src/unibreakbase.h" "$engine_src/unibreak/src/unibreakdef.h" \
          "$engine_src/unibreak/src/eastasianwidthdef.h" "$engine_src/unibreak/src/graphemebreak.h" \
          "$engine_src/unibreak/src/wordbreak.h" "$engine_inc/"
    cp -f "$engine_libs/libunibreak.a" "$engine_lib/"
    cp -f "$engine_src/utf8proc/utf8proc.h" "$engine_inc/"
    cp -f "$engine_libs/libutf8proc.a" "$engine_lib/"
    cp -f "$engine_src/zstd/lib/zstd.h" "$engine_src/zstd/lib/zstd_errors.h" "$engine_inc/"
    cp -f "$engine_libs/libzstd.a" "$engine_lib/"

    # The C++ runtime.
    cp -f "$engine_libs/libc++.a" "$engine_libs/libc++abi.a" "$engine_lib/"

    # --- The environment the engine's build.rs reads -------------------------
    export INKREAD_ENGINE_CC="$engine_xcc"
    export INKREAD_ENGINE_CXX="$engine_xcxx"
    export INKREAD_ENGINE_AR="$engine_xar"
    export INKREAD_ENGINE_RANLIB="$engine_xranlib"
    export INKREAD_ENGINE_SYSROOT="$engine_sysroot"
    export INKREAD_ENGINE_TOOLCHAIN="$engine_root/engine/cmake/armv7-linux-musle.cmake"
    # rust-lld and a static binary, the same way the device package is linked.
    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld
    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="-C target-feature=+crt-static"
    # The cc crate builds the shim's archive: let it use Zig's ar, not the
    # host's.
    export AR_armv7_unknown_linux_musleabihf="$engine_xar"
    export RANLIB_armv7_unknown_linux_musleabihf="$engine_xranlib"
    # The cc crate looks for a target-prefixed compiler by name; point it at
    # Zig's wrappers so it does not hunt for arm-linux-musleabihf-gcc.
    export CC_armv7_unknown_linux_musleabihf="$engine_xcc"
    export CXX_armv7_unknown_linux_musleabihf="$engine_xcxx"
    # Zig's C++ runtime is libc++, not libstdc++. The cc crate picks
    # libstdc++ for a Linux target unless it can tell the compiler uses
    # libc++ (the cc version Aristotle's Cargo.lock pins cannot), so say
    # it: the sysroot has libc++.a. Only for this target, so a host build
    # keeps its own choice.
    export CXXSTDLIB_armv7_unknown_linux_musleabihf=c++
    # CMake's generated Makefiles read NUM_JOBS.
    export NUM_JOBS=$engine_jobs

    ENGINE_SYSROOT=$engine_sysroot
    ENGINE_LIBS=$engine_libs
    ENGINE_SRC=$engine_src
    ENGINE_DOWNLOADS=$engine_dl
    ENGINE_BUILD_DIR=$ENGINE_BUILD
    ENGINE_ROOT=$engine_root
}
