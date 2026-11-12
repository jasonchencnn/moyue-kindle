#!/usr/bin/env bash
# crengine-ng 相关的检查：拉源码 → clippy → test。
#
# 与 scripts/check.sh 分开：默认 feature 下不需要引擎源码，
# 所以 check.sh 在任何机器上都能跑；check-engine.sh 需要能编译 C++ 引擎。
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

run_step "engine/fetch.sh（拉取 crengine-ng 源码）" \
  bash engine/fetch.sh
run_step "cargo clippy -p inkread-engine --features crengine -- -D warnings" \
  cargo clippy -p inkread-engine --features crengine --all-targets -- -D warnings
run_step "cargo test -p inkread-engine --features crengine" \
  cargo test -p inkread-engine --features crengine

printf '\n[OK] 全部检查通过\n'
