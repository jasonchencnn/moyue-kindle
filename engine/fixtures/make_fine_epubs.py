#!/usr/bin/env python3
"""Generate Chinese refined-typesetting EPUB fixtures for engine tests:

1. fine_duokan.epub — Duokan (多看) refined EPUB specification:
   - OPF spine `properties="duokan-page-fullscreen"` cover page
   - `<a class="duokan-footnote"><img src="../Images/note.png" alt="注"/></a>`
     image-only footnote link + `<ol class="duokan-footnote-content">`
   - `<div class="duokan-image-single">` with `.duokan-image-maintitle` &
     `.duokan-image-des`
   - Centered epigraph/caption (`text-align: center; text-indent: 0`) and
     deliberate `U+3000` poetry alignment

2. fine_modern.epub — WeRead / Douban / EPUB3 modern refined media & formulas:
   - Inline `<svg>` chapter divider + `<svg><text>`
   - Real WebP (VP8L lossless) image `<img src="../Images/photo.webp"/>`
   - Real GIF89a image `<img src="../Images/badge.gif"/>`
   - Real MathML `<math>` fraction formula
   - Modern CSS3 `display: flex` callout box + `<figure>`/`<figcaption>` + `<ruby>`
"""

from pathlib import Path
import struct
import zlib
import zipfile

OUT_DIR = Path(__file__).resolve().parent


def make_png(width: int, height: int, fill_gray: int, border_gray: int = 0) -> bytes:
    """Create a valid 8-bit grayscale PNG with a solid border."""
    rows = []
    for y in range(height):
        row = bytearray([0])  # filter type 0
        for x in range(width):
            if x < 3 or x >= width - 3 or y < 3 or y >= height - 3:
                row.append(border_gray)
            else:
                row.append(fill_gray)
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def make_gif(width: int = 120, height: int = 48) -> bytes:
    """Create a valid minimal dark-filled GIF89a image (pure Python, 9-bit LZW)."""
    # 256-entry global color table (size flag = 7 -> 2^(7+1)=256):
    # index 0 = dark (#202020), index 1..255 = white (#FFFFFF).
    header = b"GIF89a"
    lsd = struct.pack("<HHBBB", width, height, 0x87, 0, 0)
    gct = bytes([0x20, 0x20, 0x20]) + bytes([0xFF, 0xFF, 0xFF]) * 255
    img_desc = struct.pack("<BHHHHB", 0x2C, 0, 0, width, height, 0)

    # With LZW min_code_size = 8, CLEAR = 256, EOI = 257, 9-bit codes stay valid
    # for up to 254 symbols before reaching code 512; emit CLEAR every 200 pixels.
    bits = 0
    bit_count = 0
    out = bytearray()

    def write_code(code: int) -> None:
        nonlocal bits, bit_count
        bits |= (code & 0x1FF) << bit_count
        bit_count += 9
        while bit_count >= 8:
            out.append(bits & 0xFF)
            bits >>= 8
            bit_count -= 8

    write_code(256)  # CLEAR
    run = 0
    for _ in range(width * height):
        write_code(0)
        run += 1
        if run == 200:
            write_code(256)
            run = 0
    write_code(257)  # EOI
    if bit_count > 0:
        out.append(bits & 0xFF)

    sub_blocks = bytearray([8])  # LZW minimum code size = 8
    for i in range(0, len(out), 255):
        blk = out[i : i + 255]
        sub_blocks.append(len(blk))
        sub_blocks.extend(blk)
    sub_blocks.append(0)
    return header + lsd + gct + img_desc + bytes(sub_blocks) + b"\x3B"


def make_webp_lossless(width: int = 96, height: int = 48) -> bytes:
    """Create a valid minimal VP8L lossless WebP image (solid dark gray block)."""
    # VP8L bitstream:
    # - 1 byte signature 0x2F
    # - 14 bits (width - 1), 14 bits (height - 1), 1 bit alpha_is_used=0, 3 bits version=0
    # - 1 bit transform_present=0
    # - 1 bit color_cache_present=0
    # - 1 bit meta_prefix_codes=0
    # - 5 prefix code groups (Green, Red, Blue, Alpha, Distance), each simple 1-symbol code:
    #   bit 1 (is_simple=1), bit 0 (num_symbols-1=0 -> 1 symbol), bit 1 (is_first_8bits=1),
    #   8 bits symbol value. With 1 symbol per tree, pixel data takes 0 bits!
    bitbuf = 0
    nbits = 0
    payload = bytearray([0x2F])

    def put(val: int, count: int) -> None:
        nonlocal bitbuf, nbits
        bitbuf |= (val & ((1 << count) - 1)) << nbits
        nbits += count
        while nbits >= 8:
            payload.append(bitbuf & 0xFF)
            bitbuf >>= 8
            nbits -= 8

    put(width - 1, 14)
    put(height - 1, 14)
    put(0, 1)  # alpha_is_used
    put(0, 3)  # version
    put(0, 1)  # no transforms
    put(0, 1)  # no color cache
    put(0, 1)  # single prefix group

    # Green=0x30, Red=0x30, Blue=0x30, Alpha=0xFF, Distance=0
    for sym in (0x30, 0x30, 0x30, 0xFF, 0x00):
        put(1, 1)  # simple code
        put(0, 1)  # 1 symbol
        put(1, 1)  # 8-bit symbol
        put(sym, 8)

    if nbits > 0:
        payload.append(bitbuf & 0xFF)

    vp8l_data = bytes(payload)
    padded = vp8l_data + (b"\x00" if len(vp8l_data) % 2 else b"")
    riff_size = 4 + 8 + len(padded)
    return b"RIFF" + struct.pack("<I", riff_size) + b"WEBP" + b"VP8L" + struct.pack("<I", len(vp8l_data)) + padded


def write_epub(path: Path, files: dict[str, bytes]) -> None:
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("mimetype", b"application/epub+zip", compress_type=zipfile.ZIP_STORED)
        for name, data in files.items():
            zf.writestr(name, data, compress_type=zipfile.ZIP_DEFLATED)


CONTAINER_XML = b"""<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"""


def build_duokan_epub() -> None:
    cover_png = make_png(360, 480, fill_gray=0xB0, border_gray=0x10)
    illus_png = make_png(320, 160, fill_gray=0x90, border_gray=0x20)
    note_icon_png = make_png(28, 28, fill_gray=0x30, border_gray=0x00)

    opf = b"""<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid" xml:lang="zh-CN">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:moyue:fine-duokan</dc:identifier>
    <dc:title>\xe5\xa4\x9a\xe7\x9c\x8b\xe7\xb2\xbe\xe6\x8e\x92\xe6\xb5\x8b\xe8\xaf\x95\xe6\xa0\xb7\xe4\xb9\xa6</dc:title>
    <dc:language>zh-CN</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="css" href="Styles/book.css" media-type="text/css"/>
    <item id="cover-page" href="Text/cover.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch1" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img-cover" href="Images/cover.png" media-type="image/png" properties="cover-image"/>
    <item id="img-illus" href="Images/illus.png" media-type="image/png"/>
    <item id="img-note" href="Images/note.png" media-type="image/png"/>
  </manifest>
  <spine>
    <itemref idref="cover-page" properties="duokan-page-fullscreen"/>
    <itemref idref="ch1"/>
  </spine>
</package>
"""
    nav = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="zh-CN">
<head><title>目录</title></head>
<body>
<nav epub:type="toc">
  <ol>
    <li><a href="Text/cover.xhtml">封面</a></li>
    <li><a href="Text/ch1.xhtml">第一章 多看精排样章</a></li>
  </ol>
</nav>
</body>
</html>
""".encode("utf-8")

    book_css = """
.subtitle {
    text-align: center;
    text-indent: 0;
    font-size: 0.9em;
    color: #555555;
    margin-top: 0.4em;
    margin-bottom: 1.2em;
}
.poem {
    text-align: left;
    text-indent: 0;
    margin-left: 2em;
    margin-top: 0.8em;
    margin-bottom: 0.8em;
}
.scene-break {
    margin-top: 1.8em;
}
""".encode("utf-8")

    cover_xhtml = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh-CN">
<head><title>封面</title><link rel="stylesheet" type="text/css" href="../Styles/book.css"/></head>
<body>
  <p><img src="../Images/cover.png" alt="封面"/></p>
</body>
</html>
""".encode("utf-8")

    ch1_xhtml = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="zh-CN">
<head><title>第一章 多看精排样章</title><link rel="stylesheet" type="text/css" href="../Styles/book.css"/></head>
<body>
  <h1>第一章 多看精排样章</h1>
  <p class="subtitle">—— 题记居中不得偏右 ——</p>
  <p>中文精排讲究字格齐整、标点挤压与中西文混排留白，例如 Readest 与 KOReader 在 300ppi 墨水屏上的对比测试<a class="duokan-footnote" href="#df-1"><img src="../Images/note.png" alt="注"/></a>。「“引号”与《书名号》」紧邻时应自动压缩半角空白。</p>
  <div class="duokan-image-single">
    <img src="../Images/illus.png" alt="插图"/>
    <p class="duokan-image-maintitle">图 1-1 多看单图主标题</p>
    <p class="duokan-image-des">图示说明文字：居中显示且与主图不拆页。</p>
  </div>
  <p class="poem">\u3000\u3000床前明月光，疑是地上霜。</p>
  <p class="scene-break">第二段带有明显的场景分隔上边距，精排模式下不应被强制压扁为零。</p>
  <ol class="duokan-footnote-content">
    <li class="duokan-footnote-item" id="df-1">
      <p>多看富文本页下注：支持点击小图标弹窗，同时在引用页底部自动呈现页下注。</p>
    </li>
  </ol>
</body>
</html>
""".encode("utf-8")

    write_epub(
        OUT_DIR / "fine_duokan.epub",
        {
            "META-INF/container.xml": CONTAINER_XML,
            "OEBPS/content.opf": opf,
            "OEBPS/nav.xhtml": nav,
            "OEBPS/Styles/book.css": book_css,
            "OEBPS/Text/cover.xhtml": cover_xhtml,
            "OEBPS/Text/ch1.xhtml": ch1_xhtml,
            "OEBPS/Images/cover.png": cover_png,
            "OEBPS/Images/illus.png": illus_png,
            "OEBPS/Images/note.png": note_icon_png,
        },
    )


def build_modern_epub() -> None:
    webp_bytes = make_webp_lossless(200, 80)
    gif_bytes = make_gif(120, 48)

    opf = b"""<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid2" xml:lang="zh-CN">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid2">urn:moyue:fine-modern</dc:identifier>
    <dc:title>\xe7\x8e\xb0\xe4\xbb\xa3EPUB3\xe7\xb2\xbe\xe6\x8e\x92\xe6\xa0\xb7\xe4\xb9\xa6</dc:title>
    <dc:language>zh-CN</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="svg-page" href="Text/svg.xhtml" media-type="application/xhtml+xml"/>
    <item id="webp-page" href="Text/webp.xhtml" media-type="application/xhtml+xml"/>
    <item id="gif-page" href="Text/gif.xhtml" media-type="application/xhtml+xml"/>
    <item id="math-page" href="Text/math.xhtml" media-type="application/xhtml+xml"/>
    <item id="img-webp" href="Images/photo.webp" media-type="image/webp"/>
    <item id="img-gif" href="Images/badge.gif" media-type="image/gif"/>
  </manifest>
  <spine>
    <itemref idref="svg-page"/>
    <itemref idref="webp-page"/>
    <itemref idref="gif-page"/>
    <itemref idref="math-page"/>
  </spine>
</package>
"""
    nav = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="zh-CN">
<head><title>目录</title></head>
<body>
<nav epub:type="toc">
  <ol>
    <li><a href="Text/svg.xhtml">SVG 矢量装饰</a></li>
    <li><a href="Text/webp.xhtml">WebP 插图</a></li>
    <li><a href="Text/gif.xhtml">GIF 徽章</a></li>
    <li><a href="Text/math.xhtml">MathML 公式与 Ruby 注音</a></li>
  </ol>
</nav>
</body>
</html>
""".encode("utf-8")

    svg_xhtml = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh-CN">
<head><title>SVG</title></head>
<body>
  <div style="text-align:center; text-indent:0;">
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 120" width="400" height="120">
      <rect x="8" y="8" width="384" height="104" fill="#222222"/>
      <circle cx="80" cy="60" r="32" fill="#FFFFFF"/>
      <text x="140" y="72" font-size="36" fill="#FFFFFF">SVG 精排</text>
    </svg>
  </div>
</body>
</html>
""".encode("utf-8")

    webp_xhtml = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh-CN">
<head><title>WebP</title></head>
<body>
  <figure>
    <img src="../Images/photo.webp" alt="WebP"/>
    <figcaption>WebP 高压缩插图测试</figcaption>
  </figure>
</body>
</html>
""".encode("utf-8")

    gif_xhtml = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh-CN">
<head><title>GIF</title></head>
<body>
  <p style="text-align:center; text-indent:0;"><img src="../Images/badge.gif" alt="GIF"/></p>
</body>
</html>
""".encode("utf-8")

    math_xhtml = """<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh-CN">
<head><title>MathML</title></head>
<body>
  <p><ruby>墨<rt>mò</rt>阅<rt>yuè</rt></ruby>支持现代公式与 Flex 降级卡片：</p>
  <span style="display: flex; border-left: 4px solid #333333; padding: 8pt; margin: 10pt 0; text-indent: 0;">Flex 卡片容器在 crengine 中优雅降级为块级盒。</span>
  <div style="text-align:center; text-indent:0; margin-top: 1em;">
    <math xmlns="http://www.w3.org/1998/Math/MathML" display="block">
      <mi mathvariant="normal">E</mi>
      <mo>=</mo>
      <mfrac>
        <mrow><mi mathvariant="normal">m</mi><msup><mi mathvariant="normal">c</mi><mn>2</mn></msup></mrow>
        <msqrt><mrow><mn>1</mn><mo>-</mo><msup><mi mathvariant="normal">v</mi><mn>2</mn></msup></mrow></msqrt>
      </mfrac>
    </math>
  </div>
</body>
</html>
""".encode("utf-8")

    write_epub(
        OUT_DIR / "fine_modern.epub",
        {
            "META-INF/container.xml": CONTAINER_XML,
            "OEBPS/content.opf": opf,
            "OEBPS/nav.xhtml": nav,
            "OEBPS/Text/svg.xhtml": svg_xhtml,
            "OEBPS/Text/webp.xhtml": webp_xhtml,
            "OEBPS/Text/gif.xhtml": gif_xhtml,
            "OEBPS/Text/math.xhtml": math_xhtml,
            "OEBPS/Images/photo.webp": webp_bytes,
            "OEBPS/Images/badge.gif": gif_bytes,
        },
    )


if __name__ == "__main__":
    build_duokan_epub()
    build_modern_epub()
    print("generated fine_duokan.epub and fine_modern.epub")
