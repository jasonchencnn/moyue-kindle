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
# LunaSVG 依赖（sammycage/lunasvg v2.3.9 + koreader-base xtended 文本/图片扩展，MIT 许可）：
#   出处：koreader-base @ 6e4bc81af4e04f78d81677a323a780a71b29702a thirdparty/lunasvg
#
# 依据：ADR-015、docs/tasks/ENG-010.md、feat/engine-fine-typeset。
set -euo pipefail

REPO_URL="https://github.com/koreader/crengine.git"
COMMIT_SHA="b32a88ffbbf505cf2ea5208c1a49002d70d3609f"

LUNASVG_REPO_URL="https://github.com/sammycage/lunasvg.git"
LUNASVG_TAG="v2.3.9"
LUNASVG_COMMIT_SHA="610b8bf5148a27489b4e3344b4f5617b81be38c7"
KO_BASE_LUNASVG_URL="https://raw.githubusercontent.com/koreader/koreader-base/6e4bc81af4e04f78d81677a323a780a71b29702a/thirdparty/lunasvg"

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

# 拉取 LunaSVG（SVG 1.1 + 内联 <svg> + <svg><text>/<image> 扩展）
lunasvg_dir="$vendor_dir/lunasvg"
if [ -d "$lunasvg_dir/.git" ]; then
    lunasvg_head="$(git -C "$lunasvg_dir" rev-parse HEAD 2>/dev/null || true)"
    if [ "$lunasvg_head" = "$LUNASVG_COMMIT_SHA" ]; then
        echo "[SKIP] $lunasvg_dir 已处于目标提交 ($LUNASVG_TAG)"
    else
        echo "[FETCH] 更新 $lunasvg_dir 到 $LUNASVG_TAG..."
        git -C "$lunasvg_dir" fetch --depth 1 origin tag "$LUNASVG_TAG"
        git -C "$lunasvg_dir" checkout --force FETCH_HEAD
    fi
else
    echo "[CLONE] $LUNASVG_REPO_URL @ $LUNASVG_TAG -> $lunasvg_dir"
    mkdir -p "$lunasvg_dir"
    git -C "$lunasvg_dir" init
    git -C "$lunasvg_dir" remote add origin "$LUNASVG_REPO_URL"
    git -C "$lunasvg_dir" fetch --depth 1 origin tag "$LUNASVG_TAG"
    git -C "$lunasvg_dir" checkout FETCH_HEAD
fi

lunasvg_xtended_dir="$lunasvg_dir/xtended"
lunasvg_patches_dir="$lunasvg_dir/.ko-patches"
mkdir -p "$lunasvg_xtended_dir" "$lunasvg_patches_dir"

fetch_verified() {
    local url="$1"
    local dst="$2"
    local expected_sha="$3"
    if [ ! -f "$dst" ] || ! sha256_check "$expected_sha" "$dst"; then
        echo "[FETCH] $(basename "$dst")..."
        curl -fL --retry 3 -o "$dst" "$url"
        sha256_check "$expected_sha" "$dst" || {
            echo "[ERROR] checksum mismatch: $dst" >&2
            exit 1
        }
    fi
}

fetch_verified "$KO_BASE_LUNASVG_URL/cmake_tweaks.patch" "$lunasvg_patches_dir/cmake_tweaks.patch" \
    "4a01d42cc0523960c57e2ede9988be137346ba7f671dcd1eb4e029a45d9ca172"
fetch_verified "$KO_BASE_LUNASVG_URL/extended.patch" "$lunasvg_patches_dir/extended.patch" \
    "287eb4226093ccc8d3ebf1cba72b6b350bbead5920e2f2095e8eed097ea38e38"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/CMakeLists.txt" "$lunasvg_xtended_dir/CMakeLists.txt" \
    "9e263b48ed412c24eef79276ee8cdd962f1debf2333aa275935b7122c25e4453"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/ximageelement.cpp" "$lunasvg_xtended_dir/ximageelement.cpp" \
    "1a5f6d40f31020b4a2e163576816bc08cc7b6e53e4e907b9b286d58235f3258c"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/ximageelement.h" "$lunasvg_xtended_dir/ximageelement.h" \
    "2f9aa736b24550f56a4bc311878b77fd741db2512be2e04db727486a72c007d6"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/xlunasvg.h" "$lunasvg_xtended_dir/xlunasvg.h" \
    "e8dfecaaac799f9fa194c2ab3183190b8c50ca3636c33c33b9bd6beae7c43c78"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/xtextelement.cpp" "$lunasvg_xtended_dir/xtextelement.cpp" \
    "acb34d9eabd598e4f93f5bf6c2bca321e843f66fd640aa9537f09af532446e0c"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/xtextelement.h" "$lunasvg_xtended_dir/xtextelement.h" \
    "6878c40396d334e4253f7c2781a3ddf750da3fa42e8c9ac0262b2b2e7bf0f27b"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/xtspanelement.cpp" "$lunasvg_xtended_dir/xtspanelement.cpp" \
    "c087276c6788a425f223511c87153bf1d5076dc8afc09aa2a3b30c36bef39967"
fetch_verified "$KO_BASE_LUNASVG_URL/xtended/xtspanelement.h" "$lunasvg_xtended_dir/xtspanelement.h" \
    "bae98e350f19b101343a696059e16c458980437281b383a1430895b7056ea56b"

for lpatch in cmake_tweaks.patch extended.patch; do
    pfile="$lunasvg_patches_dir/$lpatch"
    if git -C "$lunasvg_dir" apply --check "$pfile" >/dev/null 2>&1; then
        echo "[PATCH] 应用 LunaSVG 补丁: $lpatch"
        git -C "$lunasvg_dir" apply "$pfile"
    elif git -C "$lunasvg_dir" apply --reverse --check "$pfile" >/dev/null 2>&1; then
        echo "[SKIP] LunaSVG 补丁已应用: $lpatch"
    else
        echo "[ERROR] 无法应用 LunaSVG 补丁: $lpatch" >&2
        exit 1
    fi
done
echo "[OK] LunaSVG 就绪"
