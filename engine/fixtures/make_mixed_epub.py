#!/usr/bin/env python3
"""生成测试书 fixtures/mixed.epub。

一版书里同时有拉丁文和汉字，用来验证字体回退：主字体用 Literata（只有拉丁
字形），汉字交给回退字体（设备包里的中文字体）。

两段各占一行，互不影响对方的排版：
- 第一段纯拉丁文：回退字体不该碰它，字形宽度必须和没有回退时一致；
- 第二段纯汉字：主字体没有这些字形，宽度必须和「直接用中文字体排版」时一致。

段落顺序也有讲究：拉丁文在前，所以汉字那一行的位置只由拉丁文那一行决定，
换字体不会让汉字挪位置，两种排版可以逐像素对比。

最后一段是隐藏的（display:none）：它不进排版，位置算不出来。用它的
XPointer 来验证「定位不到」会报错，而不是变成第 0 页。

用法：
    python3 fixtures/make_mixed_epub.py
"""

import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "mixed.epub")

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
    <dc:title>墨阅字体回退样张</dc:title>
    <dc:creator>墨阅项目组</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="BookId">urn:uuid:inkread-spike-mixed-0001</dc:identifier>
    <dc:description>crengine-ng 字体回退用的最小测试书，一段拉丁文一段汉字。</dc:description>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx">
    <itemref idref="chapter1"/>
  </spine>
</package>
"""

TOC_NCX = """<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="urn:uuid:inkread-spike-mixed-0001"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle>
    <text>墨阅字体回退样张</text>
  </docTitle>
  <navMap>
    <navPoint id="navpoint-1" playOrder="1">
      <navLabel>
        <text>回退样张</text>
      </navLabel>
      <content src="chapter1.xhtml"/>
    </navPoint>
  </navMap>
</ncx>
"""

# 拉丁文段落整段写在一行里：段内换行会折叠成一个空格，中文段落尤其明显。
# 最后一段是隐藏的：XPointer 指向它时位置算不出来，用来验证定位失败会报错。
CHAPTER1 = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <title>回退样张</title>
</head>
<body>
  <p>Alpha Bravo Charlie</p>
  <p>墨阅汉字排版</p>
  <p style="display:none">Hidden 隐藏</p>
</body>
</html>
"""


def main() -> None:
    # mimetype 必须是第一个条目，而且不压缩。
    with zipfile.ZipFile(OUT, "w") as epub:
        epub.writestr(
            zipfile.ZipInfo("mimetype"), "application/epub+zip", compress_type=zipfile.ZIP_STORED
        )
        epub.writestr("META-INF/container.xml", CONTAINER_XML, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/content.opf", CONTENT_OPF, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/toc.ncx", TOC_NCX, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/chapter1.xhtml", CHAPTER1, compress_type=zipfile.ZIP_DEFLATED)
    print("已生成：{}（{} 字节）".format(OUT, os.path.getsize(OUT)))


if __name__ == "__main__":
    main()
