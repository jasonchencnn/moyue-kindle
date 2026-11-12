# 架构

墨阅 Kindle 是**越狱 Kindle 上的原生阅读器**：单个静态 ARM 可执行文件，直接使用 Kindle 的 Linux 内核、帧缓冲和输入设备；通过 KUAL 启动，运行期间接管原厂界面，退出后恢复。

## 分层

```
┌─────────────────────────────────────────────────────────────┐
│ KUAL 扩展 (kual/)                                            │
│   探测机型/固件 → 启动守卫 → 启动/退出、看门狗                  │
├─────────────────────────────────────────────────────────────┤
│ aristotle/  Kindle 客户端本体（Aristotle 子树）                │
│   ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐  │
│   │ HAL          │  │ UI           │  │ 网络 / 同步        │  │
│   │ mxcfb 刷新    │  │ 首页/书库/    │  │ Readest API       │  │
│   │ 输入/旋转     │  │ 阅读/设置/统计 │  │ Wi-Fi 传书        │  │
│   └──────────────┘  └──────────────┘  └───────────────────┘  │
│   书库索引 · 书籍解析 · 字体 · i18n · 休眠/前光                 │
├─────────────────────────────────────────────────────────────┤
│ engine/  crengine FFI（EPUB/TXT 排版）                        │
│   koreader/crengine + shim.cpp + 3 个小补丁                  │
├─────────────────────────────────────────────────────────────┤
│ core/  平台无关：哈希、模型、合并规则、进度、统计、同步客户端     │
├─────────────────────────────────────────────────────────────┤
│ platform/  基础类型（灰度帧缓冲等）                            │
└─────────────────────────────────────────────────────────────┘
        ↓ 静态链接          ↓ 设备包
   armv7 musl 可执行文件   aristotle/ + extensions/aristotle/
```

| 目录 | 职责 |
| --- | --- |
| `core/` | 与平台无关的纯 Rust 逻辑：书籍 `partialMD5` 哈希（与 KOReader/Readest 一致）、书籍/进度/笔记数据模型、字段级 LWW 合并与墓碑、书库索引、阅读统计、`core::sync`。可在任意平台上用 `cargo test` 覆盖。 |
| `engine/` | `koreader/crengine` 的 Rust 绑定。`engine/fetch.sh` 拉取固定提交并打三个补丁（glyph cache 16 字节对齐修复 armv7 musl SIGBUS、JPEG 使用 IJG 色彩空间、回退字体可清空）。负责分页、渲染、XPointer、目录、词矩形、链接与脚注。 |
| `platform/` | 只保留跨层基础类型，当前主要是 `GrayBuffer`（灰度帧缓冲）与刷新矩形。 |
| `aristotle/` | Kindle 客户端本体（Aristotle 子树的修改版）：Zelda 平台 mxcfb 刷新、启动接管、触摸与旋转、UI 屏幕、书库、字体、i18n、网络策略、电源与休眠。 |
| `kual/` | KUAL 扩展：机型/固件守卫、启动与退出、防砖看门狗。 |
| `eink/`、`simulator/` | 早期路线的占位 crate，已停止开发，保留仅为 workspace 兼容，计划删除。 |

## 一次开书的数据流

1. 书库索引给出文件路径；`aristotle-library` 解析元数据与封面。
2. `engine` 让 crengine 加载 EPUB/TXT（先设视口与排版参数，再 `LoadDocument`，让缓存命中）。
3. 排版在 worker 线程执行；改字号时先局部排当前分片（局部重排），空闲 2 秒后补完剩余章节。
4. 渲染结果写入灰度帧缓冲，HAL 按区域和波形刷新（Oasis 2 全刷用 GC16，避免 GCK16 触发背光压暗）。
5. 进度以 XPointer 写回索引；阅读统计与笔记按需落盘。

## 同步

- **协议**：兼容 Readest `/api/sync`（configs + notes），账号体系与认证沿用 Readest，默认走官方认证中转 `auth-cf.readest.com`。
- **进度**：存 XPointer（crengine 原生定位），与 KOReader 同源；Readest 使用 CFI，两端通过 Readest 的映射逻辑互通。
- **合并**：标量字段与笔记按 `updatedAt` 做字段级 LWW，删除用墓碑；进度使用独立时间戳，避免整行覆盖。
- **离线优先**：网络失败不影响阅读，恢复后增量推送；队列与自动重试见 `core::sync`。
- 详细设计与限制见 [SYNC.md](SYNC.md)。

## 构建与发布

- 全静态链接：`armv7-unknown-linux-musleabihf`，用 Zig 做 C/C++ 交叉编译器（不依赖设备 glibc 版本）。
- 设备包 = `aristotle/`（程序、字体、引擎库、许可证、`SOURCE.txt`）+ `extensions/aristotle/`（KUAL 启动器）。
- 许可证：AGPL-3.0-or-later；发布二进制时同时提供源码包与第三方许可证，清单见 [THIRD_PARTY.md](../THIRD_PARTY.md)。

## 关键决策

见 [DECISIONS.md](DECISIONS.md)。其中影响最大的几条：

- 以 Aristotle 为底座、用 crengine 替换其引擎（ADR-011 / ADR-012）；
- 引擎从 crengine-ng 换成 KOReader 的 `koreader/crengine`，启用局部重排（ADR-015）；
- 设备产物使用全静态 musl，与固件浮点 ABI 无关（ADR-013）；
- 登录与同步兼容 Readest，默认使用官方认证中转（ADR-014 / ADR-016）。
