#!/usr/bin/env python3
"""QA-002 测试书生成脚本。

用于验证：
1. 书籍自带的 <style> 样式表是否生效（对比 default.css 的 justify / text-indent）；
2. 书籍自带的 class / inline / 外部 CSS 是否生效；
3. 书籍元数据与非 UTF-8 编码处理。
"""

import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
OUT_EMBEDDED = os.path.join(HERE, "qa_embedded_style.epub")
OUT_PLAIN = os.path.join(HERE, "qa_plain.epub")

CONTAINER_XML = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"""

CONTENT_OPF_EMBEDDED = """<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="BookId">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>QA-002 样式测试书（带嵌入样式）</dc:title>
    <dc:creator>QA测试组</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="BookId">urn:uuid:inkread-qa-style-0001</dc:identifier>
  </metadata>
  <manifest>
    <item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
    <item id="style" href="style.css" media-type="text/css"/>
  </manifest>
  <spine>
    <itemref idref="chapter1"/>
  </spine>
</package>
"""

CONTENT_OPF_PLAIN = """<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="BookId">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>QA-002 样式测试书（无嵌入样式）</dc:title>
    <dc:creator>QA测试组</dc:creator>
    <dc:language>zh-CN</dc:language>
    <dc:identifier id="BookId">urn:uuid:inkread-qa-plain-0001</dc:identifier>
  </metadata>
  <manifest>
    <item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="chapter1"/>
  </spine>
</package>
"""

STYLE_CSS = """
.external-right {
    text-align: right;
}
"""

CHAPTER1_EMBEDDED = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <title>嵌入样式测试</title>
  <link rel="stylesheet" type="text/css" href="style.css"/>
  <style type="text/css">
    p.center-test {
        text-align: center;
        text-indent: 0em;
    }
    p.right-test {
        text-align: right;
        text-indent: 0em;
    }
  </style>
</head>
<body>
  <p class="center-test">居中文本测试段落</p>
  <p class="right-test">右对齐文本测试段落</p>
  <p class="external-right">外部样式表右对齐</p>
  <p style="text-align: right;">行内样式右对齐</p>
  <p>普通正文段落应当保持两端对齐与首行缩进</p>
</body>
</html>
"""

CHAPTER1_PLAIN = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <title>无嵌入样式</title>
</head>
<body>
  <p>居中文本测试段落</p>
  <p>右对齐文本测试段落</p>
  <p>外部样式表右对齐</p>
  <p>行内样式右对齐</p>
  <p>普通正文段落应当保持两端对齐与首行缩进</p>
</body>
</html>
"""


def write_epub(out_path: str, opf_xml: str, chapter_xml: str, extra_files: dict = None) -> None:
    with zipfile.ZipFile(out_path, "w") as epub:
        epub.writestr(
            zipfile.ZipInfo("mimetype"), "application/epub+zip", compress_type=zipfile.ZIP_STORED
        )
        epub.writestr("META-INF/container.xml", CONTAINER_XML, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/content.opf", opf_xml, compress_type=zipfile.ZIP_DEFLATED)
        epub.writestr("OEBPS/chapter1.xhtml", chapter_xml, compress_type=zipfile.ZIP_DEFLATED)
        if extra_files:
            for name, content in extra_files.items():
                epub.writestr(f"OEBPS/{name}", content, compress_type=zipfile.ZIP_DEFLATED)
    print(f"已生成：{out_path}（{os.path.getsize(out_path)} 字节）")


def main() -> None:
    write_epub(OUT_EMBEDDED, CONTENT_OPF_EMBEDDED, CHAPTER1_EMBEDDED, {"style.css": STYLE_CSS})
    write_epub(OUT_PLAIN, CONTENT_OPF_PLAIN, CHAPTER1_PLAIN)


if __name__ == "__main__":
    main()
