//! 构建 koreader/crengine 静态库并编译 C 接口 shim。
//!
//! 只在启用 `crengine` feature 时才干活：不开 feature 时这个脚本什么都不做，
//! `cc` / `cmake` 这两个构建依赖也不会被拉进来，这样没有引擎源码的机器
//! 照常能 `cargo build` / `cargo test`。
//!
//! 步骤：
//! 1. 用 `cmake` crate 构建 `cmake/koreader-crengine`（静态库，灰度后缓冲）。
//! 2. 用 `cc` crate 编译 `shim.cpp`，链接进静态库。
//! 3. 告诉 rustc 去哪里找库、还需要哪些系统库。
//!
//! 两条路：
//! - 宿主（macOS）：和以前一样，动态链接 Homebrew 的 zlib/png/jpeg/
//!   freetype/harfbuzz/fribidi/unibreak/utf8proc/zstd。
//! - 交叉编译（armv7 musl）：`aristotle/tools/build-engine.sh` 准备好 Zig
//!   编译器和一套交叉 sysroot，这里全部静态链接。见
//!   `docs/tasks/ENG-005/notes.md` 与 ADR-015。
//!
//! 源码由 `fetch.sh` 拉取，`vendor/` 不进版本库。
//! 宏由 `cmake/koreader-crengine/crsetup.h` 统一管理，shim 与库使用完全同一份宏。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for var in [
        "INKREAD_ENGINE_SYSROOT",
        "INKREAD_ENGINE_TOOLCHAIN",
        "INKREAD_ENGINE_CC",
        "INKREAD_ENGINE_CXX",
        "INKREAD_ENGINE_AR",
        "INKREAD_ENGINE_RANLIB",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    #[cfg(feature = "crengine")]
    build_crengine();
}

/// 构建 koreader/crengine 静态库与 shim。
#[cfg(feature = "crengine")]
fn build_crengine() {
    use std::env;

    let target = env::var("TARGET").expect("TARGET should be set by cargo");
    let host = env::var("HOST").expect("HOST should be set by cargo");
    if target == host {
        build_host();
    } else {
        build_cross(&target);
    }
}

/// 宿主构建：动态链接 Homebrew 的库。
#[cfg(feature = "crengine")]
fn build_host() {
    use std::env;
    use std::path::PathBuf;

    /// macOS Homebrew 前缀。可用 `INKREAD_BREW_PREFIX` 覆盖（Intel Mac 是
    /// `/usr/local`，Linux 是 `/usr`）。
    fn brew_prefix() -> String {
        env::var("INKREAD_BREW_PREFIX").unwrap_or_else(|_| "/opt/homebrew".to_string())
    }

    /// koreader/crengine 用到的第三方库，静态库不会自动传播依赖，需要手工链接。
    /// 顺序无所谓（都是动态库），但必须在 koreader_crengine 之后。
    const SYSTEM_LIBS: &[&str] = &[
        "z",        // zlib
        "png",      // libpng
        "jpeg",     // jpeg
        "freetype", // freetype
        "harfbuzz", // harfbuzz
        "fribidi",  // fribidi
        "unibreak", // libunibreak
        "utf8proc", // utf8proc
        "zstd",     // zstd
    ];

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by cargo"),
    );
    let vendor = manifest_dir.join("vendor/koreader-crengine");
    let cmake_dir = manifest_dir.join("cmake/koreader-crengine");
    let crsetup_h = cmake_dir.join("crsetup.h");
    let brew = brew_prefix();

    assert!(
        vendor.join("CMakeLists.txt").exists(),
        "找不到 vendor/koreader-crengine，请先运行 engine/fetch.sh"
    );

    println!("cargo:rerun-if-changed=shim.cpp");
    println!("cargo:rerun-if-changed=shim.h");
    println!("cargo:rerun-if-changed=assets/default.css");
    println!("cargo:rerun-if-changed=vendor/koreader-crengine");
    println!("cargo:rerun-if-changed=cmake/koreader-crengine/CMakeLists.txt");
    println!("cargo:rerun-if-changed=cmake/koreader-crengine/crsetup.h");
    println!("cargo:rerun-if-changed=patches");

    // 1. 构建 koreader_crengine 静态库。
    let mut config = cmake::Config::new(&cmake_dir);
    config
        .define("CMAKE_BUILD_TYPE", "Release")
        .define("CMAKE_PREFIX_PATH", &brew)
        .define(
            "EXTRA_INCLUDES",
            format!("{brew}/include;{brew}/include/freetype2;{brew}/include/harfbuzz;{brew}/include/fribidi"),
        )
        .env("PKG_CONFIG_PATH", format!("{brew}/lib/pkgconfig"))
        .env("CMAKE_PREFIX_PATH", &brew);
    let dst = config.build();

    let include_dir = vendor.join("crengine/include");
    let xxhash_dir = vendor.join("thirdparty/xxhash");

    // 2. 编译 shim。通过 -include 强制引入 crsetup.h，保证 shim 与静态库的宏 100% 相同。
    let mut shim = cc::Build::new();
    shim.cpp(true)
        .file(manifest_dir.join("shim.cpp"))
        .include(&include_dir)
        .include(&xxhash_dir)
        .include(&cmake_dir)
        .include(dst.join("include"))
        .include(format!("{brew}/include"))
        .include(format!("{brew}/include/freetype2"))
        .include(format!("{brew}/include/harfbuzz"))
        .include(format!("{brew}/include/fribidi"))
        .flag_if_supported("-std=c++17")
        .flag("-include")
        .flag(crsetup_h.to_str().expect("valid crsetup path"))
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-reorder-ctor")
        .flag_if_supported("-Wno-ignored-qualifiers")
        .compile("crengine_shim");

    // 3. 链接。
    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=koreader_crengine");
    for lib in SYSTEM_LIBS {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    // macOS 的 C++ 运行时
    println!("cargo:rustc-link-lib=dylib=c++");
    // Homebrew 的库不在默认搜索路径里
    println!("cargo:rustc-link-search=native={brew}/lib");
}

/// 交叉构建：Zig 当编译器，链接交叉 sysroot 里的静态库。
#[cfg(feature = "crengine")]
fn build_cross(target: &str) {
    use std::env;
    use std::path::PathBuf;

    /// 交叉编译时静态链接的库。顺序重要：koreader_crengine 先，它依赖的库在后，
    /// 依赖彼此的顺序也要对（harfbuzz 用 freetype，png16 和 freetype 用
    /// zlib，libc++ 用 libc++abi）。
    const STATIC_LIBS: &[&str] = &[
        "freetype", "harfbuzz", "png16", "jpeg", "z", "fribidi", "unibreak", "utf8proc", "zstd",
        "c++", "c++abi",
    ];

    /// 交叉编译的编译参数。Cortex-A9 + NEON，和设备包里的 Plato 一样。
    const CROSS_C_FLAGS: &str =
        "-O2 -ffunction-sections -fdata-sections -fPIC -mcpu=cortex_a9 -mfpu=neon";

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by cargo"),
    );
    let vendor = manifest_dir.join("vendor/koreader-crengine");
    let cmake_dir = manifest_dir.join("cmake/koreader-crengine");
    let crsetup_h = cmake_dir.join("crsetup.h");

    assert!(
        vendor.join("CMakeLists.txt").exists(),
        "找不到 vendor/koreader-crengine，请先运行 engine/fetch.sh"
    );

    /// 交叉编译需要的路径，都由 `aristotle/tools/build-engine.sh` 设好。
    fn setting(name: &str) -> String {
        env::var(name).unwrap_or_else(|_| {
            panic!("{name} 没有设置；交叉编译请先运行 aristotle/tools/build-engine.sh")
        })
    }

    let sysroot = PathBuf::from(setting("INKREAD_ENGINE_SYSROOT"));
    let toolchain = setting("INKREAD_ENGINE_TOOLCHAIN");
    let cc_path = setting("INKREAD_ENGINE_CC");
    let cxx_path = setting("INKREAD_ENGINE_CXX");
    let ar_path = setting("INKREAD_ENGINE_AR");
    let ranlib_path = setting("INKREAD_ENGINE_RANLIB");

    println!("cargo:rerun-if-changed=shim.cpp");
    println!("cargo:rerun-if-changed=shim.h");
    println!("cargo:rerun-if-changed=assets/default.css");
    println!("cargo:rerun-if-changed=vendor/koreader-crengine");
    println!("cargo:rerun-if-changed=cmake/koreader-crengine/CMakeLists.txt");
    println!("cargo:rerun-if-changed=cmake/koreader-crengine/crsetup.h");
    println!("cargo:rerun-if-changed=patches");
    println!("cargo:rerun-if-changed={}", toolchain);

    let sysroot_include = sysroot.join("usr/include");

    // 1. 构建 koreader_crengine 静态库。CMake 通过工具链文件找到 Zig 和 sysroot。
    let mut config = cmake::Config::new(&cmake_dir);
    config
        .define("CMAKE_BUILD_TYPE", "Release")
        .define("CMAKE_TOOLCHAIN_FILE", &toolchain)
        .define("CMAKE_C_FLAGS", CROSS_C_FLAGS)
        .define("CMAKE_CXX_FLAGS", CROSS_C_FLAGS)
        .define("INKREAD_ENGINE_CC", &cc_path)
        .define("INKREAD_ENGINE_CXX", &cxx_path)
        .define("INKREAD_ENGINE_AR", &ar_path)
        .define("INKREAD_ENGINE_RANLIB", &ranlib_path)
        .define("INKREAD_ENGINE_SYSROOT", &sysroot)
        .define(
            "EXTRA_INCLUDES",
            format!(
                "{};{}/freetype2;{}/harfbuzz;{}/fribidi",
                sysroot_include.display(),
                sysroot_include.display(),
                sysroot_include.display(),
                sysroot_include.display(),
            ),
        );
    let dst = config.build();

    let include_dir = vendor.join("crengine/include");
    let xxhash_dir = vendor.join("thirdparty/xxhash");

    // 2. 编译 shim。
    let mut shim = cc::Build::new();
    shim.cpp(true)
        .target(target)
        .compiler(&cxx_path)
        .file(manifest_dir.join("shim.cpp"))
        .include(&include_dir)
        .include(&xxhash_dir)
        .include(&cmake_dir)
        .include(dst.join("include"))
        .include(&sysroot_include)
        .include(sysroot_include.join("freetype2"))
        .include(sysroot_include.join("harfbuzz"))
        .include(sysroot_include.join("fribidi"))
        .flag_if_supported("-std=c++17")
        .flag("-include")
        .flag(crsetup_h.to_str().expect("valid crsetup path"))
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-reorder-ctor")
        .flag_if_supported("-Wno-ignored-qualifiers")
        .compile("crengine_shim");

    // 3. 链接。全部静态：sysroot 里的库，加上 Zig 的 C++ 运行时。
    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=koreader_crengine");
    println!(
        "cargo:rustc-link-search=native={}",
        sysroot.join("usr/lib").display()
    );
    for lib in STATIC_LIBS {
        println!("cargo:rustc-link-lib=static={lib}");
    }
}
