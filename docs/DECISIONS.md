# 架构决策记录（ADR）

只记录影响代码结构与兼容性的技术决策；团队协作与工具链决策未收录。格式：编号 · 日期 · 决策 · 原因 · 影响。只追加、不改旧条目。

| 编号 | 日期 | 决策 | 原因 | 影响 |
|---|---|---|---|---|
| ADR-001 | 2026-10-28 | Kindle 原生客户端为项目主线，不做 KOReader 插件保底 | Human 判断原生 Kindle 是核心亮点 | Kindle 首发约在第 15–18 周；资源 70% 投本仓库 |
| ADR-002 | 2026-10-28 | 本仓库与墨阅（Readest fork）保持两个独立仓库 | 工具链不同（Rust 交叉编译 vs pnpm/Tauri），互不干扰 | Teamspace 建两个团队；协议靠 `sync-contract.md` + 共享 fixture 对齐 |
| ADR-003 | 2026-10-28 | 排版引擎用 crengine-ng（EPUB/FB2/MOBI/TXT），PDF 用 MuPDF | crengine 原生 XPointer 可与 KOReader 直接互通，中文排版成熟；两者许可与 AGPL 兼容 | 需要 C++ 交叉编译（ENG-001 spike 验证） |
| ADR-004 | 2026-10-28 | UI 用 Slint 软件渲染到灰度缓冲，显示用 FBInk，输入直接读 evdev | Slint 同时支持桌面模拟器与帧缓冲；FBInk 屏蔽 Kindle 各代屏幕差异 | 模拟器与真机共用 `ui` crate |
| ADR-005 | 2026-10-28 | Kindle 登录用配对码（Kindle 显示码，手机/网页确认） | 墨水屏上输入邮箱密码体验差 | 墨阅仓库需新增设备配对 API（团队 B） |
| ADR-007 | 2026-10-28 | 墨阅云只通过兑换码开通容量；保留邀请注册；停用易支付 | 开源自用定位，不在国内运营 | 墨阅仓库团队 B 节点 B-002 |
| ADR-010 | 2026-10-30 | 排版尊重书内样式：crengine `PROP_EMBEDDED_STYLES` 开启，`engine/assets/default.css` 只作基础样式；`LayoutSettings.embedded_styles` 留作用户开关 | QA-002 实测书内 `<style>`/外链/行内 CSS 均生效，推翻 ENG-002「样式表被替换」的误判；与 KOReader 做法一致 | 内置样式需要强制的规则用 `!important`；设置页（K2）提供「忽略书籍样式」开关 |
| ADR-011 | 2026-11-02 | 改走路线 C：Kindle 客户端以 Aristotle（Rust，AGPL-3.0，Plato 引擎的 Kindle 移植）为底座，复用它的 HAL、takeover、UI、书库；EPUB 排版换成本仓库的 `engine` crate（crengine-ng，ADR-003 不变）；PDF/MOBI 转换/页眉页脚字体仍用 Plato。推翻 ADR-004（Slint + FBInk + evdev 自写），修订 ADR-001：原生客户端仍是主线，同时把 KOReader + Readest 官方 `readest.koplugin` 作为零开发保底，只写安装说明 | 路线调研 S-1～S-4（`docs/research/2026-11-02-路线重选调研.md`）：S-3 已在 Aristotle 内用 crengine 渲出中文页；S-2 真机上 Aristotle 只差 Zelda 刷新命令；S-4 确认 Readest 接口可复用；Readest 已有 KOReader 插件，自写保底没有意义 | `platform` 只保留 `GrayBuffer`（`engine` 依赖它），`eink`、`simulator` 停止开发，后续删除；PLAT-001/002/003 取消；UI-002 改为给 Aristotle UI 做中文与视觉定制 |
| ADR-012 | 2026-11-02 | Aristotle 用 `git subtree` 导入本仓库 `aristotle/` 目录（保留上游历史，可 `subtree pull` 跟进），Plato 子模块随之引入；本仓库 `core`、`engine` 以 path 依赖供其使用。给上游提 PR（如 Zelda 支持）须 Human 批准 | 引擎替换和 HAL 适配都要改 Aristotle 内部 crate，fork 是必然的；放在同一仓库可共用 `scripts/check.sh` 和 worktree 流程，不再新增第三个仓库（与 ADR-002 一致） | 新增 APP 前缀任务；Aristotle 的 fmt/clippy 规则与本仓库 AGENTS.md 冲突时，`aristotle/` 内沿用上游风格 |
| ADR-013 | 2026-11-02 | 设备产物沿用 Aristotle 的方式：`armv7-unknown-linux-musleabihf` 全静态链接，Zig 做 C/C++ 交叉编译器；crengine-ng 同样静态编进来。取代 H-001 notes 里「改用 `armv7-unknown-linux-gnueabi`」的结论 | S-2 实测：静态 musl 硬浮点二进制在 5.16.2 软浮点固件上正常运行，与设备 glibc（2.20）和浮点 ABI 无关；以后固件升到硬浮点也不用换目标 | ENG-001b 改为 musl 静态交叉编译；构建可在 macOS 上用 Zig 完成，Oracle ARM 服务器作备用 |
| ADR-014 | 2026-11-02 | 登录改用 Readest 已有的设备码接口 `/api/crosspoint/device/{code,approve,token}`（RFC 8628 风格），取代 ADR-005 设想的 `/api/device/pair/*`；Readest API 基址与 Supabase 基址在客户端都可配置；国内网络下 Supabase 认证经 Human 自有服务器的中转访问 | S-4：该接口已上线、字段清楚，Readest 网页已有 `/link` 批准页；实测家宽直连 `*.supabase.co` 在 TLS 握手阶段被重置，`web.readest.com` 正常 | `sync-contract.md` §6 按实际接口重写（SYNC-001）；Supabase 中转是对外部署，须 Human 批准后由 Leader 在 Oracle 上搭建（SYNC-002） |
| ADR-015 | 2026-11-04 | 排版引擎从 crengine-ng 0.9.15 换成 KOReader 的分支 koreader/crengine（固定为 KOReader v2026.07.1 采用的提交 `b32a88f`），修订 ADR-003；随之启用「局部重排」：改字号后先排当前章节，其余在后台补完。本仓库维护两处小补丁：glyph cache 16 字节对齐（armv7 musl 上 malloc 只有 8 字节对齐，否则 SIGBUS）、JPEG 改用标准 IJG 色彩空间（与 Plato/MuPDF 共用同一个 libjpeg） | S-5 真机实测（Oasis 2，《紅樓夢》）：改字号后当前页可读 6.34 s→1.10 s，后台 3.45 s 补完；首开 5.92→4.93 s；峰值内存 33.7→23.5 MiB；全书排版本身两者相当。XPointer 与 KOReader 同源，利于和 KOReader / Readest 的进度互通。Human 批准切换 | ENG-010 迁移引擎与构建，C2 已有的引擎能力（目录、选词、链接、缓存、进度、回退字体、多线程锁）全部回归；APP-007 在阅读器接入局部重排。局部重排补完前总页数、目录页码可能暂不准确。向 KOReader 上游提交补丁属于对外发布，须 Human 另行批准 |
| ADR-016 | 2026-11-11 | 修订 ADR-014 的认证中转方案，默认使用 Readest 官方 `https://auth-cf.readest.com` 处理密码登录与续期 | Kindle 直连原 Supabase 主机连接重置，官方中转续期与云端书单读取成功，Human 确认真机测试 OK 并要求纳入最新包 | 全新设置、旧配置缺省及恢复默认采用官方中转，显式自定义地址继续生效；契约见 §10.3，自建 Oracle 中转不再是当前登录链路的依赖 |
