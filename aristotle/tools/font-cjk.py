#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the fallback font: Chinese (Noto Sans SC) and symbols (DejaVu Sans).

usage: font-cjk.py OUT.ttf CJK.otf SYMBOLS.ttf

Plato's engine falls back to one font for every script the body font does
not cover (Aristotle's fork of Plato, "core: mupdf tofu fallback"): the file
data/fonts/DejaVuSans-Symbols.ttf, whatever it holds. Chinese needs a font
of its own, so this script puts Noto Sans SC's ideographs into that file,
next to DejaVu Sans's symbols, which are already there.

Noto Sans SC ships as an OpenType font with CFF outlines, DejaVu Sans as a
TrueType font with quadratic ones. fontTools merges fonts of one kind only,
so the CFF font is converted to quadratic outlines first (cu2qu) and the
symbol font is scaled to the same units per em. The Chinese font comes
first, so the merged font carries its name records: those are what the
licence notices (tools/bundle-licenses.py) quote.
"""

import sys
import tempfile
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.merge import Merger
from fontTools.pens.cu2quPen import Cu2QuPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.ttLib.scaleUpem import scale_upem

# The name records the licence notice quotes, and the fontTools names they
# are written under.
NAME_RECORDS = {
    "copyright": 0,
    "familyName": 1,
    "styleName": 2,
    "uniqueFontIdentifier": 3,
    "fullName": 4,
    "version": 5,
    "psName": 6,
    "licenseDescription": 13,
    "licenseInfoURL": 14,
}


def as_truetype(src_path: Path, out_path: Path) -> int:
    """The CFF font as a TrueType font: the same outlines, quadratic.

    Returns the units per em the font has.
    """
    src = TTFont(str(src_path))
    upem = src["head"].unitsPerEm
    glyph_set = src.getGlyphSet()
    order = src.getGlyphOrder()
    builder = FontBuilder(upem, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap(src.getBestCmap())
    glyphs = {}
    for name in order:
        pen = TTGlyphPen(None)
        glyph_set[name].draw(Cu2QuPen(pen, max_err=1.0, reverse_direction=True))
        glyphs[name] = pen.glyph()
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({n: src["hmtx"][n] for n in order})
    builder.setupHorizontalHeader(
        ascent=src["hhea"].ascent, descent=src["hhea"].descent, lineGap=src["hhea"].lineGap
    )
    names = src["name"]
    builder.setupNameTable(
        {
            key: names.getDebugName(record) or ""
            for key, record in NAME_RECORDS.items()
            if names.getDebugName(record)
        }
    )
    builder.setupOS2(
        sTypoAscender=src["OS/2"].sTypoAscender,
        sTypoDescender=src["OS/2"].sTypoDescender,
        usWinAscent=src["OS/2"].usWinAscent,
        usWinDescent=src["OS/2"].usWinDescent,
    )
    builder.setupPost()
    builder.save(str(out_path))
    return upem


def main() -> None:
    if len(sys.argv) != 4:
        sys.exit("usage: font-cjk.py OUT.ttf CJK.otf SYMBOLS.ttf")
    out, cjk_path, symbols_path = (Path(p) for p in sys.argv[1:4])
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        cjk_ttf = tmp / "cjk.ttf"
        upem = as_truetype(cjk_path, cjk_ttf)

        symbols = TTFont(str(symbols_path))
        # The symbol font's maths table has no counterpart in the Chinese
        # font and cannot be merged; the symbols themselves do not need it.
        if "MATH" in symbols:
            del symbols["MATH"]
        if symbols["head"].unitsPerEm != upem:
            scale_upem(symbols, upem)
        symbols_ttf = tmp / "symbols.ttf"
        symbols.save(str(symbols_ttf))

        merged = Merger().merge([str(cjk_ttf), str(symbols_ttf)])
        merged.save(str(out))


main()
