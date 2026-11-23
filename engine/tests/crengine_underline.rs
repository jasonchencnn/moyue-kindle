//! 正文每行下方引导线的行为测试（KO2-UNDERLINE-001 / TYPE-101）。
//!
//! 需要 `--features crengine`，并且要有能编译 C++ 引擎的工具链。
//! 跑法见 `scripts/check-engine.sh`。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{
    Document, Engine, LayoutSettings, Margins, RenderOptions, Underline, UnderlineScope,
    DEFAULT_MARGIN_HORIZONTAL_PX, DEFAULT_MARGIN_VERTICAL_PX,
};
use inkread_platform::GrayBuffer;

/// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

/// 引导线的灰度（0 黑、255 白），与 aristotle-render 的 `UNDERLINE_GRAY` 一致。
const GRAY: u8 = 0x20;

const MARGIN_LEFT: u32 = DEFAULT_MARGIN_HORIZONTAL_PX;
const MARGIN_RIGHT: u32 = DEFAULT_MARGIN_HORIZONTAL_PX;
const MARGIN_TOP: u32 = DEFAULT_MARGIN_VERTICAL_PX;
const MARGIN_BOTTOM: u32 = DEFAULT_MARGIN_VERTICAL_PX;

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

fn render(
    doc: &mut Box<dyn Document>,
    page: u32,
    underline: Underline,
    scope: UnderlineScope,
) -> GrayBuffer {
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page_with_options(
        page,
        &mut buffer,
        &RenderOptions {
            invert_images: false,
            underline,
            underline_gray: GRAY,
            underline_scope: scope,
        },
    )
    .expect("渲染失败");
    buffer
}

/// 一行引导线的跨度：`(y, 最左像素, 最右像素)`（左右都含）。
type Span = (usize, usize, usize);

/// 找出只有引导线时才出现的像素行。
///
/// 对比「不画引导线」与「画引导线」两次渲染，差出来的像素就是引导线本身：
/// 文字的灰度上也可能是 `0x20`，直接按颜色找会把字形的抗锯齿边缘算进来。
fn guide_spans(off: &GrayBuffer, on: &GrayBuffer, gray: u8) -> Vec<Span> {
    let width = PAGE_WIDTH as usize;
    let height = PAGE_HEIGHT as usize;
    let mut spans = Vec::new();
    for y in 0..height {
        let start = y * width;
        let mut first = None;
        let mut last = 0usize;
        let mut count = 0usize;
        for x in 0..width {
            let i = start + x;
            if off.pixels()[i] != on.pixels()[i] {
                assert_eq!(on.pixels()[i], gray, "第 {y} 行第 {x} 列不是引导线灰度");
                first.get_or_insert(x);
                last = x;
                count += 1;
            }
        }
        // 点线是断开的，一行至少 8 个像素才算画了引导线。
        if count >= 8 {
            spans.push((y, first.unwrap(), last));
        }
    }
    spans
}

#[test]
fn text_only_guide_follows_the_text_width() {
    let engine = engine();
    let mut doc = open(&engine, manifest_dir().join("fixtures/sample.epub"));

    let off = render(&mut doc, 0, Underline::Off, UnderlineScope::TextOnly);
    let on = render(&mut doc, 0, Underline::Solid, UnderlineScope::TextOnly);
    let spans = guide_spans(&off, &on, GRAY);
    assert!(!spans.is_empty(), "文字划线一行都没画出来");

    let body_width = (PAGE_WIDTH - MARGIN_LEFT - MARGIN_RIGHT) as usize;
    let body_bottom = (PAGE_HEIGHT - MARGIN_BOTTOM) as usize;
    for (y, x0, x1) in &spans {
        assert!(
            *x0 >= MARGIN_LEFT as usize,
            "第 {y} 行的引导线跑到左边距里：x0={x0}"
        );
        assert!(
            *x1 < (PAGE_WIDTH - MARGIN_RIGHT) as usize,
            "第 {y} 行的引导线跑到右边距里：x1={x1}"
        );
        assert!(
            x1 - x0 <= body_width,
            "文字划线超出了正文区：第 {y} 行宽 {}，正文区宽 {body_width}",
            x1 - x0
        );
        // 纵向也必须落在正文区里：线宽 2px，最后一行像素是 y+1。
        assert!(
            *y >= MARGIN_TOP as usize,
            "第 {y} 行的引导线画进了上边距：y={y}"
        );
        assert!(
            *y + 2 <= body_bottom,
            "第 {y} 行的引导线画进了下边距：y+2={}，正文区底部是 {body_bottom}",
            y + 2
        );
    }

    // 段末的短行一定比正文区窄：这就是「只画文字宽度」和「整行」的区别。
    assert!(
        spans.iter().any(|(_, x0, x1)| x1 - x0 < body_width - 8),
        "测试页上没有找到短于正文区的文字划线"
    );
}

#[test]
fn full_line_guide_spans_the_margins_and_fills_blank_slots() {
    let engine = engine();
    let mut doc = open(&engine, manifest_dir().join("fixtures/sample.epub"));
    let last = doc.page_count() - 1;

    let off = render(&mut doc, last, Underline::Off, UnderlineScope::FullLine);
    let on = render(&mut doc, last, Underline::Solid, UnderlineScope::FullLine);
    let spans = guide_spans(&off, &on, GRAY);
    assert!(!spans.is_empty(), "整行划线一行都没画出来");

    let body_left = MARGIN_LEFT as usize;
    let body_right = (PAGE_WIDTH - MARGIN_RIGHT) as usize;
    for (y, x0, x1) in &spans {
        assert_eq!(*x0, body_left, "第 {y} 行的整行划线没有从左边距开始");
        assert_eq!(*x1, body_right - 1, "第 {y} 行的整行划线没有到右边距");
        assert!(*y >= MARGIN_TOP as usize, "第 {y} 行的整行划线画进了上边距");
        assert!(
            *y + 1 < (PAGE_HEIGHT - MARGIN_BOTTOM) as usize,
            "第 {y} 行的整行划线画进了下边距"
        );
    }

    // 短页的末尾：最后一条线要接近正文区底部（空白行槽补满），而不是停在
    // 最后一行文字下面。
    let body_bottom = (PAGE_HEIGHT - MARGIN_BOTTOM) as usize;
    let max_y = spans.iter().map(|(y, _, _)| *y).max().unwrap();
    assert!(
        max_y + 2 > body_bottom.saturating_sub(160),
        "整行划线没有补到页面底部：最后一条线在 y={max_y}，正文区底部是 {body_bottom}"
    );

    // 同一页上，整行模式比文字模式多出空白行槽的线。
    let text_off = render(&mut doc, last, Underline::Off, UnderlineScope::TextOnly);
    let text_on = render(&mut doc, last, Underline::Solid, UnderlineScope::TextOnly);
    let text_spans = guide_spans(&text_off, &text_on, GRAY);
    assert!(
        spans.len() > text_spans.len(),
        "整行模式应当比文字模式多出空白行槽的线：整行 {} 行，文字 {} 行",
        spans.len(),
        text_spans.len()
    );
}

#[test]
fn the_new_gray_is_what_gets_drawn() {
    let engine = engine();
    let mut doc = open(&engine, manifest_dir().join("fixtures/sample.epub"));

    let off = render(&mut doc, 0, Underline::Off, UnderlineScope::TextOnly);
    for scope in [UnderlineScope::TextOnly, UnderlineScope::FullLine] {
        for underline in [Underline::Solid, Underline::Dotted] {
            let on = render(&mut doc, 0, underline, scope);
            let spans = guide_spans(&off, &on, GRAY);
            assert!(
                !spans.is_empty(),
                "{underline:?} / {scope:?} 一行都没画出来"
            );
        }
    }

    // 显式给 0 仍然画纯黑：0x20 是调用方（aristotle-render）的默认值，
    // 不是引擎硬编码的颜色。
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
    let black_spans = guide_spans(&off, &black, 0);
    assert!(!black_spans.is_empty(), "灰度 0 的引导线没有画出来");
}

#[test]
fn guides_never_leave_the_page_in_tight_viewports() {
    let engine = engine();
    let narrow = LayoutSettings {
        margins: Margins {
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
        },
        ..LayoutSettings::default()
    };
    for height in [80, 96, 112] {
        let mut doc = engine
            .open(
                &manifest_dir().join("fixtures/sample.epub"),
                (320, height),
                &narrow,
            )
            .expect("open compact viewport");
        let mut off = GrayBuffer::new(320, height);
        doc.render_page(0, &mut off).unwrap();

        for scope in [UnderlineScope::TextOnly, UnderlineScope::FullLine] {
            for underline in [Underline::Solid, Underline::Dotted] {
                let mut on = GrayBuffer::new(320, height);
                doc.render_page_with_options(
                    0,
                    &mut on,
                    &RenderOptions {
                        underline,
                        underline_gray: GRAY,
                        underline_scope: scope,
                        ..RenderOptions::default()
                    },
                )
                .unwrap();
                assert_eq!(on.pixels().len(), (320 * height) as usize);
                for (i, (before, after)) in off.pixels().iter().zip(on.pixels()).enumerate() {
                    if *before < 32 {
                        assert_eq!(
                            before, after,
                            "引导线覆盖了字形：height={height}, {underline:?}/{scope:?}, 像素 {i}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn full_line_handles_margins_wider_than_the_page() {
    let engine = engine();
    let wide_margins = LayoutSettings {
        margins: Margins {
            left: 256,
            right: 256,
            top: 60,
            bottom: 60,
        },
        ..LayoutSettings::default()
    };
    let mut doc = engine
        .open(
            &manifest_dir().join("fixtures/sample.epub"),
            (320, 200),
            &wide_margins,
        )
        .expect("open narrow page with wide margins");
    let mut buffer = GrayBuffer::new(320, 200);
    // 左右边距加起来超过页宽：没有可画的地方，但不能崩、不能越界写。
    doc.render_page_with_options(
        0,
        &mut buffer,
        &RenderOptions {
            underline: Underline::Solid,
            underline_gray: GRAY,
            underline_scope: UnderlineScope::FullLine,
            ..RenderOptions::default()
        },
    )
    .expect("render full line on a page whose margins exceed its width");
    assert_eq!(buffer.pixels().len(), 320 * 200);
}
