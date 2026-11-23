//! 精排开关与字重渲染测试（TYPE-101）。
//!
//! 需要 `--features crengine`，并且要有能编译 C++ 引擎的工具链。
//! 跑法见 `scripts/check-engine.sh`。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{
    Document, Engine, LayoutSettings, FONT_WEIGHT_BOLD, FONT_WEIGHT_LIGHT, FONT_WEIGHT_NORMAL,
};
use inkread_platform::GrayBuffer;

const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn engine() -> CrengineEngine {
    CrengineEngine::with_default_fonts().expect("初始化 crengine 引擎失败")
}

fn open(engine: &CrengineEngine, layout: &LayoutSettings) -> Box<dyn Document> {
    open_with_size(engine, layout, (PAGE_WIDTH, PAGE_HEIGHT))
}

fn open_with_size(
    engine: &CrengineEngine,
    layout: &LayoutSettings,
    size: (u32, u32),
) -> Box<dyn Document> {
    engine
        .open(&manifest_dir().join("fixtures/sample.epub"), size, layout)
        .expect("打开文档失败")
}

fn render(doc: &mut Box<dyn Document>, page: u32) -> GrayBuffer {
    render_with_size(doc, page, (PAGE_WIDTH, PAGE_HEIGHT))
}

fn render_with_size(doc: &mut Box<dyn Document>, page: u32, size: (u32, u32)) -> GrayBuffer {
    let mut buffer = GrayBuffer::new(size.0, size.1);
    doc.render_page(page, &mut buffer).expect("渲染失败");
    buffer
}

/// 页面上「有墨」的像素数：灰度低于 128 的算墨。
fn ink(buffer: &GrayBuffer) -> usize {
    buffer.pixels().iter().filter(|pixel| **pixel < 128).count()
}

fn render_with_weight(engine: &CrengineEngine, weight: u16) -> GrayBuffer {
    // 页面足够高，让整章文字都排在第 0 页上：这样 300/400/600 比较的是
    // 同一段文字，不会因为粗体更宽、换行更多而少掉半页。
    let size = (PAGE_WIDTH, 8000);
    let layout = LayoutSettings {
        font_weight: weight,
        ..LayoutSettings::default()
    };
    let mut doc = open_with_size(engine, &layout, size);
    assert!(doc.page_count() > 0, "字重 {weight} 的文档没有页");
    render_with_size(&mut doc, 0, size)
}

#[test]
fn heavier_weight_puts_more_ink_on_the_page() {
    let engine = engine();
    let light = render_with_weight(&engine, FONT_WEIGHT_LIGHT);
    let normal = render_with_weight(&engine, FONT_WEIGHT_NORMAL);
    let bold = render_with_weight(&engine, FONT_WEIGHT_BOLD);

    let light_ink = ink(&light);
    let normal_ink = ink(&normal);
    let bold_ink = ink(&bold);

    assert!(normal_ink > 0, "400 字重渲染出来是空白页");
    assert!(
        bold_ink > normal_ink,
        "600 字重的墨量应当大于 400：{bold_ink} vs {normal_ink}"
    );
    assert!(
        bold_ink >= normal_ink + normal_ink / 100,
        "600 字重应当明显比 400 重（至少多 1%）：{bold_ink} vs {normal_ink}"
    );
    assert!(
        normal_ink >= light_ink,
        "400 字重的墨量不该少于 300：{normal_ink} vs {light_ink}"
    );
}

#[test]
fn fine_typesetting_can_be_switched_off_and_on() {
    let engine = engine();

    let mut fine_doc = open(
        &engine,
        &LayoutSettings {
            fine_typesetting: true,
            ..LayoutSettings::default()
        },
    );
    let fine = render(&mut fine_doc, 0);

    let mut plain_doc = open(
        &engine,
        &LayoutSettings {
            fine_typesetting: false,
            ..LayoutSettings::default()
        },
    );
    assert!(plain_doc.page_count() > 0, "关掉精排后文档没有页");
    let plain = render(&mut plain_doc, 0);

    assert!(ink(&fine) > 0, "精排页面是空白页");
    assert!(ink(&plain) > 0, "传统排版页面是空白页");
    assert_ne!(
        fine.pixels(),
        plain.pixels(),
        "精排开/关渲染出的页面完全一样，开关没有生效"
    );
}

#[test]
fn out_of_range_weight_is_rejected_before_touching_the_engine() {
    let engine = engine();
    for weight in [99, 901] {
        let err = engine
            .open(
                &manifest_dir().join("fixtures/sample.epub"),
                (PAGE_WIDTH, PAGE_HEIGHT),
                &LayoutSettings {
                    font_weight: weight,
                    ..LayoutSettings::default()
                },
            )
            .err()
            .expect("越界字重应当报错");
        assert!(
            matches!(err, inkread_engine::Error::InvalidLayout(_)),
            "越界字重应当返回 InvalidLayout，实际：{err:?}"
        );
    }
}
