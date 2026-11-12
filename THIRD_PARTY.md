# Third-party software

Copyright (C) 2026 Aristotle contributors.

Moyue Preview (墨阅预览版) is based on Aristotle and is licensed under the
GNU AGPL 3.0 or later ([LICENSE](LICENSE)). It includes or links the following
work by others. The release-level copies of the texts not kept next to their
code live in [`../licenses/`](../licenses/).

## In this repository

| Path | What | Licence |
| --- | --- | --- |
| `third_party/plato` | [Plato](https://github.com/baskerville/plato), a submodule pointing at our fork [piotrkowski/plato](https://github.com/piotrkowski/plato), branch `aristotle`. The MuPDF it pins is built with the patches in `tools/mupdf-patches/`. | AGPL-3.0 |
| `crates/aristotle-render/assets/fonts/`, `crates/aristotle-ui/assets/fonts/` | [Literata](https://github.com/googlefonts/literata) (The Literata Project Authors, designed by TypeTogether), [Charis](https://software.sil.org/charis/) and [Gentium Book](https://software.sil.org/gentium/) (SIL Global) | SIL OFL 1.1, text next to each font |
| `site/assets/fonts/InterVariable.woff2` | [Inter](https://github.com/rsms/inter) by the Inter Project Authors, used on the project site | SIL OFL 1.1, text in `site/assets/fonts/OFL.txt` |
| `tools/emulator/eink-emulator-pw4.patch` | Our patch to [eink-emulator](https://github.com/katadelos/eink-emulator), which has no licence. The patch keeps one line of upstream context per change. | Ours |
| `crates/aristotle-library`, `crates/aristotle-render` | [encoding_rs](https://github.com/hsivonen/encoding_rs) by Henri Sivonen, used for GBK and GB18030 text decoding | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| `crates/aristotle-library` | [pinyin](https://github.com/mozillazg/rust-pinyin) by mozillazg, used for Chinese pinyin sorting | MIT |
| `crates/aristotle-library/src/id.rs` | The partial-MD5 document hash follows KOReader's `util.partialMD5`, so both compute the same id. | KOReader: AGPL-3.0 |
| `crates/aristotle-ui/src/keyboard/pinyin_data.rs` | [rime-pinyin-simp](https://github.com/rime/rime-pinyin-simp) (converted via KOReader's `zh_pinyin_data.lua`), pinyin syllables and initial bitmasks | Apache-2.0 |
| `crates/aristotle-ui/src/keyboard/opencc_data.rs` | [OpenCC](https://github.com/BYVoid/OpenCC) `STCharacters.txt` and `TSCharacters.txt` character tables, used for Traditional to Simplified Chinese normalization | Apache-2.0 |
| `crates/aristotle-ui/src/screens/home/lunar.rs` | [tyme4rs](https://github.com/6tail/tyme4rs) by 6tail, used for the Chinese lunar calendar and the 24 solar terms on the home screen's clock card | MIT |
| `crates/aristotle-ui/assets/pet-cat.png`, `crates/aristotle-ui/assets/pet-fish.png`, `crates/aristotle-ui/assets/cat.mask`, `crates/aristotle-ui/assets/fish.mask` | The desk cat of the home screen and the fish it catches, drawn with Figma AI (gpt-image-2.5-flare). The originals and the candidates they were chosen from are in `docs/ui/prototype-v2/pet/`. | Ours |
| `crates/aristotle-ui/assets/slogan.png`, `crates/aristotle-ui/assets/slogan.mask` | The status bar slogan 「墨阅——用心阅读」, rendered from [Ma Shan Zheng](https://github.com/googlefonts/mashanzheng). Only the rendered picture ships; the font file does not. | SIL OFL 1.1, text in `crates/aristotle-ui/assets/licenses/MaShanZheng-OFL.txt` |
| `tools/font-cjk.py`, the device bundle's `DejaVuSans-Symbols.ttf` | [Noto Sans SC](https://github.com/notofonts/noto-cjk) ideographs merged into the fallback font; [DejaVu Sans](https://dejavu-fonts.github.io/) symbols. The Noto licence text is in `crates/aristotle-ui/assets/test-fonts/Noto-OFL.txt`; the DejaVu licence is in the build bundle. | SIL OFL 1.1 (Noto Sans SC), Bitstream Vera (DejaVu Sans) |
| `engine/` (`crengine` feature), `engine/vendor/koreader-crengine` | [koreader/crengine](https://github.com/koreader/crengine) at the commit pinned by `engine/fetch.sh`, the layout engine for EPUB and TXT. The build also links xxHash (BSD-2-Clause), FriBiDi (LGPL-2.1-or-later), libunibreak (zlib), utf8proc (MIT) and Zstandard (BSD-3-Clause, also GPL-2.0-or-later). | GPL-2.0-or-later and the licences named for each library |

## In the device bundle

`tools/build-render.sh` adds a `licenses/` folder and a `SOURCE.txt` to the
bundle. `tools/bundle-licenses.py` generates them from what the build used.
In short:

- The document engine is Plato's `plato-core` (AGPL-3.0) with MuPDF (AGPL-3.0),
  jbig2dec (AGPL-3.0), FreeType (FTL or GPL-2.0), HarfBuzz (MIT), libjpeg (IJG),
  libpng, OpenJPEG (BSD-2-Clause), zlib, bzip2, Gumbo (Apache-2.0) and Little
  CMS (MIT).
- The runtime is musl (MIT), LLVM's libc++, libc++abi and libunwind (Apache-2.0
  with LLVM exception), the Rust standard library (MIT or Apache-2.0) and
  about a hundred Rust crates, listed with their licences in
  `licenses/rust/CRATES.txt`. Page snapshots use Zstandard (BSD) through
  `zstd-sys`.
- The fonts are Literata, Noto Sans SC, Noto Sans, Noto Sans Coptic,
  Libertinus Serif, Source Code Pro, Parisienne and Delius (SIL OFL 1.1),
  DejaVu Sans (Bitstream Vera licence), Ma Shan Zheng (SIL OFL 1.1, rendered
  slogan only), and inside MuPDF the URW base 14 fonts and Noto Serif.
- The layout engine for EPUB and TXT is koreader/crengine (GPL-2.0-or-later),
  with xxHash, FriBiDi, libunibreak, utf8proc and Zstandard.
- Hyphenation patterns for Polish, English (US and GB), Ancient Greek and
  Latin from [hyph-utf8](https://github.com/hyphenation/tex-hyphen), compiled
  by [kl-hyphenate](https://github.com/baskerville/kl-hyphenate). Each is
  under the licence named in its master file, which the bundle includes.

As AGPL-3.0 section 6 requires, `SOURCE.txt` names the Aristotle commit, the
Plato revision and patches, and the exact C library sources.

## Not included

Dictionaries, books and the stock Kindle software are not part of Aristotle.
The notes on the stock reader's file formats (`docs/library.md`,
`crates/aristotle-library/src/sidecar.rs`) come from interoperability analysis
and contain no Amazon code.
