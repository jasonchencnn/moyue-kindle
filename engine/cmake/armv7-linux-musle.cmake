# Cross toolchain for the Kindle: armv7 musl, hard float, static.
#
# The compiler is the Zig wrapper that aristotle/tools/build-engine.sh
# generates; it already carries the target and the CPU flags. The paths come
# from the cache variables engine/build.rs passes on the cmake command line.
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR arm)

set(CMAKE_C_COMPILER "${INKREAD_ENGINE_CC}")
set(CMAKE_CXX_COMPILER "${INKREAD_ENGINE_CXX}")
set(CMAKE_AR "${INKREAD_ENGINE_AR}")
set(CMAKE_RANLIB "${INKREAD_ENGINE_RANLIB}")

# The configure step only builds a static library; the device binary is
# linked by rustc, so CMake must not try to link an executable here.
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

# Headers and libraries come from the cross sysroot, never from the host:
# a macOS libpng would not link for armv7.
set(CMAKE_FIND_ROOT_PATH "${INKREAD_ENGINE_SYSROOT}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

# crengine-ng's own find modules (harfbuzz, FriBidi, libunibreak, zstd,
# utf8proc) read the version from the headers in the sysroot.
set(PKG_CONFIG_LIBDIR "${INKREAD_ENGINE_SYSROOT}/usr/lib/pkgconfig")
set(PKG_CONFIG_PATH "")
