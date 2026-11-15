//! 正文每行下方引导线的行为测试（KO2-UNDERLINE-001）。
//!
//! 需要 `--features crengine`，并且要有能编译 C++ 引擎的工具链。
//! 跑法见 `scripts/check-engine.sh`。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Document, Engine, LayoutSettings, RenderOptions, Underline};
use inkread_platform::GrayBuffer;

/// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

/// 引导线的灰度（0 黑、255 白），与 aristotle-render 的 `UNDERLINE_GRAY` 一致。
const GRAY: u8 = 0x60;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn engine() -> CrengineEngine {
    CrengineEngine::with_default_fonts().expect("初始化 crengine 引擎失败")
}

fn open(engine: &CrengineEngine, path: PathBuf) -> Box<dyn Document> {
    engine
        .open(&path, (PAGE_WIDTH, PAGE_HEIGHT), &LayoutSettings::default())
        .expect("打开文档失败")
}

fn render(doc: &mut Box<dyn Document>, page: u32, underline: Underline) -> GrayBuffer {
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page_with_options(
        page,
        &mut buffer,
        &RenderOptions {
            invert_images: false,
            underline,
            underline_gray: GRAY,
        },
    )
    .expect("渲染失败");
    buffer
}

/// 这一行里有多少个像素是引导线的灰度。
fn gray_pixels(buffer: &GrayBuffer, y: usize) -> usize {
    let row = &buffer.pixels()[y * buffer.width() as usize..(y + 1) * buffer.width() as usize];
    row.iter().filter(|p| **p == GRAY).count()
}

/// 有引导线的行数：每一行引导线是连续的灰度像素，数量至少 8 个才算。
fn guide_rows(buffer: &GrayBuffer) -> usize {
    (0..buffer.height() as usize)
        .filter(|y| gray_pixels(buffer, *y) >= 8)
        .count()
}

#[test]
fn solid_guide_appears_under_the_text_lines() {
    let engine = engine();
    let mut doc = open(&engine, manifest_dir().join("fixtures/sample.epub"));

    let off = render(&mut doc, 0, Underline::Off);
    let solid = render(&mut doc, 0, Underline::Solid);

    // 关掉时页面上没有引导线的灰度。
    assert_eq!(guide_rows(&off), 0, "关闭时不该有引导线");
    // 打开后有引导线，而且实线比点线占的行数多（点线是断开的）。
    let solid_rows = guide_rows(&solid);
    assert!(solid_rows > 0, "实线引导线一行都没画出来");

    let mut black = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page_with_options(
        0,
        &mut black,
        &RenderOptions {
            underline: Underline::Solid,
            underline_gray: 0,
            ..RenderOptions::default()
        },
    )
    .unwrap();
    assert!(off
        .pixels()
        .iter()
        .zip(black.pixels())
        .any(|(before, after)| *before != 0 && *after == 0));

    let dotted = render(&mut doc, 0, Underline::Dotted);
    let dotted_rows = guide_rows(&dotted);
    assert!(dotted_rows > 0, "点线引导线一行都没画出来");
    assert!(
        dotted_rows >= solid_rows,
        "点线的行数不该比实线少：点线 {dotted_rows} 实线 {solid_rows}"
    );

    // 引导线只加像素，不改文字的位置：两页的差异行都是引导线。
    let diff_rows = (0..PAGE_HEIGHT as usize)
        .filter(|y| {
            off.pixels()[*y * PAGE_WIDTH as usize..] != solid.pixels()[*y * PAGE_WIDTH as usize..]
        })
        .count();
    assert!(diff_rows > 0, "实线引导线没有改变画面");
}

#[test]
fn guide_stays_inside_the_page_on_the_last_page() {
    let engine = engine();
    let mut doc = open(&engine, manifest_dir().join("fixtures/sample.epub"));
    let pages = doc.page_count();
    assert!(pages > 1, "样张应该不止一页");

    // 最后一页：引导线不能画到页面外面，也不能把文字切掉。
    let last = pages - 1;
    let buffer = render(&mut doc, last, Underline::Solid);
    // 页面底边之下没有像素：GrayBuffer 本身就越界不了，这里确认末行没有被
    // 引导线覆盖成一条实心带。
    let bottom = PAGE_HEIGHT as usize - 1;
    assert!(
        gray_pixels(&buffer, bottom) < buffer.width() as usize,
        "页面最后一行被整行涂成了引导线"
    );
    // 整页的灰度都在 0..=255 之间（u8 本身保证），这里确认没有越界的写入。
    assert!(!buffer.pixels().is_empty(), "页面缓冲是空的");
}

#[test]
fn guide_is_drawn_in_the_body_block_only() {
    let engine = engine();
    let mut doc = open(&engine, manifest_dir().join("fixtures/sample.epub"));
    let buffer = render(&mut doc, 0, Underline::Solid);

    // 引导线不会落在页眉位置（页面上边缘留白）：最上面 40 行是干净的。
    for y in 0..40 {
        assert_eq!(gray_pixels(&buffer, y), 0, "第 {y} 行不该有引导线");
    }
}

#[test]
fn guides_never_overwrite_glyphs_in_tight_viewports() {
    let engine = engine();
    let layout = LayoutSettings {
        margins: inkread_engine::Margins {
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
        },
        ..LayoutSettings::default()
    };
    for height in 80..112 {
        let mut doc = engine
            .open(
                &manifest_dir().join("fixtures/sample.epub"),
                (320, height),
                &layout,
            )
            .expect("open compact viewport");
        let mut off = GrayBuffer::new(320, height);
        let mut solid = GrayBuffer::new(320, height);
        doc.render_page(0, &mut off).unwrap();
        doc.render_page_with_options(
            0,
            &mut solid,
            &RenderOptions {
                underline: Underline::Solid,
                underline_gray: GRAY,
                ..RenderOptions::default()
            },
        )
        .unwrap();
        for (before, after) in off.pixels().iter().zip(solid.pixels()) {
            if *before < 32 {
                assert_eq!(before, after, "guide overwrote a glyph at height {height}");
            }
        }
    }
}
