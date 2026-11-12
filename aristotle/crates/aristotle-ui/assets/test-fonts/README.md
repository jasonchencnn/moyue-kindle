# The test fonts

`CjkFixture-Regular.ttf` is a fixture, not a shipped font.

It is Noto Sans SC cut to the characters of the fallback unit test
(`crates/aristotle-ui/src/text/engine.rs`): 紅樓夢 墨阅排版样张， the Latin
letters of the same string, and the space. The test needs a Chinese font of
its own to shape with, and the font the app ships is downloaded with a
checksum and stays out of the tree (see `tools/build-render.sh`), so the
test cannot use it. A fixture cut to sixteen characters is nine kilobytes
and keeps the test running without a network.

Regenerate it after a change to the test string:

    fontTools subset NotoSansSC-Regular.otf \
        --output-file=CjkFixture-Regular.ttf \
        --unicodes=U+7D05,U+6A13,U+5922,U+58A8,U+9605,U+6392,U+7248,U+6837,U+5F20,U+FF0C,U+0020,U+0044,U+0072,U+0065,U+0061,U+006D \
        --layout-features='*' --name-IDs='*' --name-languages='*' --notdef-outline

Noto Sans SC is under the SIL Open Font License 1.1; the licence text is
`Noto-OFL.txt` beside it.
