#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Multi-screen UI layout regression matrix for 6-inch, 7-inch and 10.2-inch
# e-ink panels. Runs aristotle-ui-preview across representative resolutions and
# DPIs in zh and en, and fails when any screen reports bounds/overflow/
# collapsed/overlap/target findings.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"

# Default CJK fallback data dir if present in workspace or cache.
if [ -z "${ARISTOTLE_RENDER_DATA:-}" ]; then
  if [ -f "$repo_root/target/ui-verify-out/data/fonts/DejaVuSans-Symbols.ttf" ]; then
    export ARISTOTLE_RENDER_DATA="$repo_root/target/ui-verify-out/data"
  elif [ -f "${ARISTOTLE_CACHE:-$HOME/.cache/aristotle}/data/fonts/DejaVuSans-Symbols.ttf" ]; then
    export ARISTOTLE_RENDER_DATA="${ARISTOTLE_CACHE:-$HOME/.cache/aristotle}/data"
  fi
fi

OUT_BASE="${1:-${TMPDIR:-/tmp}/moyue-ui-matrix}"
LANGS="${MOYUE_UI_MATRIX_LANGS:-zh en}"
PROFILES=(
  "basic3:600x800:167"
  "pw2:758x1024:212"
  "pw4:1072x1448:300"
  "pw5:1236x1648:300"
  "ko2:1264x1680:300"
  "pw6:1272x1696:300"
  "scribe:1860x2480:300"
)

cargo build --manifest-path "$repo_root/aristotle/Cargo.toml" \
  -p aristotle-ui --bin aristotle-ui-preview --release >/dev/null

BIN="$repo_root/aristotle/target/release/aristotle-ui-preview"
mkdir -p "$OUT_BASE"

total_findings=0
for entry in "${PROFILES[@]}"; do
  name="${entry%%:*}"
  rest="${entry#*:}"
  sz="${rest%%:*}"
  dpi="${rest##*:}"
  for lang in $LANGS; do
    dir="$OUT_BASE/${name}-${lang}"
    rm -rf "$dir"
    mkdir -p "$dir"
    "$BIN" --size "$sz" --dpi "$dpi" --lang "$lang" "$dir" >"$dir/run.log" 2>&1
    cnt=$(wc -l <"$dir/qa-findings.txt" | tr -d ' ')
    total_findings=$((total_findings + cnt))
    printf "%-10s %-10s %3sdpi %-2s : %4d findings\n" "$name" "$sz" "$dpi" "$lang" "$cnt"
    if [ "$cnt" -gt 0 ]; then
      awk -F'\t' '{c[$1" ("$2")"]++} END {for (k in c) printf "    %3d  %s\n", c[k], k}' \
        "$dir/qa-findings.txt" | sort -rn | head -n 12
    fi
  done
done

echo "Total findings across matrix: $total_findings (artifacts: $OUT_BASE)"
if [ "$total_findings" -ne 0 ]; then
  exit 1
fi
