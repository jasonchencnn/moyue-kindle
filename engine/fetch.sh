#!/usr/bin/env bash
# 拉取 koreader/crengine 源码及辅助依赖（xxHash）并应用补丁。
#
# KOReader 最新稳定版（v2026.07.1）使用的 koreader/crengine 提交：
#   出处：koreader/koreader v2026.07.1 -> koreader-base (6e4bc81af4e04f78d81677a323a780a71b29702a)
#         -> thirdparty/kpvcrlib/crengine @ b32a88ffbbf505cf2ea5208c1a49002d70d3609f
#   仓库：https://github.com/koreader/crengine.git
#   SHA： b32a88ffbbf505cf2ea5208c1a49002d70d3609f
#
# xxHash 依赖（Cyan4973/xxHash v0.8.3）：
#   出处：koreader-base thirdparty/xxhash (v0.8.3)
#
# 依据：ADR-015、docs/tasks/ENG-010.md。
set -euo pipefail

REPO_URL="https://github.com/koreader/crengine.git"
COMMIT_SHA="b32a88ffbbf505cf2ea5208c1a49002d70d3609f"

XXHASH_H_URL="https://raw.githubusercontent.com/Cyan4973/xxHash/v0.8.3/xxhash.h"
XXHASH_H_SHA256="17973c0dc49d9854ca26caa191f0e12f7a424b68858d9a78de3860d959d85e4b"

XXHASH_C_URL="https://raw.githubusercontent.com/Cyan4973/xxHash/v0.8.3/xxhash.c"
XXHASH_C_SHA256="5c3591fe6e6c86a619eb26760e9520e37a6fd5152882ab5ad93f912e2a855966"

XXHASH_LIC_URL="https://raw.githubusercontent.com/Cyan4973/xxHash/v0.8.3/LICENSE"
XXHASH_LIC_SHA256="6ffedbc0f7878612d2b23589f1ff2ab15633e1df7963a5d9fc750ec5500c7e7a"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
vendor_dir="$script_dir/vendor"
src_dir="$vendor_dir/koreader-crengine"
patches_dir="$script_dir/patches"

sha256_check() {
    local expected="$1"
    local file="$2"
    if command -v sha256sum >/dev/null 2>&1; then
        echo "$expected  $file" | sha256sum -c - >/dev/null 2>&1
    elif command -v shasum >/dev/null 2>&1; then
        echo "$expected  $file" | shasum -a 256 -c - >/dev/null 2>&1
    else
        echo "未找到 sha256 校验工具（sha256sum / shasum）" >&2
        return 1
    fi
}

mkdir -p "$vendor_dir"

if [ -d "$src_dir/.git" ]; then
    current_head="$(git -C "$src_dir" rev-parse HEAD 2>/dev/null || true)"
    if [ "$current_head" = "$COMMIT_SHA" ]; then
        echo "[SKIP] $src_dir 已处于目标提交 ($COMMIT_SHA)"
    else
        echo "[FETCH] 更新 $src_dir 到 $COMMIT_SHA..."
        git -C "$src_dir" fetch --depth 1 origin "$COMMIT_SHA"
        git -C "$src_dir" checkout "$COMMIT_SHA"
    fi
else
    echo "[CLONE] $REPO_URL @ $COMMIT_SHA -> $src_dir"
    mkdir -p "$src_dir"
    git -C "$src_dir" init
    git -C "$src_dir" remote add origin "$REPO_URL"
    git -C "$src_dir" fetch --depth 1 origin "$COMMIT_SHA"
    git -C "$src_dir" checkout "$COMMIT_SHA"
fi

actual_commit="$(git -C "$src_dir" rev-parse HEAD)"
if [ "$actual_commit" != "$COMMIT_SHA" ]; then
    echo "[ERROR] commit SHA 校验失败：期望 $COMMIT_SHA，实际 $actual_commit" >&2
    exit 1
fi
echo "[OK] koreader/crengine 就绪：$actual_commit"

# 拉取 xxHash
xxhash_dir="$src_dir/thirdparty/xxhash"
mkdir -p "$xxhash_dir"

if [ ! -f "$xxhash_dir/xxhash.h" ] || ! sha256_check "$XXHASH_H_SHA256" "$xxhash_dir/xxhash.h"; then
    echo "[FETCH] xxhash.h..."
    curl -fL --retry 3 -o "$xxhash_dir/xxhash.h" "$XXHASH_H_URL"
    sha256_check "$XXHASH_H_SHA256" "$xxhash_dir/xxhash.h" || {
        echo "[ERROR] xxhash.h checksum mismatch" >&2
        exit 1
    }
fi

if [ ! -f "$xxhash_dir/xxhash.c" ] || ! sha256_check "$XXHASH_C_SHA256" "$xxhash_dir/xxhash.c"; then
    echo "[FETCH] xxhash.c..."
    curl -fL --retry 3 -o "$xxhash_dir/xxhash.c" "$XXHASH_C_URL"
    sha256_check "$XXHASH_C_SHA256" "$xxhash_dir/xxhash.c" || {
        echo "[ERROR] xxhash.c checksum mismatch" >&2
        exit 1
    }
fi

if [ ! -f "$xxhash_dir/LICENSE" ] || ! sha256_check "$XXHASH_LIC_SHA256" "$xxhash_dir/LICENSE"; then
    echo "[FETCH] xxHash LICENSE..."
    curl -fL --retry 3 -o "$xxhash_dir/LICENSE" "$XXHASH_LIC_URL"
    sha256_check "$XXHASH_LIC_SHA256" "$xxhash_dir/LICENSE" || {
        echo "[ERROR] xxHash LICENSE checksum mismatch" >&2
        exit 1
    }
fi
echo "[OK] xxHash 就绪"

# 打补丁
if [ -d "$patches_dir" ]; then
    for patch in "$patches_dir"/*.patch; do
        [ -f "$patch" ] || continue
        patch_name="$(basename "$patch")"
        if git -C "$src_dir" apply --check "$patch" >/dev/null 2>&1; then
            echo "[PATCH] 应用补丁: $patch_name"
            git -C "$src_dir" apply "$patch"
        elif git -C "$src_dir" apply --reverse --check "$patch" >/dev/null 2>&1; then
            echo "[SKIP] 补丁已应用: $patch_name"
        else
            echo "[ERROR] 无法应用补丁: $patch_name" >&2
            exit 1
        fi
    done
fi
echo "[OK] 全部补丁就绪"
