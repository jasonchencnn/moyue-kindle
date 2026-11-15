//! crengine 实现的行为测试。
//!
//! 需要 `--features crengine`，并且要有能编译 C++ 引擎的工具链。
//! 跑法见 `scripts/check-engine.sh`。

#![cfg(feature = "crengine")]

use std::path::{Path, PathBuf};

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Document, Engine, LayoutSettings};
use inkread_platform::GrayBuffer;

/// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

/// 黄金图是用 macOS 自带的宋体渲染的。
const GOLDEN_FONT_FACE: &str = "Songti SC";

/// 逐像素比对时允许的灰度差。FreeType 的 hinting 与抗锯齿在不同版本/平台上
/// 可能有细微差别，差 8 级（255 级里的 3%）以内当作没差别。
///
/// 放宽这个容差不会掩盖排版回归：页边距、字号、缩进一改，受影响的像素远
/// 超过 0.5%。
const PIXEL_TOLERANCE: u8 = 8;

/// 允许有差别的像素占比上限（百分比）。
const MAX_DIFF_PERCENT: f64 = 0.5;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path() -> PathBuf {
    manifest_dir().join("fixtures/sample.epub")
}

/// 混排测试书：有一段拉丁文、一段汉字，还有一段隐藏的文字。
///
/// 隐藏段落（`display:none`）不进排版，位置算不出来：拿它的 XPointer 可以
/// 验证「定位不到」会报错，而不是变成第 0 页。
fn mixed_fixture_path() -> PathBuf {
    manifest_dir().join("fixtures/mixed.epub")
}

fn golden_path() -> PathBuf {
    manifest_dir().join("tests/golden/engine_sample_p0.png")
}

fn engine() -> CrengineEngine {
    CrengineEngine::with_default_fonts().expect("初始化 crengine 引擎失败")
}

fn open_default(engine: &CrengineEngine) -> Box<dyn Document> {
    engine
        .open(
            &fixture_path(),
            (PAGE_WIDTH, PAGE_HEIGHT),
            &LayoutSettings::default(),
        )
        .expect("打开测试书失败")
}

#[test]
fn opens_the_fixture_and_has_at_least_two_pages() {
    let engine = engine();
    let doc = open_default(&engine);
    assert!(
        doc.page_count() >= 2,
        "测试书应当至少两页，实际 {} 页",
        doc.page_count()
    );
}

#[test]
fn reads_title_and_author_from_the_document() {
    let engine = engine();
    let doc = open_default(&engine);
    assert_eq!(doc.title(), "墨阅排版样张");
    assert_eq!(doc.author(), "墨阅项目组");
}

#[test]
fn xpointer_round_trips_back_to_the_same_page() {
    let engine = engine();
    let doc = open_default(&engine);
    let pages = doc.page_count();

    // 任务要求：page_xpointer(1) 之后 page_of_xpointer 要能回到 1。
    let xpointer = doc.page_xpointer(1).expect("取第 1 页 XPointer 失败");
    assert!(!xpointer.is_empty(), "XPointer 不应是空串");
    assert_eq!(
        doc.page_of_xpointer(&xpointer)
            .expect("按 XPointer 定位页码失败"),
        1
    );

    // 每一页都应当能往返。
    for page in 0..pages {
        let xpointer = doc.page_xpointer(page).expect("取 XPointer 失败");
        let found = doc
            .page_of_xpointer(&xpointer)
            .expect("按 XPointer 定位页码失败");
        assert_eq!(found, page, "第 {page} 页的 XPointer 定位到了第 {found} 页");
    }
}

#[test]
fn a_malformed_xpointer_is_an_error() {
    let engine = engine();
    let doc = open_default(&engine);
    let err = doc
        .page_of_xpointer("not-an-xpointer")
        .expect_err("格式不对的 XPointer 应当报错");
    assert!(
        matches!(err, inkread_engine::Error::XPointer(_)),
        "错误类型应当是 XPointer，实际：{err:?}"
    );
}

#[test]
fn an_xpointer_that_cannot_be_located_is_an_error() {
    let engine = engine();
    let doc = engine
        .open(
            &mixed_fixture_path(),
            (PAGE_WIDTH, PAGE_HEIGHT),
            &LayoutSettings::default(),
        )
        .expect("打开测试书失败");

    // 隐藏段落不进排版，位置算不出来。引擎内部把这种情况当成第 0 页，
    // 那样调用方会以为书翻到了开头：必须报出来。
    let err = doc
        .page_of_xpointer("/body/DocFragment[1]/body/p[3]/text().0")
        .expect_err("定位不到的 XPointer 应当报错");
    assert!(
        matches!(err, inkread_engine::Error::XPointer(_)),
        "错误类型应当是 XPointer，实际：{err:?}"
    );

    // 同一本书里能定位到的 XPointer 照常能用。
    assert_eq!(
        doc.page_of_xpointer("/body/DocFragment[1]/body/p[1]/text().0")
            .expect("能定位的 XPointer 应当成功"),
        0
    );
}

#[test]
fn rejects_an_out_of_range_page() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    let err = doc
        .render_page(doc.page_count(), &mut buffer)
        .expect_err("越界页码应当报错");
    assert!(
        err.to_string().contains("页码越界"),
        "错误信息应当说明页码越界，实际：{err}"
    );
}

#[test]
fn changing_the_font_size_changes_the_page_count() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let small = doc.page_count();

    let bigger = LayoutSettings {
        font_size_px: inkread_engine::MAX_FONT_SIZE_PX,
        ..LayoutSettings::default()
    };
    doc.set_layout(&bigger).expect("换字号重新排版失败");
    let large = doc.page_count();

    assert!(large > small, "字号调大后页数应该变多：{small} -> {large}");
}

#[test]
fn renders_page_zero_like_the_golden_image() {
    let engine = engine();

    // 黄金图依赖具体字体，字体不在就跳过比对，其余用例照跑。
    if !engine
        .font_faces()
        .iter()
        .any(|face| face.as_str() == GOLDEN_FONT_FACE)
    {
        eprintln!("跳过黄金图比对：没有 {GOLDEN_FONT_FACE} 字体");
        return;
    }

    let golden_path = golden_path();
    assert!(
        golden_path.exists(),
        "缺少黄金图 {}\n生成命令：\n  cargo run -p inkread-engine --features crengine --example render_page -- \\\n    engine/fixtures/sample.epub engine/tests/golden/engine_sample_p0.png --page 0",
        golden_path.display()
    );

    let mut doc = open_default(&engine);
    let mut rendered = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut rendered).expect("渲染第 0 页失败");

    let (width, height, golden) = load_golden(&golden_path);
    assert_eq!((width, height), (PAGE_WIDTH, PAGE_HEIGHT), "黄金图尺寸不对");
    assert_eq!(golden.len(), rendered.pixels().len(), "黄金图像素数不对");

    let mut diff = 0usize;
    let mut max_delta = 0u8;
    for (actual, expected) in rendered.pixels().iter().zip(golden.iter()) {
        let delta = actual.abs_diff(*expected);
        max_delta = max_delta.max(delta);
        if delta > PIXEL_TOLERANCE {
            diff += 1;
        }
    }

    let total = golden.len();
    let percent = diff as f64 * 100.0 / total as f64;
    assert!(
        percent <= MAX_DIFF_PERCENT,
        "第 0 页与黄金图差异过大：{diff}/{total} 像素（{percent:.3}%，上限 {MAX_DIFF_PERCENT}%），最大灰度差 {max_delta}"
    );
}

/// 读黄金图，返回 (宽, 高, 像素)。
fn load_golden(path: &Path) -> (u32, u32, Vec<u8>) {
    let file = std::fs::File::open(path).expect("打开黄金图失败");
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().expect("读黄金图信息失败");
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).expect("解码黄金图失败");
    assert_eq!(
        info.color_type,
        png::ColorType::Grayscale,
        "黄金图应当是灰度图"
    );
    assert_eq!(info.bit_depth, png::BitDepth::Eight, "黄金图应当是 8 位");
    (info.width, info.height, buffer)
}

#[test]
fn image_inversion_inverts_images_and_preserves_text() {
    let engine = engine();
    let interactive_epub = manifest_dir().join("fixtures/interactive.epub");
    let mut doc = engine
        .open(
            &interactive_epub,
            (PAGE_WIDTH, PAGE_HEIGHT),
            &LayoutSettings::default(),
        )
        .expect("打开 interactive.epub 失败");

    let mut buf_normal = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    let mut buf_inverted = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);

    doc.render_page_with_options(
        0,
        &mut buf_normal,
        &inkread_engine::RenderOptions {
            invert_images: false,
            underline: inkread_engine::Underline::Off,
            underline_gray: 0,
        },
    )
    .expect("渲染普通页面失败");

    doc.render_page_with_options(
        0,
        &mut buf_inverted,
        &inkread_engine::RenderOptions {
            invert_images: true,
            underline: inkread_engine::Underline::Off,
            underline_gray: 0,
        },
    )
    .expect("渲染图片反色页面失败");

    let mut inverted_pixels_count = 0;
    let mut identical_pixels_count = 0;

    let normal_px = buf_normal.pixels();
    let inverted_px = buf_inverted.pixels();
    assert_eq!(normal_px.len(), inverted_px.len());

    for (p_norm, p_inv) in normal_px.iter().zip(inverted_px.iter()) {
        if *p_norm == *p_inv {
            identical_pixels_count += 1;
        } else {
            // 反色区域：两个像素值相加接近 255
            let sum = *p_norm as u16 + *p_inv as u16;
            assert!(
                (sum as i32 - 255).abs() <= 8,
                "图片区域像素应当互为反色：norm={}, inv={}, sum={sum}",
                p_norm,
                p_inv
            );
            inverted_pixels_count += 1;
        }
    }

    assert!(
        inverted_pixels_count > 1000,
        "应当检测到图片反色区域（实际反色像素 {}）",
        inverted_pixels_count
    );
    assert!(
        identical_pixels_count > 1_000_000,
        "文字与背景区域像素应当保持完全一致（实际相同像素 {}）",
        identical_pixels_count
    );
}

#[test]
fn interactive_epub_words_and_links() {
    let engine = CrengineEngine::with_default_fonts().expect("初始化引擎失败");
    let interactive_epub = manifest_dir().join("fixtures/interactive.epub");
    let doc = engine
        .open(
            &interactive_epub,
            (PAGE_WIDTH, PAGE_HEIGHT),
            &LayoutSettings::default(),
        )
        .expect("打开 interactive.epub 失败");
    assert!(doc.page_xpointer(0).is_ok());
    assert!(doc.page_xpointer(1).is_ok());
    let links = doc.page_links(0).expect("取第 0 页链接失败");
    assert!(!links.is_empty(), "第 0 页应包含链接");
    let words = doc.page_words(0).expect("取第 0 页词失败");
    assert!(!words.is_empty(), "第 0 页应包含词");
    assert!(words.iter().any(|w| w.text == "文"));
}
