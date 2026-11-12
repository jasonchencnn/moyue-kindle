#!/usr/bin/env bash
# QA-101：模拟环境回归。
#
# 用 `aristotle-ui-preview` 按 1264×1680、300 dpi 出中/英/波、日/夜的全部屏幕图，
# 再跑五项自动检查（布局越界与文字溢出、可点击区、死按钮、三语言文案键、
# 「即将推出」入口），并把有原型参考图的页面并排成 compare 图。
#
# 用法：
#   scripts/qa-101.sh            跑全部检查，打印结果，退出码 0（供报告用）
#   scripts/qa-101.sh --strict   有缺陷时退出 1（CI/验收用）
#   QA101_OUT=DIR scripts/qa-101.sh
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"

strict=0
for arg in "$@"; do
  case "$arg" in
    --strict) strict=1 ;;
    -h|--help)
      sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "[FAIL] 未知参数：$arg" >&2
      exit 2
      ;;
  esac
done

if ! command -v python3 >/dev/null 2>&1; then
  echo "[FAIL] 未找到 python3，检查脚本用它比对文案与布局结果" >&2
  exit 127
fi

# 任务包指定的构建目录与渲染数据；已在环境里设过就沿用。
: "${CARGO_TARGET_DIR:=${HOME}/.cache/aristotle/target-QA-101}"
: "${ARISTOTLE_RENDER_DATA:=${HOME}/.cache/aristotle/render/device/data}"
: "${QA101_OUT:=${HOME}/.cache/aristotle/qa-101}"
export CARGO_TARGET_DIR ARISTOTLE_RENDER_DATA
export ARISTOTLE_CACHE="${ARISTOTLE_CACHE:-${HOME}/.cache/aristotle}"

if [ ! -d "$ARISTOTLE_RENDER_DATA" ]; then
  echo "[FAIL] 没有渲染数据目录 $ARISTOTLE_RENDER_DATA" >&2
  echo "       先跑 aristotle/tools/build-render.sh（见 docs/reader.md），或用" >&2
  echo "       ARISTOTLE_RENDER_DATA=... 指向已有的 device/data。" >&2
  exit 1
fi

size=1264x1680
dpi=300
langs=(zh en pl)
screens="$QA101_OUT/screens"
mkdir -p "$screens"

run_step() {
  local label="$1"
  shift
  printf '\n==> %s\n' "$label"
  "$@"
}

defects=0
note_defect() {
  defects=$((defects + 1))
  printf '  [缺陷] %s\n' "$1"
}

# --- 1. 构建预览工具 -------------------------------------------------------
run_step "构建 aristotle-ui-preview" \
  sh -c 'cd aristotle && cargo build --release --locked -p aristotle-ui --bin aristotle-ui-preview'
preview="$CARGO_TARGET_DIR/release/aristotle-ui-preview"
if [ ! -x "$preview" ]; then
  echo "[FAIL] 没有生成 $preview" >&2
  exit 1
fi

# --- 2. 出图：中/英/波 × 日/夜 ---------------------------------------------
# 每页 day 版和 night 版一起出；语言用 --lang 分开跑，失败时看得出是哪种语言。
for lang in "${langs[@]}"; do
  run_step "出图 ${lang}（${size} @ ${dpi}dpi）" \
    "$preview" --size "$size" --dpi "$dpi" --lang "$lang" "$screens/$lang"
done

# --- 3. 布局：越界、文字溢出、塌陷（文字/控件没有画出来的空间） ------------
printf '\n==> 检查布局（越界 / 溢出 / 塌陷）\n'
layout_bad=0
for lang in "${langs[@]}"; do
  findings="$screens/$lang/qa-findings.txt"
  if [ ! -f "$findings" ]; then
    note_defect "${lang}：预览没有写出 qa-findings.txt"
    layout_bad=$((layout_bad + 1))
    continue
  fi
  bounds=$(grep -c $'\tbounds\t' "$findings" || true)
  overflow=$(grep -c $'\toverflow\t' "$findings" || true)
  collapsed=$(grep -c $'\tcollapsed\t' "$findings" || true)
  printf '  %s：越界 %s，文字溢出 %s，塌陷 %s\n' "$lang" "$bounds" "$overflow" "$collapsed"
  if [ "$bounds" -gt 0 ] || [ "$overflow" -gt 0 ] || [ "$collapsed" -gt 0 ]; then
    layout_bad=$((layout_bad + 1))
    { grep -E $'\t(bounds|overflow|collapsed)\t' "$findings" || true; } | sed 's/^/    /'
  fi
done
if [ "$layout_bad" -gt 0 ]; then
  note_defect "布局检查有发现，见 $screens/<lang>/qa-findings.txt"
fi

# --- 4. 可点击区 < 64 px ----------------------------------------------------
printf '\n==> 检查可点击区（最小 64 px）\n'
target_bad=0
for lang in "${langs[@]}"; do
  findings="$screens/$lang/qa-findings.txt"
  small=$(grep -c $'\ttarget\t' "$findings" 2>/dev/null || true)
  printf '  %s：%s 处\n' "$lang" "$small"
  if [ "$small" -gt 0 ]; then
    target_bad=$((target_bad + 1))
    { grep $'\ttarget\t' "$findings" || true; } | sed 's/^/    /'
  fi
done
if [ "$target_bad" -gt 0 ]; then
  note_defect "有可点击区小于 64 px（命中区会按 9 mm 放大，但可见目标偏小）"
fi

# --- 5. 文案键：三语言键集合、类型、占位符一致 -----------------------------
printf '\n==> 检查三语言文案键\n'
if ! python3 - <<'PY'
import json, pathlib, sys

root = pathlib.Path("aristotle/crates/aristotle-i18n/locales")
locales = ["en", "pl", "zh"]
plural_keys = {"zero", "one", "two", "few", "many", "other"}

def flatten(obj, prefix=""):
    out = {}
    for key, value in obj.items():
        if key == "@locale":
            continue
        name = f"{prefix}.{key}" if prefix else key
        if isinstance(value, dict) and set(value) <= plural_keys:
            out[name] = value  # a plural message
        elif isinstance(value, dict):
            out.update(flatten(value, name))
        else:
            out[name] = value
    return out

def kind(value):
    return "plural" if isinstance(value, dict) else "text"

def placeholders(value):
    forms = value.values() if isinstance(value, dict) else [value]
    found = set()
    for text in forms:
        rest = text
        while "{" in rest:
            rest = rest[rest.index("{") + 1:]
            if "}" not in rest:
                break
            name, rest = rest.split("}", 1)
            found.add(name)
    return found

messages = {code: flatten(json.loads((root / f"{code}.json").read_text())) for code in locales}
base = messages["en"]
problems = []
for code in locales[1:]:
    for key in sorted(set(messages[code]) - set(base)):
        problems.append(f"{code}.json: 多出键 {key}")
    for key in sorted(set(base) - set(messages[code])):
        problems.append(f"{code}.json: 缺少键 {key}")
    for key, value in sorted(messages[code].items()):
        if key not in base:
            continue
        if kind(value) != kind(base[key]):
            problems.append(f"{code}.json: {key} 的类型和 en 不一致")
        elif placeholders(value) != placeholders(base[key]):
            problems.append(
                f"{code}.json: {key} 的占位符 {sorted(placeholders(value))} != en 的 {sorted(placeholders(base[key]))}"
            )
if problems:
    print(f"  {len(problems)} 个问题：")
    for problem in problems:
        print("   ", problem)
    sys.exit(1)
print(f"  en / pl / zh 各有 {len(base)} 条文案，键、类型、占位符一致")
PY
then
  note_defect "三语言文案键不一致"
fi

# --- 6. 死按钮：每个 ActionId 都有人接 -------------------------------------
printf '\n==> 检查死按钮（每个 action 都有处理分支）\n'
if ! python3 - <<'PY'
import pathlib, re, sys

ui = pathlib.Path("aristotle/crates/aristotle-ui/src")
app = pathlib.Path("aristotle/crates/aristotle/src")
texts = [(p, p.read_text()) for p in ui.rglob("*.rs")] + [(p, p.read_text()) for p in app.rglob("*.rs")]

actions = []
for path in (ui / "screens").glob("*.rs"):
    for match in re.finditer(r"pub const (\w+): ActionId = (\d+);", path.read_text()):
        actions.append((path.stem, match.group(1), match.group(2)))

dead = []
for module, name, value in actions:
    declaration = re.compile(r"^\s*pub const " + re.escape(name) + r"\s*:")
    referred = False
    for _, text in texts:
        for line in text.splitlines():
            if re.search(r"\b" + re.escape(name) + r"\b", line) and not declaration.match(line):
                referred = True
                break
        if referred:
            break
    if not referred:
        dead.append(f"{module}::{name} = {value}")
if dead:
    print(f"  {len(dead)} 个 action 没有任何处理分支：")
    for item in dead:
        print("   ", item)
    sys.exit(1)
print(f"  {len(actions)} 个 action 都有引用（直连或在 UI 内部处理）")
PY
then
  note_defect "有 action 没有被处理（死按钮）"
fi

# --- 7. 「即将推出」入口：只允许账号、云端、书内搜索 ------------------------
printf '\n==> 检查「即将推出」入口\n'
if ! python3 - <<'PY'
import pathlib, sys

source = pathlib.Path("aristotle/crates/aristotle/src/main.rs").read_text().splitlines()
allowed = ("a::ACCOUNT", "a::CLOUD_LIBRARY", "menu_action::SEARCH")
sites = []
for index, line in enumerate(source):
    if "show_coming_soon(" not in line or "fn show_coming_soon" in line:
        continue
    window = "\n".join(source[max(0, index - 15):index])
    # 刷新「即将推出」页本身也调 show_coming_soon，那不是入口。
    if "Screen::ComingSoon" in window:
        continue
    reason = next((token for token in allowed if token in window), None)
    sites.append((index + 1, reason))
print(f"  找到 {len(sites)} 个入口：" + ", ".join(str(line) for line, _ in sites))
for line, reason in sites:
    print(f"   main.rs:{line} -> {reason or '不在允许清单里'}")
sys.exit(1 if any(reason is None for _, reason in sites) else 0)
PY
then
  note_defect "有账号/云端/书内搜索以外的「即将推出」入口"
fi

# --- 8. 产物完整性：每页都有 night 版，三语言屏幕相同，尺寸正确 -------------
printf '\n==> 检查出图完整性\n'
if ! python3 - "$screens" "$size" <<'PY'
import pathlib, struct, sys

screens = pathlib.Path(sys.argv[1])
want_w, want_h = (int(v) for v in sys.argv[2].split("x"))
core = {"home", "library", "library-grid", "reader", "reader-menu", "settings",
        "mine", "quotelibrary", "scan-empty", "scan-scanning", "scan-done",
        "screensaver", "wifitransfer", "readertypeset"}
problems = []
sets = {}
for lang in ("zh", "en", "pl"):
    directory = screens / lang
    manifest = directory / "qa-screens.txt"
    if not manifest.is_file():
        problems.append(f"{lang}: 没有 qa-screens.txt")
        sets[lang] = set()
        continue
    names = {line.strip() for line in manifest.read_text().splitlines() if line.strip()}
    sets[lang] = names
    if len(names) < 40:
        problems.append(f"{lang}: 只出了 {len(names)} 页")
    for name in sorted(core - names):
        problems.append(f"{lang}: 缺少核心页 {name}")
    for name in sorted(names):
        day = directory / f"{name}.png"
        night = directory / f"{name}-night.png"
        if not day.is_file():
            problems.append(f"{lang}: 没有 {day.name}")
            continue
        if not night.is_file():
            problems.append(f"{lang}: {day.name} 没有 night 版")
            continue
        width, height = struct.unpack(">II", day.read_bytes()[16:24])
        if (width, height) != (want_w, want_h):
            problems.append(f"{lang}/{day.name} 是 {width}x{height}，不是 {want_w}x{want_h}")
for other in ("en", "pl"):
    for name in sorted(sets["zh"] ^ sets[other]):
        problems.append(f"zh/{other} 屏幕集合不同：{name}")
if problems:
    print(f"  {len(problems)} 个问题：")
    for problem in problems[:30]:
        print("   ", problem)
    if len(problems) > 30:
        print(f"    ……另有 {len(problems) - 30} 条")
    sys.exit(1)
print(f"  三语言各 {len(sets['zh'])} 页，都有 day/night 版，尺寸 {want_w}x{want_h}")
PY
then
  note_defect "出图不完整"
fi

# --- 9. compare 图：每张参考图都有并排对比 ---------------------------------
printf '\n==> 检查与原型参考图的 compare 图\n'
if ! python3 - "$screens" <<'PY'
import pathlib, struct, sys

root = pathlib.Path(sys.argv[1])
reference = pathlib.Path("docs/ui/prototype-v2/reference")
compares = {p.name: p for p in root.glob("compare-*.png")}
# 网格页的对比图沿用既有文件名 compare-grid.png（POLISH-001 / LIB-005 记过）。
alias = {"librarygrid": "grid"}
missing = [
    ref.name
    for ref in sorted(reference.glob("*.png"))
    if f"compare-{alias.get(ref.stem, ref.stem)}.png" not in compares
]
if missing:
    print("  没有并排图的参考页：" + ", ".join(missing))
    sys.exit(1)
for name, path in sorted(compares.items()):
    width, height = struct.unpack(">II", path.read_bytes()[16:24])
    if height != 1680 or width < 1264 * 2:
        print(f"  {name} 尺寸不对：{width}x{height}")
        sys.exit(1)
print(f"  {len(compares)} 张 compare 图，覆盖全部参考页")
PY
then
  note_defect "缺少与原型参考图的并排图"
fi

# --- 汇总 ------------------------------------------------------------------
printf '\n==> 汇总\n'
printf '  产物：%s\n' "$QA101_OUT"
printf '  - 屏幕图：%s/<lang>/\n' "$screens"
printf '  - 布局发现：%s/<lang>/qa-findings.txt\n' "$screens"
printf '  - 并排对比：%s/compare-*.png\n' "$screens"
if [ "$defects" -gt 0 ]; then
  printf '  %d 项自动检查有发现（见上）；逐页结论与 P0/P1/P2 见 docs/tasks/QA-101/report.md。\n' "$defects"
  if [ "$strict" -eq 1 ]; then
    exit 1
  fi
else
  printf '  自动检查全部通过。\n'
fi
