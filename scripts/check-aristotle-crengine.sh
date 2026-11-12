#!/usr/bin/env bash
# Aristotle 的 crengine 排版路径的检查：fmt → clippy → test → 主程序带 crengine 编译。
#
# 与 scripts/check.sh 分开：这条路径要 crengine-ng 的源码（engine/vendor/），
# 还要 Plato 的 C 库，check.sh 在任何机器上都能跑，所以放在单独的脚本里，
# 跟 scripts/check-engine.sh 一个路子。
#
# 用法：
#   scripts/check-aristotle-crengine.sh
#
# 依赖：
# - engine/vendor/crengine-ng（engine/fetch.sh 拉下来）
# - Plato 的 C 库：设备构建用 PLATO_LIB_DIRS（tools/build-plato.sh 的产物）；
#   macOS 主机上用 $ARISTOTLE_CACHE/plato-libs（S-3 报告 §5 的做法）。
# - 主程序那一步按设备目标检查，要设备的 Plato C 库
#   （$ARISTOTLE_CACHE/plato-build/main，tools/build-plato.sh 的产物）和交叉
#   工具链（Zig，tools/engine-sysroot.sh 备的）。
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"

if ! command -v cargo >/dev/null 2>&1; then
  echo "[FAIL] 未找到 cargo，请先安装 Rust 工具链（https://rustup.rs）" >&2
  exit 127
fi

if ! command -v cmake >/dev/null 2>&1; then
  echo "[FAIL] 未找到 cmake，编译 crengine-ng 需要它" >&2
  exit 127
fi

if [ ! -d engine/vendor/koreader-crengine ]; then
  echo "[FAIL] 没有 engine/vendor/koreader-crengine，先跑 engine/fetch.sh" >&2
  exit 1
fi

# 跑一步检查：失败时打印失败的步骤，并以该步的退出码结束脚本。
run_step() {
  local label="$1"
  shift
  local status=0

  printf '\n==> %s\n' "$label"
  "$@" || status=$?
  if [ "$status" -ne 0 ]; then
    printf '\n[FAIL] %s 失败（退出码 %d）\n' "$label" "$status" >&2
    exit "$status"
  fi
  printf '[OK] %s\n' "$label"
}

# Plato 的 C 库在哪：设备构建自己设 PLATO_LIB_DIRS；macOS 主机上用
# $ARISTOTLE_CACHE/plato-libs 里的自编 wrapper 加上 Homebrew 的 mupdf。
: "${ARISTOTLE_CACHE:=${XDG_CACHE_HOME:-$HOME/.cache}/aristotle}"
export ARISTOTLE_CACHE

if [ -z "${PLATO_LIB_DIRS:-}" ] && [ -d "$ARISTOTLE_CACHE/plato-libs" ]; then
  export PLATO_LIB_DIRS="$ARISTOTLE_CACHE/plato-libs"
  export RUSTFLAGS="-L native=$ARISTOTLE_CACHE/plato-libs -L native=/opt/homebrew/lib --cfg plato_no_djvu${RUSTFLAGS:+ $RUSTFLAGS}"
fi

# Aristotle 是 subtree 导入的独立 workspace，按上游约定单独检查。
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ARISTOTLE_CACHE/target-aristotle-crengine}"

pkg_args=(-p aristotle-render --no-default-features --features crengine)

# 在 Aristotle 的 workspace 里跑一步检查。
run_aristotle_step() {
  local label="$1"
  shift
  local status=0

  printf '\n==> %s\n' "$label"
  (
    cd -- "$repo_root/aristotle" || exit 1
    "$@"
  ) || status=$?
  if [ "$status" -ne 0 ]; then
    printf '\n[FAIL] %s 失败（退出码 %d）\n' "$label" "$status" >&2
    exit "$status"
  fi
  printf '[OK] %s\n' "$label"
}

# cargo fmt 不收 feature 参数，但会把这个包的所有文件都过一遍。
run_aristotle_step "aristotle: cargo fmt --check (crengine)" \
  cargo fmt -p aristotle-render -- --check
run_aristotle_step "aristotle: cargo clippy -D warnings (crengine)" \
  cargo clippy --config .cargo/config.toml "${pkg_args[@]}" --all-targets -- -D warnings
run_aristotle_step "aristotle: cargo test (crengine)" \
  cargo test --config .cargo/config.toml "${pkg_args[@]}" -- --test-threads=1

# The reader app with the crengine engine. The app opens a book on a worker
# thread and hands the document to the main thread, so the engine's Document
# trait has to be Send: without that bound the app does not compile at all
# (docs/tasks/BUILD-002/notes.md §6). The app crate links the HAL, and the
# HAL uses Linux-only libc calls, so it does not build on macOS
# (docs/tasks/APP-002/notes.md). This step checks it for the device target
# instead: armv7-unknown-linux-musleabihf, in the same environment as
# tools/build-render.sh device (the Zig cross compiler, the device's Plato C
# libraries, the engine's cross sysroot).
#
# RUSTFLAGS is dropped for this step: the host steps above set it to the
# host's Plato libraries, and the device build brings its own flags through
# .cargo/config.toml.
run_aristotle_step "aristotle: cargo check -p aristotle --features crengine (device)" \
  env -u RUSTFLAGS CARGO_CMD=check tools/build-render.sh device
run_aristotle_step "aristotle: cargo check --tests -p aristotle --features crengine (device)" \
  env -u RUSTFLAGS CARGO_CMD=check CARGO_FLAGS=--tests tools/build-render.sh device

printf '\n[OK] 全部检查通过\n'
