# Changelog

All notable changes to Aristotle are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each release is a
tag `vX.Y.Z` whose version matches `Cargo.toml`, and its GitHub release notes
are its section below.

## [Unreleased]

## [0.3.8] - 2026-11-27

- 内置离线英汉词典与音标字体支持（`data/dicts`），查词与音标渲染更完整。
- 排版面板、阅读页上下栏及横竖排样式微调，修正部分页面布局与刷新问题。
- 更新独立书库入口图标与启动/OTA 脚本，版本号升至 0.3.8 并全量同步共享基线、适配中心目录与全机型 OTA 通道。

### Security

- Update `rustls-webpki` to 0.103.16 and `webpki-roots` to 1.0.10 to address RUSTSEC-2026-0382 and RUSTSEC-2026-0381.

### Fixed

- Stop idle polling outside the library, use worker notifications, and honor relative hold deadlines.
- Keep idle clocks from refreshing the panel and avoid forced flashes when waking Last screen.
- Save fewer reading checkpoints and make release timing logs opt-in with `ARISTOTLE_PERF=1`.
- Use a reversible `ondemand` governor during takeover and reduce the agent's USB maintenance work.
- Ignore touch during managed sleep and fail startup if managed sleep cannot be acquired.

### Changed

## [0.3.2] - 2026-11-18

- 排版面板在 600 x 800（Kindle 10）下按面板宽度分配控件，窄屏自动堆叠，不再重叠或裁切
- Kindle 10（2019）加入共享基线：实测几何 608 x 10752 / stride 608 / rotate 3，167 dpi
- PW2 使用 212 dpi；旧内核触摸时钟不可用时回退到单调时钟，恢复启动
- Version metadata, the extension version and the About screen use 0.3.2.

## [0.3.1] - 2026-11-14

The KO2 preview adds manual over-the-air updates and brings the support
diagnostics into the main KUAL extension.

### Added

- KUAL > 墨阅 > 检查更新（OTA）: fetch a versioned manifest over HTTPS from
  `me.chen-cn.top`, verify the device, firmware, version and SHA-256, and
  install a staged application and extension with rollback on failure. User
  books, progress, login, settings, screensavers and fonts are outside the
  replaced trees and are not touched.
- KUAL > 墨阅 > 诊断日志: the DIAG-001 collector and confirmation-protected
  upload, with credentials read from `/mnt/us/.aristotle/diagnostics/`.
- `tools/package.sh ota` and `tools/ota-manifest.py` for the release package,
  manifest and the corresponding-source publication fields.

### Changed

- Version metadata, the extension version and the About screen use 0.3.1.

### Security

- The OTA updater never executes package content, refuses archive traversal,
  symlinks and paths outside `aristotle/` and `extensions/aristotle/`, and
  writes only under `/mnt/us` and `/var/tmp`.

## [0.1.0] - 2026-10-17

The first release, for the Kindle Paperwhite 4 (2018) on firmware 5.13.x.
It is alpha software, tested on one device.

### Added

- A library and reader that run in place of the stock interface. While it
  runs, the stock interface, the store and the cloud and telemetry services
  are stopped; quitting or a reboot brings them back.
- The library lists `documents/` with *Books*, *Folders* and *Series* views,
  shelves and a *Continue reading* card. It reads the reading position the
  stock reader saved for each book.
- The reader opens EPUB, DRM-free MOBI and AZW3, PDF and TXT, using Plato's
  document engine. Pages turn with one full-screen update, as in the stock
  reader.
- Text is set in Literata with Knuth-Plass justification, with hyphenation
  for English, Polish, Ancient Greek and Latin.
- Your own reading fonts: TTF or OTF files in `fonts/` on the USB drive show
  up in the reader's menu.
- Hold a word to look it up in offline StarDict dictionaries, inflected forms
  included. Footnotes open in a popup.
- Highlights and bookmarks. Highlights are added to `My Clippings.txt` in the
  stock format and written as one Markdown note per book.
- *Settings* for the frontlight, night mode, covers, the screensaver delay, the
  USB mode and the language (English or Polish), and an *About* section with
  the version.
- *Start Aristotle* and *Return to the Kindle interface* in KUAL, and,
  with the agent, *Start Aristotle at boot*, which starts it instead of the
  stock interface and falls back to stock after three failed launches.
- Reading-position sync with KOReader through a KOSync server, and downloads
  from OPDS catalogs such as calibre's content server. Passwords stay in a
  root-only folder on the device.
- A KUAL extension with an optional agent that keeps a USB network and SSH
  up for development, switches the USB drive mode on request, and restarts
  the stock interface if it stays down. A file named `aristotle-disable` at
  the root of the USB drive keeps the agent from starting. While installed,
  the agent keeps Amazon's update daemons from running.
- An optional Wi-Fi policy that lets the device reach only the hosts you
  list.
- The scripts that change the device refuse to run on anything but a
  Paperwhite 4 on firmware 5.13.x.
- Release downloads: one zip to unpack onto the Kindle, a source tarball and
  SHA-256 sums.

[Unreleased]: https://github.com/piotrkowski/aristotle/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/piotrkowski/aristotle/releases/tag/v0.1.0
