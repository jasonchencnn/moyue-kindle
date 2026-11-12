#!/usr/bin/env python3
"""生成带内嵌 CSS 与外链 CSS 的缓存测试书 fixtures/cache_css.epub。

crengine 只有在源文件大于 DOCUMENT_CACHING_MIN_SIZE（64 KB）时才会走读缓存。
本书不仅大小超过 64 KB，而且包含：
- 外链样式表 style.css（由 <link rel="stylesheet"> 引用）；
- 章节内的内嵌 <style> 块；
- 使用了 em 相对字号、首行缩进、外边距、强调样式的正文段落。

用来验证 ENG-009：带 CSS 的书籍第二次打开时命中排版缓存，免重排。
"""

import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "cache_css.epub")

CHAPTER_BYTES = 20 * 1024

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
    <dc:title>带样式缓存测试书</dc:title>
    <dc:creator>墨阅项目组</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="BookId">urn:uuid:inkread-cache-css-fixture-0001</dc:identifier>
    <dc:description>带内嵌与外链 CSS 的大型测试书，用于验证排版缓存命中与免重排。</dc:description>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="css" href="style.css" media-type="text/css"/>
    <item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
    <item id="chapter2" href="chapter2.xhtml" media-type="application/xhtml+xml"/>
    <item id="chapter3" href="chapter3.xhtml" media-type="application/xhtml+xml"/>
    <item id="chapter4" href="chapter4.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx">
    <itemref idref="chapter1"/>
    <itemref idref="chapter2"/>
    <itemref idref="chapter3"/>
    <itemref idref="chapter4"/>
  </spine>
</package>
"""

STYLE_CSS = """
body {
  margin: 1em;
  font-size: 1em;
  line-height: 1.4;
}
h1.chapter-title {
  font-size: 1.5em;
  text-align: center;
  margin-top: 1em;
  margin-bottom: 0.8em;
}
p.lead {
  font-size: 1.1em;
  font-weight: bold;
  margin-bottom: 0.6em;
}
p.indent {
  text-indent: 2em;
  margin-top: 0.4em;
  margin-bottom: 0.4em;
}
p.quote {
  font-size: 0.9em;
  margin-left: 2em;
  margin-right: 2em;
}
span.em-tag {
  font-style: italic;
}
"""

TOC_NCX = """<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="urn:uuid:inkread-cache-css-fixture-0001"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle>
    <text>带样式缓存测试书</text>
  </docTitle>
  <navMap>
    <navPoint id="navpoint-1" playOrder="1">
      <navLabel>
        <text>第一章</text>
      </navLabel>
      <content src="chapter1.xhtml"/>
    </navPoint>
    <navPoint id="navpoint-2" playOrder="2">
      <navLabel>
        <text>第二章</text>
      </navLabel>
      <content src="chapter2.xhtml"/>
    </navPoint>
    <navPoint id="navpoint-3" playOrder="3">
      <navLabel>
        <text>第三章</text>
      </navLabel>
      <content src="chapter3.xhtml"/>
    </navPoint>
    <navPoint id="navpoint-4" playOrder="4">
      <navLabel>
        <text>第四章</text>
      </navLabel>
      <content src="chapter4.xhtml"/>
    </navPoint>
  </navMap>
</ncx>
"""

PARAGRAPHS = [
    "电子墨水屏和常见的液晶显示屏不同：它依赖双稳态微胶囊在电场作用下定向移动来呈现字形。"
    "由于反射外界自然光，墨水屏阅读体验非常接近传统纸质书，且完全没有背光频闪带来的眼睛酸胀感。",
    "排版引擎需要把长篇小说流式切分为固定设备视口大小的页面序列。行距、段间距、基线对齐与字阶阶梯，"
    "任何参数的微调都会引发全书各段落分页点的多米诺骨牌式变迁。",
    "带内嵌样式表和外部 CSS 的电子书在解析与排版时有着两套不同的样式计算生命周期："
    "装订加载时计算一次初始样式，而在最终渲染时会结合当前视口宽度与默认字阶重新解析级联规则。",
    "当文档缓存启用时，已排版的页面流与段落切分数据直接序列化到磁盘缓存文件中。"
    "再次开书时，若排版上下文和样式哈希完全一致，则应当毫秒级命中缓存，免除昂贵的全文重排流程。",
    "在墨水屏设备有限的 CPU 算力与电池容量限制下，重开大部头书籍免重排至关重要。"
    "KoReader 与墨阅皆致力于提供丝滑顺畅的开书翻页体验。",
]


def chapter_body(index: int) -> str:
    parts = [
        '<?xml version="1.0" encoding="UTF-8"?>\n',
        '<html xmlns="http://www.w3.org/1999/xhtml">\n<head>\n',
        "  <title>第{}章</title>\n".format(index),
        '  <link rel="stylesheet" type="text/css" href="style.css"/>\n',
        '  <style type="text/css">\n',
        "    .chapter-custom-{} {{ text-decoration: none; }}\n".format(index),
        "    .footnote-{} {{ font-size: 0.85em; margin-top: 1em; }}\n".format(index),
        "  </style>\n",
        "</head>\n<body>\n",
        '  <h1 class="chapter-title chapter-custom-{}">第{}章 · 样式与排版</h1>\n'.format(
            index, index
        ),
        '  <p class="lead">导读：本章节测试内嵌 CSS 与外链 CSS 在文档缓存机制下的复用表现。</p>\n',
    ]
    size = sum(len(p.encode("utf-8")) for p in parts)
    n = 0
    while size < CHAPTER_BYTES:
        text = PARAGRAPHS[(index + n) % len(PARAGRAPHS)]
        cls = "indent" if n % 3 != 0 else "quote"
        para = '  <p class="{}">{}<span class="em-tag">【第{}节标记】</span></p>\n'.format(
            cls, text, n + 1
        )
        parts.append(para)
        size += len(para.encode("utf-8"))
        n += 1
    parts.append(
        '  <p class="footnote-{}">本章尾注：字数统计达标，进入下一章节。</p>\n'.format(index)
    )
    parts.append("</body>\n</html>\n")
    return "".join(parts)


def main() -> None:
    with zipfile.ZipFile(OUT, "w") as epub:
        epub.writestr(
            zipfile.ZipInfo("mimetype"), "application/epub+zip", compress_type=zipfile.ZIP_STORED
        )
        epub.writestr("META-INF/container.xml", CONTAINER_XML, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/content.opf", CONTENT_OPF, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/style.css", STYLE_CSS, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/toc.ncx", TOC_NCX, compress_type=zipfile.ZIP_DEFLATED)
        for index in range(1, 5):
            epub.writestr(
                "OEBPS/chapter{}.xhtml".format(index),
                chapter_body(index),
                compress_type=zipfile.ZIP_STORED,
            )
    print("已生成：{}（{} 字节）".format(OUT, os.path.getsize(OUT)))


if __name__ == "__main__":
    main()
