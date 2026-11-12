#!/usr/bin/env python3
"""生成测试书 fixtures/interactive.epub。

用于验证 APP-003 的各项交互功能：
- 选词与高亮（中英文单字与词组）
- 链接（书内锚点跳转 + 书外 URL）
- 脚注（带 epub:type="noteref" / role="doc-noteref"，能通过 notes.rs 解析出正文）
- 图片（带独立图片文件，用于验证夜间模式图片反色）
"""

import os
import struct
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "interactive.epub")


def make_png(w: int, h: int, fill_gray: int) -> bytes:
    """生成 8 位单通道灰度 PNG 图像。"""
    raw_data = bytearray()
    for y in range(h):
        raw_data.append(0)  # filter type 0: None
        for x in range(w):
            raw_data.append(fill_gray)
    compressed = zlib.compress(raw_data)

    png = bytearray(b"\x89PNG\r\n\x1a\n")
    # IHDR
    ihdr_data = struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0)
    ihdr_crc = zlib.crc32(b"IHDR" + ihdr_data)
    png += struct.pack(">I", len(ihdr_data)) + b"IHDR" + ihdr_data + struct.pack(">I", ihdr_crc)
    # IDAT
    idat_crc = zlib.crc32(b"IDAT" + compressed)
    png += struct.pack(">I", len(compressed)) + b"IDAT" + compressed + struct.pack(">I", idat_crc)
    # IEND
    iend_crc = zlib.crc32(b"IEND")
    png += struct.pack(">I", 0) + b"IEND" + struct.pack(">I", iend_crc)
    return bytes(png)


CONTAINER_XML = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"""

CONTENT_OPF = """<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="BookId">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>墨阅交互测试样张</dc:title>
    <dc:creator>墨阅项目组</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="BookId">urn:uuid:inkread-app003-interactive-0001</dc:identifier>
    <dc:description>APP-003 测试书，含词、高亮、内链、外链、脚注与图片。</dc:description>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
    <item id="chapter2" href="chapter2.xhtml" media-type="application/xhtml+xml"/>
    <item id="test_image" href="images/test_image.png" media-type="image/png"/>
  </manifest>
  <spine toc="ncx">
    <itemref idref="chapter1"/>
    <itemref idref="chapter2"/>
  </spine>
</package>
"""

TOC_NCX = """<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="urn:uuid:inkread-app003-interactive-0001"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle>
    <text>墨阅交互测试样张</text>
  </docTitle>
  <navMap>
    <navPoint id="navpoint-1" playOrder="1">
      <navLabel>
        <text>第一章 · 正文与交互</text>
      </navLabel>
      <content src="chapter1.xhtml"/>
    </navPoint>
    <navPoint id="navpoint-2" playOrder="2">
      <navLabel>
        <text>第二章 · 锚点与附注</text>
      </navLabel>
      <content src="chapter2.xhtml"/>
    </navPoint>
  </navMap>
</ncx>
"""

CHAPTER1 = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head>
  <title>第一章 · 正文与交互</title>
</head>
<body>
  <h1>第一章 · 正文与交互</h1>
  <p>墨水屏阅读器在显示书籍时，需要支持选词、高亮、词典查询等常用交互功能。汉字在排版引擎中通常单字成词。<a href="chapter2.xhtml#fn-1" id="ref-1" class="footnote" epub:type="noteref" role="doc-noteref">[1]</a> 读者点击脚注标记时，应当弹出浮窗显示注释内容。</p>
  <p>这一段包含一个书内跳转链接：<a href="chapter2.xhtml#target-sec">前往第二章锚点目标</a>。同时包含一个书外地址链接：<a href="https://example.com/inkread">墨阅项目主页链接</a>。</p>
  <p>下面是一张测试图片，用于验证夜间模式下的图片反色特性：在开启夜间模式时图片区域应执行预反色，从而在屏幕全屏反色后恢复正常的正色呈现。</p>
  <p><img src="images/test_image.png" alt="测试图片" width="120" height="80" /></p>
  <p>English text section: An electronic ink display provides high contrast paper-like reading experience without emitting active backlight. Selected words can be highlighted and retrieved across different layout font sizes safely.</p>
</body>
</html>
"""

CHAPTER2 = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head>
  <title>第二章 · 锚点与附注</title>
</head>
<body>
  <h1 id="chapter2-title">第二章 · 锚点与附注</h1>
  <p>本章包含第一章中各个链接的落点目标以及注释文本。</p>
  <h2 id="target-sec">第二章锚点目标</h2>
  <p>这里是内部跳转链接的落点目标段落。通过 XPointer 或锚点定位到本页，验证跨章节跳转是否准确无误。</p>
  <aside id="fn-1" class="footnote" epub:type="footnote" role="doc-footnote">
    <p>【注 1】这是第一条测试脚注的正文内容，详细解释了正文的概念与注释细节。</p>
  </aside>
  <p>补充测试段落：改动字号后重新排版，验证此前标记的高亮选区依然能按 XPointer 定位到同一段文本内容。</p>
</body>
</html>
"""


def main() -> None:
    image_bytes = make_png(120, 80, 70)
    with zipfile.ZipFile(OUT, "w") as epub:
        epub.writestr(
            zipfile.ZipInfo("mimetype"),
            "application/epub+zip",
            compress_type=zipfile.ZIP_STORED,
        )
        epub.writestr("META-INF/container.xml", CONTAINER_XML, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/content.opf", CONTENT_OPF, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/toc.ncx", TOC_NCX, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/chapter1.xhtml", CHAPTER1, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/chapter2.xhtml", CHAPTER2, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/images/test_image.png", image_bytes, compress_type=zipfile.ZIP_DEFLATED)
    print(f"已生成：{OUT}（{os.path.getsize(OUT)} 字节）")


if __name__ == "__main__":
    main()
