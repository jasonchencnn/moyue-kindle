#!/usr/bin/env bash
# 本地与 CI 共用的检查入口：fmt → clippy → test。
# 对应 AGENTS.md「必须遵守」2。三步全过才算通过，任一步失败立即退出非零并打印失败步骤。
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"

if ! command -v cargo >/dev/null 2>&1; then
  echo "[FAIL] 未找到 cargo，请先安装 Rust 工具链（https://rustup.rs）" >&2
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

run_step "cargo fmt --all -- --check" \
  cargo fmt --all -- --check
run_step "cargo clippy --workspace --all-targets -- -D warnings" \
  cargo clippy --workspace --all-targets -- -D warnings
run_step "cargo test --workspace" \
  cargo test --workspace

# Aristotle 是 subtree 导入的独立 workspace，按上游约定单独检查：
# 先 source tools/env.sh，构建目录放在 $ARISTOTLE_CACHE/target。
# 只检查在 macOS 上能编译的 crate。aristotle-hal、aristotle-render 和
# aristotle 依赖 Linux 专有的硬件接口与 Plato C 库，只能在 Linux 上编译，
# 因此不在本脚本内检查。
# 上游代码的 clippy 告警不修改，用精确的 -A 放行，并在
# docs/tasks/APP-001/notes.md 记录（本次检查无告警）。
#
# 两条另外的检查，各自需要别的东西，所以是单独的脚本：
# - scripts/check-engine.sh：引擎 crate（要 crengine-ng 的源码）。
# - scripts/check-aristotle-crengine.sh：Aristotle 的 crengine 排版路径
#   （要 crengine-ng 的源码和 Plato 的 C 库）。
aristotle_crates=(aristotle-screen aristotle-i18n aristotle-library aristotle-net aristotle-ui)
aristotle_pkg_args=()
for crate in "${aristotle_crates[@]}"; do
  aristotle_pkg_args+=(-p "$crate")
done

run_aristotle_step() {
  local label="$1"
  shift
  local status=0

  printf '\n==> %s\n' "$label"
  (
    cd -- "$repo_root/aristotle" || exit 1
    # shellcheck source=/dev/null
    . tools/env.sh
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ARISTOTLE_CACHE/target}"
    "$@"
  ) || status=$?
  if [ "$status" -ne 0 ]; then
    printf '\n[FAIL] %s 失败（退出码 %d）\n' "$label" "$status" >&2
    exit "$status"
  fi
  printf '[OK] %s\n' "$label"
}

run_aristotle_step "aristotle: cargo fmt --check (${aristotle_crates[*]})" \
  cargo fmt "${aristotle_pkg_args[@]}" -- --check
run_aristotle_step "aristotle: cargo clippy -D warnings (${aristotle_crates[*]})" \
  cargo clippy --config .cargo/config.toml "${aristotle_pkg_args[@]}" --all-targets -- -D warnings
run_aristotle_step "aristotle: cargo test (${aristotle_crates[*]})" \
  cargo test --config .cargo/config.toml "${aristotle_pkg_args[@]}"

printf '\n[OK] 全部检查通过\n'
