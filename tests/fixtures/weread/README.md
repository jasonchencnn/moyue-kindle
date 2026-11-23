# WeRead 章节包测试 fixture（WR-003）

`synthetic-*` 数据，无任何真实账号、vid、bookId、正文或图片。用于验证
**传统 ZipCrypto** 解密（AC-02）与合成三章 EPUB 重建（AC-03/AC-04）。

- 文件：`chapter-package.zip.b64`（把真实加密 ZIP 做成 base64 文本入库）
- 解出后 SHA-256：`334f3d0428fbc59fca1ad464e77b135231ea4d1b2abcc7f5ce29e0dae8d578d0`
- ZIP 密码（合成）：`synthetic-zip-pass`
- 内容：
  - `info.txt`：`chapters[]` 列出章 0/1/2 的正文文件（章 1/2 故意与 TOC `files[]` 不同，
    以证明 `info.txt` 是正文路径事实来源）
  - `Text/0.xhtml`（公共资源占位）
  - `Text/ch1.xhtml`、`Text/ch2.xhtml`（单文档、合法 XHTML，含图片/样式引用）
  - `Images/cover.png`、`Images/inline.png`（1×1 合成 PNG）
  - `Styles/shared.css`、`Styles/ch2.css`

## 生成命令（离线，Info-ZIP）

```bash
rm -rf /tmp/wr003-fixture /tmp/wr003-chapter-package.zip
mkdir -p /tmp/wr003-fixture/{Text,Images,Styles}
# …写入上述合成文本/图片…
cd /tmp/wr003-fixture
zip -r -X -P 'synthetic-zip-pass' /tmp/wr003-chapter-package.zip Text Images Styles info.txt

# 确认是传统 ZipCrypto（file security status: encrypted）
zipinfo -v /tmp/wr003-chapter-package.zip | grep 'file security status'

base64 -i /tmp/wr003-chapter-package.zip | tr -d '\n' > chapter-package.zip.b64
printf '\n' >> chapter-package.zip.b64
sha256sum /tmp/wr003-chapter-package.zip
```

测试通过 `include_str!` 读取 `.b64` 再 base64 解码，不复用被测加解密代码生成数据。
