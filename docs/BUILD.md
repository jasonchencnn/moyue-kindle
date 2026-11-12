# 从源码构建

墨阅 Kindle 由两部分组成：仓库根的 Rust workspace（`core` / `engine` / `platform` 等）和 `aristotle/` 下的 Kindle 客户端 workspace。设备包是静态 ARM 可执行文件 + KUAL 扩展。

> 本文只写当前仓库实际使用的构建路径。Aristotle 上游的通用说明见 `aristotle/.github/CONTRIBUTING.md` 和 `aristotle/docs/`。

## 环境要求

- macOS 或 Linux（开发机为 macOS，另在 aarch64 Ubuntu 上做过交叉验证）
- Rust（rustup），目标：`armv7-unknown-linux-musleabihf`、`x86_64-unknown-linux-musl`
- `curl`、`tar`、`xz`、`openssl`、`xxd`、`make`、`cmake`、`bash`、主机 C 编译器
- Python 3（部分构建/打包脚本与测试）
- 首次构建要联网：拉取 Zig 工具链、crengine 源码、Plato 的 C 库

所有大文件都放在 `ARISTOTLE_CACHE`（默认 `~/.cache/aristotle`），包括工具链、C 库、构建目录、截图与测试产物。建议先 source `aristotle/tools/env.sh`：

```sh
git clone --recursive https://github.com/jasonchencnn/moyue-kindle.git
cd moyue-kindle
. aristotle/tools/env.sh
export CARGO_TARGET_DIR="$ARISTOTLE_CACHE/target"
```

## 1. 纯 Rust 部分（不需要 C++ 工具链）

```sh
./scripts/check.sh
```

等价于 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。这一步覆盖 `core`、`platform` 与不开 crengine feature 的 `engine`，可以在任何机器上跑。

## 2. crengine 排版引擎

```sh
engine/fetch.sh          # 拉取 koreader/crengine 固定提交 + 补丁 + xxHash
./scripts/check-engine.sh
```

crengine 需要 CMake、主机 C/C++ 工具链和其依赖库（FreeType、HarfBuzz、FriBiDi、libunibreak、utf8proc、zstd 等）。macOS 上可用 Homebrew 安装，脚本会给出缺失项。

端到端检查（主程序带 crengine 编过，含 ARM 目标）：

```sh
./scripts/check-aristotle-crengine.sh
```

这一步需要 Plato 的 C 库与 Zig 交叉工具链，见下一步。

## 3. Kindle 设备包（Oasis 2）

```sh
cd aristotle
. tools/env.sh
export CARGO_TARGET_DIR="$ARISTOTLE_CACHE/target"

tools/fetch-tools.sh          # 固定版本的 Zig 等主机侧工具
tools/build-plato.sh          # Plato 的静态 C 库（设备目标）
tools/build-render.sh device  # 构建渲染器与阅读器，装配设备目录
tools/package.sh              # 生成 zip：aristotle/ + extensions/aristotle/
```

产物默认在 `$ARISTOTLE_CACHE/render/device` 与 `$ARISTOTLE_CACHE/dist/`。装到设备的方法见 [INSTALL.md](INSTALL.md)。

> 首次构建约需 10–20 分钟；构建缓存未识别源码变更时，先清理对应的 `CARGO_TARGET_DIR` 或 `render/device` 目录再重试。

## 4. 主机预览（不出设备）

```sh
cd aristotle
. tools/env.sh
cargo run --release -p aristotle-ui --bin aristotle-ui-preview -- /tmp/shots --lang all
```

会为每个屏幕输出 PNG，用于检查布局与文案；不涉及真机、不使用设备波形。

## 5. 检查与截图

| 命令 | 用途 |
| --- | --- |
| `./scripts/check.sh` | 纯 Rust：fmt / clippy / 测试 |
| `./scripts/check-engine.sh` | 引擎绑定与黄金图 |
| `./scripts/check-aristotle-crengine.sh` | 主程序 + crengine 全链路、ARM 类型检查 |
| `./scripts/qa-101.sh` | 三语（简中/英/波兰）日间+夜间全屏出图回归 |

真机截图（需要设备开启 SSH）：

```sh
KINDLE=root@<device-ip> ./scripts/take-screenshot.sh home
```

## 常见问题

- **`engine/vendor` 不存在**：先跑 `engine/fetch.sh`，第三方源码不进仓库。
- **编译很慢 / 内存不足**：构建目录放在 `ARISTOTLE_CACHE`，不要用 `/tmp`（很多机器上是 RAM disk）。
- **`plato` 子目录为空**：`git submodule update --init --recursive`。
- **链接错误**：确认 Zig 工具链与 Plato C 库是同一个目标（device 或 host），不要混用缓存。
