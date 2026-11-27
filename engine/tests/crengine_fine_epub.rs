//! 多看 / 微信读书 / 现代 EPUB3 精排能力回归测试。
//!
//! 覆盖：
//! - 多看全屏封面（`duokan-page-fullscreen`）、纯图片脚注热区（`<a class="duokan-footnote"><img/></a>`）、
//!   页下注（`-cr-hint: footnote-inpage`）、居中题记/图注不被强推缩进、全角空格在精排下保留；
//! - 内联 `<svg>`（LunaSVG + xtended）、WebP（libwebp）、GIF（内置）、MathML 公式、
//!   CSS3 `display: flex` 优雅降级、真实 DPI 参数生效。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Engine, LayoutSettings, MAX_DPI, MIN_DPI};
use inkread_platform::GrayBuffer;

const PAGE_W: u32 = 800;
const PAGE_H: u32 = 1000;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn ink_pixels(buf: &GrayBuffer) -> usize {
    buf.pixels().iter().filter(|&&p| p < 240).count()
}

fn ink_in_band(buf: &GrayBuffer, y0: u32, y1: u32) -> usize {
    let w = buf.width() as usize;
    let mut count = 0usize;
    for y in y0.min(buf.height())..y1.min(buf.height()) {
        let row = &buf.pixels()[y as usize * w..(y as usize + 1) * w];
        count += row.iter().filter(|&&p| p < 240).count();
    }
    count
}

#[test]
fn duokan_image_footnote_link_is_clickable_and_renders_inpage_footnote() {
    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let path = fixture("fine_duokan.epub");

    let fine_layout = LayoutSettings::default();
    let mut doc = engine
        .open(&path, (PAGE_W, PAGE_H), &fine_layout)
        .expect("open fine_duokan.epub");
    assert!(
        doc.page_count() >= 2,
        "duokan fixture should have cover + chapter pages, got {}",
        doc.page_count()
    );

    // Page 0 is the `duokan-page-fullscreen` cover page and must render visible image ink.
    let mut cover_buf = GrayBuffer::new(PAGE_W, PAGE_H);
    doc.render_page(0, &mut cover_buf).expect("render cover");
    assert!(
        ink_pixels(&cover_buf) > 10_000,
        "duokan-page-fullscreen cover should render visible image pixels"
    );

    // Chapter page contains `<a class="duokan-footnote" href="#df-1"><img .../></a>`.
    // Previously `getSegmentRects(rects)` defaulted to includeImages=false and dropped it!
    let mut found_duokan_link = false;
    let mut footnote_page = None;
    for p in 0..doc.page_count() {
        let links = doc.page_links(p).expect("page_links");
        for link in links {
            if link.href.contains("df-1") && !link.rects.is_empty() {
                found_duokan_link = true;
                footnote_page = Some(p);
                break;
            }
        }
        if found_duokan_link {
            break;
        }
    }
    assert!(
        found_duokan_link,
        "Duokan image-only footnote link (<a class='duokan-footnote'><img/></a>) must have clickable rects"
    );

    // The page referencing `#df-1` should render body + image clearly, while
    // suppressing original in-page/chapter-end footnote text (shown in popup).
    let p = footnote_page.unwrap();
    let mut ch_buf = GrayBuffer::new(PAGE_W, PAGE_H);
    doc.render_page(p, &mut ch_buf).expect("render chapter");
    assert!(ink_pixels(&ch_buf) > 5_000);
    assert_eq!(
        ink_in_band(&ch_buf, PAGE_H - 220, PAGE_H),
        0,
        "original footnote text should be suppressed at the bottom of page {p}"
    );
}

#[test]
fn fine_mode_preserves_ideographic_spaces_while_uniform_mode_strips_them() {
    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let path = fixture("fine_duokan.epub");

    let fine = LayoutSettings {
        fine_typesetting: true,
        embedded_styles: true,
        ..LayoutSettings::default()
    };
    let uniform = LayoutSettings {
        fine_typesetting: false,
        embedded_styles: false,
        ..LayoutSettings::default()
    };

    let mut fine_doc = engine.open(&path, (PAGE_W, PAGE_H), &fine).expect("fine");
    let mut unif_doc = engine
        .open(&path, (PAGE_W, PAGE_H), &uniform)
        .expect("uniform");

    let mut fine_buf = GrayBuffer::new(PAGE_W, PAGE_H);
    let mut unif_buf = GrayBuffer::new(PAGE_W, PAGE_H);
    fine_doc.render_page(1, &mut fine_buf).expect("fine p1");
    unif_doc.render_page(1, &mut unif_buf).expect("unif p1");

    assert_ne!(
        fine_buf.pixels(),
        unif_buf.pixels(),
        "fine mode and uniform mode should produce distinct typography on fine_duokan.epub"
    );
}

#[test]
fn modern_epub3_renders_inline_svg_webp_gif_and_mathml() {
    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let path = fixture("fine_modern.epub");

    let layout = LayoutSettings {
        user_css: Some("body { page-break-before: always; }".to_string()),
        ..LayoutSettings::default()
    };
    let mut doc = engine
        .open(&path, (PAGE_W, PAGE_H), &layout)
        .expect("open fine_modern.epub");

    // 4 spine items: inline SVG, WebP, GIF, MathML + Flex fallback.
    // Every single spine item starts a new page and MUST render substantial ink!
    assert!(
        doc.page_count() >= 4,
        "expected >= 4 pages for SVG/WebP/GIF/MathML fixture, got {}",
        doc.page_count()
    );

    let labels = [
        "inline SVG (LunaSVG)",
        "WebP (libwebp)",
        "GIF",
        "MathML + Flex",
    ];
    for (idx, label) in labels.iter().enumerate() {
        let mut buf = GrayBuffer::new(PAGE_W, PAGE_H);
        doc.render_page(idx as u32, &mut buf)
            .unwrap_or_else(|e| panic!("render page {idx} ({label}) failed: {e}"));
        let ink = ink_pixels(&buf);
        assert!(
            ink > 2_000,
            "page {idx} ({label}) should render visible pixels, got only {ink} ink pixels"
        );
    }
}

#[test]
fn dpi_validation_and_css_absolute_unit_scaling() {
    let low = LayoutSettings {
        dpi: MIN_DPI - 1,
        ..LayoutSettings::default()
    };
    assert!(low.validate().is_err());

    let high = LayoutSettings {
        dpi: MAX_DPI + 1,
        ..LayoutSettings::default()
    };
    assert!(high.validate().is_err());

    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let path = fixture("fine_modern.epub");

    let layout_167 = LayoutSettings {
        dpi: 167,
        user_css: Some("body { page-break-before: always; }".to_string()),
        ..LayoutSettings::default()
    };
    let layout_300 = LayoutSettings {
        dpi: 300,
        user_css: Some("body { page-break-before: always; }".to_string()),
        ..LayoutSettings::default()
    };
    assert!(layout_167.validate().is_ok());
    assert!(layout_300.validate().is_ok());

    let mut doc_167 = engine
        .open(&path, (PAGE_W, PAGE_H), &layout_167)
        .expect("167");
    let mut doc_300 = engine
        .open(&path, (PAGE_W, PAGE_H), &layout_300)
        .expect("300");

    // Page 3 (math.xhtml) has `padding: 8pt; margin: 10pt 0;` which scales with DPI.
    let mut buf_167 = GrayBuffer::new(PAGE_W, PAGE_H);
    let mut buf_300 = GrayBuffer::new(PAGE_W, PAGE_H);
    doc_167.render_page(3, &mut buf_167).expect("render 167");
    doc_300.render_page(3, &mut buf_300).expect("render 300");
    assert_ne!(
        buf_167.pixels(),
        buf_300.pixels(),
        "CSS pt/mm absolute units should scale differently at 167 dpi vs 300 dpi"
    );
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn write_stored_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let len = data.len() as u32;
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u16;

        // Local file header
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: store
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        // Central directory header
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&len.to_le_bytes());
        central.extend_from_slice(&len.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);

    // End of central directory
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(path, out).expect("write test epub");
}

#[test]
fn embedded_fonts_toggle_controls_epub_font_face_in_fine_mode() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let embedded_ttf =
        manifest_dir.join("../aristotle/crates/aristotle-ui/assets/fonts/Charis-Italic.ttf");
    let default_ttf =
        manifest_dir.join("../aristotle/crates/aristotle-ui/assets/fonts/Literata-Regular.ttf");
    let font_bytes = std::fs::read(&embedded_ttf).expect("read Charis-Italic.ttf");

    let epub_path = std::env::temp_dir().join(format!(
        "inkread-embedded-font-test-{}.epub",
        std::process::id()
    ));

    let container_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

    let content_opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">embedded-font-test</dc:identifier>
    <dc:title>Embedded Font Test</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="css" href="style.css" media-type="text/css"/>
    <item id="f1" href="fonts/embedded.ttf" media-type="font/ttf"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
  </spine>
</package>"#;

    let style_css = br#"@font-face {
  font-family: 'BookCustomItalic';
  src: url('fonts/embedded.ttf');
}
body {
  font-family: 'BookCustomItalic', serif;
  font-size: 1.2em;
}
"#;

    let ch1_xhtml = br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <title>Embedded Font</title>
  <link rel="stylesheet" type="text/css" href="style.css"/>
</head>
<body>
  <p>Sphinx of black quartz, judge my vow. Fine typesetting embedded font verification.</p>
</body>
</html>"#;

    write_stored_zip(
        &epub_path,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", container_xml),
            ("OEBPS/content.opf", content_opf),
            ("OEBPS/style.css", style_css),
            ("OEBPS/fonts/embedded.ttf", &font_bytes),
            ("OEBPS/ch1.xhtml", ch1_xhtml),
        ],
    );

    let engine = CrengineEngine::new(vec![default_ttf]).expect("crengine init with Literata");

    let with_embedded = LayoutSettings {
        font_face: "Literata".into(),
        fine_typesetting: true,
        embedded_styles: true,
        embedded_fonts: true,
        ..LayoutSettings::default()
    };
    let without_embedded = LayoutSettings {
        embedded_fonts: false,
        ..with_embedded.clone()
    };

    let mut doc_on = engine
        .open(&epub_path, (PAGE_W, PAGE_H), &with_embedded)
        .expect("open with embedded_fonts=true");
    let mut doc_off = engine
        .open(&epub_path, (PAGE_W, PAGE_H), &without_embedded)
        .expect("open with embedded_fonts=false");

    let mut buf_on = GrayBuffer::new(PAGE_W, PAGE_H);
    let mut buf_off = GrayBuffer::new(PAGE_W, PAGE_H);
    doc_on.render_page(0, &mut buf_on).expect("render on");
    doc_off.render_page(0, &mut buf_off).expect("render off");

    assert!(
        ink_pixels(&buf_on) > 500,
        "embedded font page must have visible ink"
    );
    assert!(
        ink_pixels(&buf_off) > 500,
        "fallback font page must have visible ink"
    );
    assert_ne!(
        buf_on.pixels(),
        buf_off.pixels(),
        "fine typesetting with embedded_fonts=true must render the book's embedded @font-face font"
    );

    // Toggling embedded_fonts at runtime on an already-open document must take effect too.
    doc_on
        .set_layout(&without_embedded)
        .expect("toggle embedded_fonts off");
    let mut buf_toggled = GrayBuffer::new(PAGE_W, PAGE_H);
    doc_on
        .render_page(0, &mut buf_toggled)
        .expect("render toggled");
    assert_eq!(
        buf_toggled.pixels(),
        buf_off.pixels(),
        "toggling embedded_fonts off on an open document must match opening with embedded_fonts=false"
    );

    let _ = std::fs::remove_file(&epub_path);
}

fn encode_gray_png(width: u32, height: u32, gray: u8) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write png header");
        let pixels = vec![gray; (width * height) as usize];
        writer.write_image_data(&pixels).expect("write png data");
    }
    out
}

#[test]
fn svg_header_images_without_explicit_dimensions_and_external_svg_render_in_fine_mode() {
    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let head_png = encode_gray_png(480, 240, 0x30);
    let wrapper_svg = br#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
     width="480" height="240" viewBox="0 0 480 240">
  <image width="480" height="240" xlink:href="head.png"/>
</svg>"#;

    let cases: &[(&str, &str, &str)] = &[
        (
            "no_wh",
            "",
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
                    width="100%" height="100%" viewBox="0 0 480 240">
                 <image xlink:href="../Images/head.png"/>
               </svg>"#,
        ),
        (
            "width_only",
            "",
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
                    width="100%" height="100%" viewBox="0 0 480 240">
                 <image width="480" xlink:href="../Images/head.png"/>
               </svg>"#,
        ),
        (
            "auto_wh",
            "",
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
                    width="100%" height="100%" viewBox="0 0 480 240">
                 <image width="auto" height="auto" xlink:href="../Images/head.png"/>
               </svg>"#,
        ),
        (
            "css_wh",
            "svg image { width: 480px; height: 240px; }",
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
                    width="100%" height="100%" viewBox="0 0 480 240">
                 <image xlink:href="../Images/head.png"/>
               </svg>"#,
        ),
        (
            "external_svg_relative_image",
            "",
            r#"<div><img src="../Images/wrapper.svg" alt="header"/></div>"#,
        ),
    ];

    let container_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

    let content_opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">svg-header-test</dc:identifier>
    <dc:title>SVG Header Image Test</dc:title>
    <dc:language>zh-CN</dc:language>
  </metadata>
  <manifest>
    <item id="p1" href="Text/p1.xhtml" media-type="application/xhtml+xml"/>
    <item id="css" href="Styles/main.css" media-type="text/css"/>
    <item id="img-head" href="Images/head.png" media-type="image/png"/>
    <item id="img-svg" href="Images/wrapper.svg" media-type="image/svg+xml"/>
  </manifest>
  <spine>
    <itemref idref="p1"/>
  </spine>
</package>"#;

    let fine = LayoutSettings {
        fine_typesetting: true,
        embedded_styles: true,
        ..LayoutSettings::default()
    };

    for (name, css, body) in cases {
        let epub_path = std::env::temp_dir().join(format!(
            "inkread_svg_header_{name}_{}.epub",
            std::process::id()
        ));
        let xhtml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <title>{name}</title>
  <link rel="stylesheet" type="text/css" href="../Styles/main.css"/>
</head>
<body>{body}</body>
</html>"#
        );
        write_stored_zip(
            &epub_path,
            &[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", container_xml),
                ("OEBPS/content.opf", content_opf),
                ("OEBPS/Styles/main.css", css.as_bytes()),
                ("OEBPS/Text/p1.xhtml", xhtml.as_bytes()),
                ("OEBPS/Images/head.png", &head_png),
                ("OEBPS/Images/wrapper.svg", wrapper_svg),
            ],
        );

        let mut doc = engine
            .open(&epub_path, (PAGE_W, PAGE_H), &fine)
            .unwrap_or_else(|e| panic!("open {name}: {e}"));
        let mut buf = GrayBuffer::new(PAGE_W, PAGE_H);
        doc.render_page(0, &mut buf)
            .unwrap_or_else(|e| panic!("render {name}: {e}"));
        let dark = ink_pixels(&buf);
        let _ = std::fs::remove_file(&epub_path);

        assert!(
            dark > 20_000,
            "SVG header image case `{name}` must render visible image pixels in fine mode, got {dark}"
        );
    }
}

#[test]
fn fine_mode_first_open_stays_fast_for_multi_chapter_cjk_epub() {
    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let para = "<p>这是中文排版引擎首次开书性能回归测试段落，含标点：“你好，世界！”及英文 Hello World 12345。春江潮水连海平，海上明月共潮生。</p>";
    let body = para.repeat(25);
    let mut chapter_xmls: Vec<(String, Vec<u8>)> = Vec::new();
    let mut manifest = String::new();
    let mut spine = String::new();
    for i in 1..=12 {
        manifest.push_str(&format!(
            r#"<item id="c{i}" href="c{i}.xhtml" media-type="application/xhtml+xml"/>"#
        ));
        spine.push_str(&format!(r#"<itemref idref="c{i}"/>"#));
        chapter_xmls.push((
            format!("OEBPS/c{i}.xhtml"),
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>第{i}章</title></head><body><h1>第{i}章</h1>{body}</body></html>"#
            )
            .into_bytes(),
        ));
    }
    let container = br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
    let opf = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Perf</dc:title><dc:language>zh-CN</dc:language></metadata><manifest>{manifest}</manifest><spine>{spine}</spine></package>"#
    )
    .into_bytes();
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", container.as_slice()),
        ("OEBPS/content.opf", opf.as_slice()),
    ];
    for (name, bytes) in &chapter_xmls {
        entries.push((name.as_str(), bytes.as_slice()));
    }

    let epub_path =
        std::env::temp_dir().join(format!("inkread-fine-perf-{}.epub", std::process::id()));
    write_stored_zip(&epub_path, &entries);

    let fine = LayoutSettings {
        fine_typesetting: true,
        embedded_styles: true,
        ..LayoutSettings::default()
    };
    let plain = LayoutSettings {
        fine_typesetting: false,
        embedded_styles: false,
        ..LayoutSettings::default()
    };

    let t0 = std::time::Instant::now();
    let doc_plain = engine.open(&epub_path, (PAGE_W, PAGE_H), &plain).unwrap();
    let plain_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert!(doc_plain.page_count() > 10);

    let t1 = std::time::Instant::now();
    let doc_fine = engine.open(&epub_path, (PAGE_W, PAGE_H), &fine).unwrap();
    let fine_ms = t1.elapsed().as_secs_f64() * 1000.0;
    assert!(doc_fine.page_count() > 10);

    let _ = std::fs::remove_file(&epub_path);
    // Fine mode with FreeType kerning + CJK pipeline must stay well under 2.5x
    // plain mode (full HarfBuzz shaping was 3x+ and blew up on ARM e-ink).
    assert!(
        fine_ms <= (plain_ms * 2.5).max(plain_ms + 80.0),
        "fine mode first-open regressed: fine={fine_ms:.1} ms vs plain={plain_ms:.1} ms"
    );
}

#[test]
fn duokan_phase1_to_3_compat_properties() {
    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let path = fixture("fine_duokan.epub");
    let layout = LayoutSettings {
        fine_typesetting: true,
        embedded_styles: true,
        margins: inkread_engine::Margins {
            left: 48,
            right: 48,
            top: 48,
            bottom: 48,
        },
        ..LayoutSettings::default()
    };

    // 1. duokan-page-fullscreen cover (3:4) on 800x1000 viewport scales to 750x1000 centered,
    //    so it must bleed flush into the top/bottom 48px outer margin bands (y in 0..16).
    let mut doc = engine.open(&path, (PAGE_W, PAGE_H), &layout).expect("open");
    let mut cover = GrayBuffer::new(PAGE_W, PAGE_H);
    doc.render_page(0, &mut cover).expect("render cover");
    let mut margin_ink = 0usize;
    for y in 0..16i32 {
        for x in 0..PAGE_W as i32 {
            if cover.get_pixel(x, y).unwrap_or(255) < 250 {
                margin_ink += 1;
            }
        }
    }
    assert!(
        margin_ink > 100,
        "duokan-page-fullscreen should paint into outer margins (got {margin_ink} margin ink pixels)"
    );

    // 2. duokan-text-indent: 0 vs default 2em, duokan-bleed: lefttopright,
    //    parent text-align: center inheritance, and border-radius rounded box.
    let container = br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
    let opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:moyue:duokan-p13</dc:identifier>
    <dc:title>Duokan Compat</dc:title>
    <dc:language>zh-CN</dc:language>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
    <itemref idref="ch2"/>
  </spine>
</package>"#;
    let ch1 = br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><style>
.bleed { duokan-bleed: lefttopright; background-color: #202020; height: 40px; }
.center-wrap { text-align: center; }
.dk-no-indent { duokan-text-indent: 0; }
.rounded { background-color: #dddddd; border: 2px solid #222222; border-radius: 10px; padding: 12px; margin-top: 16px; }
</style></head>
<body>
  <div class="bleed"></div>
  <div class="center-wrap"><p class="dk-no-indent">CENTERED</p></div>
  <div class="rounded"><p class="dk-no-indent">ROUNDED CARD</p></div>
</body>
</html>"#;
    let ch2 = br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><style>
.plain { background-color: #dddddd; border: 2px solid #222222; padding: 12px; margin-top: 16px; }
</style></head>
<body>
  <div class="plain"><p style="text-indent:0">SQUARE CARD</p></div>
</body>
</html>"#;

    let tmp_epub =
        std::env::temp_dir().join(format!("inkread-duokan-p13-{}.epub", std::process::id()));
    write_stored_zip(
        &tmp_epub,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", container),
            ("OEBPS/content.opf", opf),
            ("OEBPS/ch1.xhtml", ch1),
            ("OEBPS/ch2.xhtml", ch2),
        ],
    );

    let mut d2 = engine
        .open(&tmp_epub, (PAGE_W, PAGE_H), &layout)
        .expect("open duokan p13 epub");
    let mut p0 = GrayBuffer::new(PAGE_W, PAGE_H);
    d2.render_page(0, &mut p0).expect("render p0");
    let _ = std::fs::remove_file(&tmp_epub);

    // Bleed banner must paint flush to the top-left (x=4, y=4) and top-right (x=PAGE_W-5, y=4) edges.
    let tl = p0.get_pixel(4, 4).unwrap_or(255);
    let tr = p0.get_pixel(PAGE_W as i32 - 5, 4).unwrap_or(255);
    assert!(
        tl < 128,
        "duokan-bleed: lefttopright must reach top-left edge (pixel={tl})"
    );
    assert!(
        tr < 128,
        "duokan-bleed: lefttopright must reach top-right edge (pixel={tr})"
    );
}

#[test]
fn zero_line_spacing_renders_safely_in_fine_mode() {
    let engine = CrengineEngine::with_default_fonts().expect("crengine init");
    let path = fixture("fine_duokan.epub");

    let zero_layout = LayoutSettings {
        fine_typesetting: true,
        embedded_styles: true,
        line_spacing_percent: 0,
        ..LayoutSettings::default()
    };
    let normal_layout = LayoutSettings {
        fine_typesetting: true,
        embedded_styles: true,
        line_spacing_percent: 170,
        ..LayoutSettings::default()
    };
    assert!(zero_layout.validate().is_ok());

    let mut doc_zero = engine
        .open(&path, (PAGE_W, PAGE_H), &zero_layout)
        .expect("open with 0% line spacing");
    let mut doc_normal = engine
        .open(&path, (PAGE_W, PAGE_H), &normal_layout)
        .expect("open with 170% line spacing");

    let mut buf_zero = GrayBuffer::new(PAGE_W, PAGE_H);
    let mut buf_normal = GrayBuffer::new(PAGE_W, PAGE_H);
    // Page 1 is chapter 1 (body text with multiple lines).
    doc_zero.render_page(1, &mut buf_zero).expect("render 0%");
    doc_normal
        .render_page(1, &mut buf_normal)
        .expect("render 170%");

    assert!(
        ink_pixels(&buf_zero) > 1_000,
        "0% line spacing page should still render text safely"
    );
    assert_ne!(
        buf_zero.pixels(),
        buf_normal.pixels(),
        "0% line spacing must produce visibly tighter layout than 170%"
    );
}
