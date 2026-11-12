# 第三方许可证

本目录保存发布包需要随附、但代码树里没有放在源文件旁边的许可证文本。完整清单见
[`aristotle/THIRD_PARTY.md`](../aristotle/THIRD_PARTY.md)。

| 文件 | 对象 | 许可证 |
|---|---|---|
| `Literata-OFL.txt` | Literata 字体 | SIL OFL 1.1 |
| `NotoSansSC-OFL.txt` | Noto Sans SC（并入设备包回退字体） | SIL OFL 1.1 |
| `MaShanZheng-OFL.txt` | 马善政楷书（状态栏标语，只发布渲染图） | SIL OFL 1.1 |
| `OpenCC-LICENSE.txt` | OpenCC 简繁单字映射表 | Apache-2.0 |
| `pinyin-LICENSE.txt` | `pinyin` crate（Pinyin sort） | MIT |
| `rime-pinyin-simp-LICENSE.txt` | rime-pinyin-simp（拼音输入候选表） | Apache-2.0 |
| `koreader-crengine-COPYING.txt` | koreader/crengine（EPUB/TXT 排版引擎） | GPL-2.0-or-later |

设备包里的 `licenses/`、`SOURCE.txt`、`C-SOURCES.txt` 与 `ENGINE-SOURCES.txt`
由 `aristotle/tools/bundle-licenses.py` 在构建时生成；源码包由
`aristotle/tools/package.sh source` 生成。本目录保证从仓库直接取用这些文本时也有据可查。
