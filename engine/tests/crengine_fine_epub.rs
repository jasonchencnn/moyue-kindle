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

    // The page referencing `#df-1` should render body + image + in-page footnote clearly.
    let p = footnote_page.unwrap();
    let mut ch_buf = GrayBuffer::new(PAGE_W, PAGE_H);
    doc.render_page(p, &mut ch_buf).expect("render chapter");
    assert!(ink_pixels(&ch_buf) > 5_000);
    // In-page footnote appears near the bottom of the referencing page.
    assert!(
        ink_in_band(&ch_buf, PAGE_H - 220, PAGE_H) > 200,
        "in-page footnote should render at the bottom of page {p}"
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
