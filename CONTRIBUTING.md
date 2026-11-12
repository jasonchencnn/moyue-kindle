# 贡献指南

感谢你愿意参与墨阅 Kindle。提交前请读一遍本文。

## 先讨论，再动手

- **小修**（错别字、文档、一行的 bug 修复）：直接开 PR。
- **功能或架构改动**：先开 issue 说明动机、方案和影响范围，维护者确认后再写代码，避免白做。
- 不确定某件事是否在范围内，先开 issue 问。

## 不接受的内容

本项目是阅读器，不是内容平台：

- 不提供、不推荐、不暗示任何盗版书源或公共书籍下载；
- 不包含 DRM 去除、固件破解、越狱工具或绕过亚马逊服务限制的代码；
- 不加入与阅读无关的账号、支付、广告或遥测。

## 开发环境与检查

```sh
git clone --recursive https://github.com/jasonchencnn/moyue-kindle.git
cd moyue-kindle
./scripts/check.sh                     # fmt + clippy + 单元测试
./scripts/check-engine.sh              # 需要先跑 engine/fetch.sh，且装了 C++ 工具链
./scripts/check-aristotle-crengine.sh  # 端到端：主程序带 crengine，需要 Plato C 库
```

设备包构建见 [BUILD.md](docs/BUILD.md)。提交前至少通过 `scripts/check.sh`；改动涉及排版、HAL、UI 的任务还要跑对应的第二、第三套检查。

## 代码约定

- Rust 2021，`cargo fmt` 与 `cargo clippy -D warnings` 必须干净。
- `unsafe` 只允许出现在 FFI 边界，每一处写 `// SAFETY:` 说明成立的前提。
- 公共 API 写 `///` 文档注释；不提交注释掉的代码、调试打印和无任务号的 TODO。
- 提交信息用 Conventional Commits：`feat(core): ...`、`fix(ui): ...`，subject 用英文，正文中英皆可。
- 新增依赖必须与 AGPL-3.0-or-later 兼容（MIT / Apache-2.0 / BSD / GPLv2+ / GPLv3 / AGPL 可以；GPLv2-only 和专有许可证不行）。
- 改动 `aristotle/` 子树时，沿用上游 Aristotle 的风格与约定，并遵守其 `aristotle/.github/CONTRIBUTING.md`。

## UI 与真机

- 界面字号不低于 32 px，点击区不小于 64 px（墨水屏）。
- 改动屏幕需要生成中英文截图，说明每屏是否有溢出、截断或错位。
- 真机测试由维护者在 Kindle 上进行；PR 里请写清"真机需要验证什么"。

## 同步协议

改动同步数据格式前先读 [docs/SYNC.md](docs/SYNC.md)。格式必须保持与 Readest / KOReader 兼容；不兼容的改动需要先开 issue 讨论。

## 许可证

提交即表示你同意代码以 [AGPL-3.0-or-later](LICENSE) 发布。来自其他项目的代码必须兼容 AGPL 并保留原始版权声明。
