#!/usr/bin/env python3
"""生成测试书 fixtures/cache.epub。

这本书没有别的目的：够大。

crengine 只有两种情况下才用文档缓存（lvdocview.cpp）：
- 读缓存：源文件大于 DOCUMENT_CACHING_MIN_SIZE（64 KB）；
- 写缓存：源文件大于 30 KB（lvdocview.cpp 的 swapToCache）。

fixtures/ 下现有的几本测试书都只有几 KB，够不到这两个门槛，缓存那条路
根本走不到。所以这里专门造一本 80 KB 左右的书。

章节正文不压缩（ZIP_STORED）：书的大小就是正文的字数，生成出来多大是
多少，不用猜压缩率。

用法：
    python3 fixtures/make_cache_epub.py
"""

import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "cache.epub")

# 缓存门槛是 64 KB（DOCUMENT_CACHING_MIN_SIZE）。正文给到 80 KB 上下，
# 留出余量，别让改动把书压到门槛以下。
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
    <dc:title>缓存测试书</dc:title>
    <dc:creator>墨阅项目组</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="BookId">urn:uuid:inkread-cache-fixture-0001</dc:identifier>
    <dc:description>够大的测试书，用来走 crengine 的文档缓存那条路。</dc:description>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
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

TOC_NCX = """<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="urn:uuid:inkread-cache-fixture-0001"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle>
    <text>缓存测试书</text>
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

# 填充用的段落。内容不重要，重要的是够长：每章要撑到 20 KB。
PARAGRAPHS = [
    "电子墨水屏和常见的液晶显示屏不一样：它没有背光，靠黑白颗粒在电场作用下改变状态来显示图文。"
    "因此刷新速度慢，但断电后画面依然保留，待机功耗也低得多，适合拿来看长篇。",
    "把书装进墨水屏设备，真正难的地方不在于把文字画出来，而在于把一整本书切成大小合适的页面。"
    "行距、字号、页边距、留白，任何一项不合适，长时间阅读都会累。",
    "排版引擎把连续文本切成固定大小的页面。切分的位置由字号、行距、页边距共同决定："
    "改一个数字，整本书的分页都会跟着变，缓存里存的那份排版结果也就不能再用了。",
    "本书由脚本自动生成，只为把文件撑到缓存的门槛以上。文字重复多少遍都不影响测试结果，"
    "测试比的是两次打开给出的页数、XPointer 与像素是不是一样。",
    "中文段落必须整段写在一行里。HTML 会把段落中间的换行和缩进折叠成一个空格，"
    "而 crengine 没有实现 CSS Text 3 里那条「两个 CJK 字符之间的空白折叠为无」的规则，"
    "所以段内换行会在中文句子中间留下一个可见的空格。",
    "英文段落可以折行：折行处折叠出来的空格正好是词间该有的空格。"
    "这一段是英文的，用来确认混排的时候拉丁字与汉字的基线还是对齐的。",
]


def chapter_body(index: int) -> str:
    """一章正文，撑到 CHAPTER_BYTES 左右。"""
    parts = [
        '<?xml version="1.0" encoding="UTF-8"?>\n',
        '<html xmlns="http://www.w3.org/1999/xhtml">\n<head>\n  <title>第{}章</title>\n</head>\n<body>\n'.format(
            index
        ),
        "  <h1>第{}章 · 填充</h1>\n".format(index),
    ]
    size = sum(len(p.encode("utf-8")) for p in parts)
    n = 0
    while size < CHAPTER_BYTES:
        text = PARAGRAPHS[(index + n) % len(PARAGRAPHS)]
        para = "  <p>{}</p>\n".format(text)
        parts.append(para)
        size += len(para.encode("utf-8"))
        n += 1
    parts.append("</body>\n</html>\n")
    return "".join(parts)


def main() -> None:
    # mimetype 必须是第一个条目，而且不压缩。
    with zipfile.ZipFile(OUT, "w") as epub:
        epub.writestr(
            zipfile.ZipInfo("mimetype"), "application/epub+zip", compress_type=zipfile.ZIP_STORED
        )
        epub.writestr("META-INF/container.xml", CONTAINER_XML, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/content.opf", CONTENT_OPF, compress_type=zipfile.ZIP_DEFLATED)
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
