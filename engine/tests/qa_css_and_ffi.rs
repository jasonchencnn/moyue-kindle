//! QA-002: engine crate 测试（书籍嵌入样式实测与 FFI 健壮性）。
//!
//! 重点核查：
//! - 验证书籍自带的 CSS（<style>、<link rel="stylesheet">、行内 style）是否真正生效，
//!   反驳 ENG-002 notes 声称的「内置样式表替换书籍自带样式表，自带 CSS 被丢弃」假设。
//! - 多线程并发调用排版与渲染时的安全性（验证 FFI 层的全局互斥锁有效性）。
//! - FFI 缺陷测试：GrayBuffer 尺寸不匹配检查、超大页面尺寸校验。

#![cfg(feature = "crengine")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Engine, LayoutSettings};
use inkread_platform::GrayBuffer;

const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure_fixtures() -> (PathBuf, PathBuf) {
    let embedded_path = manifest_dir().join("fixtures/qa_embedded_style.epub");
    let plain_path = manifest_dir().join("fixtures/qa_plain.epub");
    if !embedded_path.exists() || !plain_path.exists() {
        let script = manifest_dir().join("fixtures/make_qa_epubs.py");
        let status = Command::new("python3")
            .arg(&script)
            .status()
            .expect("failed to execute make_qa_epubs.py");
        assert!(status.success(), "make_qa_epubs.py failed");
    }
    (embedded_path, plain_path)
}

fn engine() -> CrengineEngine {
    CrengineEngine::with_default_fonts().expect("初始化 crengine 引擎失败")
}

/// 扫描指定垂直行区间 `y_start..=y_end` 内深色墨迹（灰度 < 250）的水平范围 `[min_x, max_x]`。
fn find_ink_x_bounds(buffer: &GrayBuffer, y_start: u32, y_end: u32) -> Option<(u32, u32)> {
    let mut min_x = buffer.width();
    let mut max_x = 0;
    let mut found = false;

    for y in y_start..=y_end {
        for x in 0..buffer.width() {
            if let Some(pixel) = buffer.get_pixel(x as i32, y as i32) {
                if pixel < 250 {
                    found = true;
                    if x < min_x {
                        min_x = x;
                    }
                    if x > max_x {
                        max_x = x;
                    }
                }
            }
        }
    }

    if found {
        Some((min_x, max_x))
    } else {
        None
    }
}

// -----------------------------------------------------------------------------
// 1. 书籍自带 CSS 是否生效的实测核查
// -----------------------------------------------------------------------------

#[test]
fn book_embedded_styles_are_retained_and_override_default_css() {
    let (embedded_path, plain_path) = ensure_fixtures();
    let eng = engine();
    let layout = LayoutSettings::default();

    // 1. 打开带 <style> 的书并渲染第 0 页
    let mut doc_embedded = eng
        .open(&embedded_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("open embedded epub");
    let mut buf_embedded = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc_embedded
        .render_page(0, &mut buf_embedded)
        .expect("render embedded page");

    // 2. 打开无嵌入样式的书并渲染第 0 页作为基准对照
    let mut doc_plain = eng
        .open(&plain_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("open plain epub");
    let mut buf_plain = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc_plain
        .render_page(0, &mut buf_plain)
        .expect("render plain page");

    // 段落 1: "居中文本测试段落"（y 范围约 98..141）
    let (emb_min1, emb_max1) =
        find_ink_x_bounds(&buf_embedded, 98, 141).expect("find embedded line 1 ink");
    let (pln_min1, pln_max1) =
        find_ink_x_bounds(&buf_plain, 98, 141).expect("find plain line 1 ink");

    let emb_center1 = (emb_min1 + emb_max1) / 2;
    let page_center = PAGE_WIDTH / 2; // 632

    // 证据 1：在带有 <style> 的书中，第 1 段确实被居中排版（中心在 632 附近）
    assert!(
        (emb_center1 as i32 - page_center as i32).abs() <= 10,
        "embedded style center paragraph should be centered at ~632, got midpoint {emb_center1}"
    );

    // 对照 1：在无嵌入样式的书中，第 1 段为默认左对齐 + 2em 首行缩进（左边距约 165）
    assert!(
        pln_min1 <= 175,
        "plain line 1 should start near left margin + indent (~165), got {pln_min1}"
    );
    assert!(
        pln_max1 < 600,
        "plain line 1 should not reach the center or right, got max_x {pln_max1}"
    );

    // 段落 2: "右对齐文本测试段落"（y 范围约 232..276，<style> p.right-test { text-align: right }）
    let (_emb_min2, emb_max2) =
        find_ink_x_bounds(&buf_embedded, 232, 276).expect("find embedded line 2 ink");
    let (_pln_min2, pln_max2) =
        find_ink_x_bounds(&buf_plain, 232, 276).expect("find plain line 2 ink");

    let expected_right_edge = PAGE_WIDTH - layout.margins.right; // 1264 - 72 = 1192

    // 证据 2：带有样式的书第 2 段右端紧贴右页边距（~1190）
    assert!(
        (emb_max2 as i32 - expected_right_edge as i32).abs() <= 10,
        "embedded style right-align paragraph should reach right margin (~1190), got {emb_max2}"
    );

    // 对照 2：无样式的书第 2 段保持左对齐，右端远离右页边距
    assert!(
        pln_max2 < 650,
        "plain line 2 should stay on the left side, got {pln_max2}"
    );

    // 结论：ENG-002 notes 中「setStyleSheet 替换文档自带样式表，自带 CSS 被丢弃」是误判；
    // 书自带的 <style> 与 class 样式被 crengine 完整保留并生效。
}

#[test]
fn book_external_and_inline_css_styles_are_retained() {
    let (embedded_path, _plain_path) = ensure_fixtures();
    let eng = engine();
    let layout = LayoutSettings::default();

    let mut doc = eng
        .open(&embedded_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("open embedded epub");
    let mut buf = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut buf).expect("render page");

    let expected_right = PAGE_WIDTH - layout.margins.right; // 1192

    // 段落 3: 外部 CSS（link rel="stylesheet"）中的 .external-right（y 范围约 367..412）
    let (_min3, max3) = find_ink_x_bounds(&buf, 367, 412).expect("find line 3 ink");
    assert!(
        (max3 as i32 - expected_right as i32).abs() <= 10,
        "external linked css should take effect and align right, got max_x {max3}"
    );

    // 段落 4: 行内 style="text-align: right;"（y 范围约 502..546）
    let (_min4, max4) = find_ink_x_bounds(&buf, 502, 546).expect("find line 4 ink");
    assert!(
        (max4 as i32 - expected_right as i32).abs() <= 10,
        "inline style should take effect and align right, got max_x {max4}"
    );
}

// -----------------------------------------------------------------------------
// 2. 多线程并发与引擎互斥锁安全性
// -----------------------------------------------------------------------------

#[test]
fn concurrent_rendering_across_multiple_threads() {
    let fixture = manifest_dir().join("fixtures/sample.epub");
    let eng = Arc::new(engine());
    let mut handles = Vec::new();

    for thread_idx in 0..4 {
        let eng_clone = Arc::clone(&eng);
        let fixture_path = fixture.clone();
        let handle = thread::spawn(move || {
            let layout = LayoutSettings {
                font_size_px: 36 + (thread_idx as u32) * 4,
                ..LayoutSettings::default()
            };
            let mut doc = eng_clone
                .open(&fixture_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
                .expect("open in thread");
            let page_count = doc.page_count();
            assert!(page_count > 0);

            let mut buf = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
            for p in 0..page_count.min(2) {
                doc.render_page(p, &mut buf).expect("render in thread");
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("thread join failed");
    }
}

// -----------------------------------------------------------------------------
// 3. FFI 缺陷测试（ENG-003 修复后不再忽略）
// -----------------------------------------------------------------------------

#[test]
fn defect_render_page_accepts_swapped_buffer_dimensions() {
    let fixture = manifest_dir().join("fixtures/sample.epub");
    let eng = engine();
    let layout = LayoutSettings::default();

    // 文档打开为 1264 x 1680
    let mut doc = eng
        .open(&fixture, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("open doc");

    // 传入宽长对调的 GrayBuffer(1680, 1264)，其总像素字节数完全相等 (2,123,520)
    let mut swapped_buf = GrayBuffer::new(PAGE_HEIGHT, PAGE_WIDTH);

    // 必须校验 buffer.width() == doc.width && buffer.height() == doc.height，
    // 不匹配时返回 Err(Error::Render("buffer dimension mismatch"))。
    // 只校验字节数不够：宽高对调时字节数一样，校验会通过，
    // 随后按 1264 步长写入 1680 宽的缓冲造成画面错位。
    let result = doc.render_page(0, &mut swapped_buf);
    assert!(
        result.is_err(),
        "render_page should reject buffer with mismatched width/height"
    );
}

#[test]
fn defect_open_accepts_excessive_page_size() {
    let fixture = manifest_dir().join("fixtures/sample.epub");
    let eng = engine();
    let layout = LayoutSettings::default();

    // open() 必须对 page_size 设上限校验（8192px）。传入 10000 x 10000 等
    // 超出合理显示设备范围的尺寸要被挡住；更大的尺寸（如 70000x70000）
    // 更会在底层 LVGrayDrawBuf/Resize 导致整型溢出甚至 SIGSEGV。
    let result = eng.open(&fixture, (10000, 10000), &layout);
    assert!(
        result.is_err(),
        "excessive page_size should be rejected as Error::InvalidLayout"
    );
}

// -----------------------------------------------------------------------------
// 4. embedded_styles 开关（ADR-010）
// -----------------------------------------------------------------------------

#[test]
fn embedded_styles_switch_off_restores_left_alignment() {
    let (embedded_path, _plain_path) = ensure_fixtures();
    let eng = engine();

    // 打开时关掉书内样式：书里的 <style> / 外链 CSS / 行内 style 都不该生效，
    // 段落回到 default.css 基础样式的左对齐 + 首行缩进。
    let layout = LayoutSettings {
        embedded_styles: false,
        ..LayoutSettings::default()
    };
    let mut doc = eng
        .open(&embedded_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("open embedded epub with embedded_styles off");
    let mut buf = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut buf).expect("render page");

    // 段落 1: "居中文本测试段落"（y 范围约 98..141）
    let (min_x, max_x) = find_ink_x_bounds(&buf, 98, 141).expect("find line 1 ink");
    let center = (min_x + max_x) / 2;
    let page_center = PAGE_WIDTH / 2;

    assert!(
        (center as i32 - page_center as i32).abs() > 10,
        "paragraph should NOT stay centered when embedded styles are off, midpoint {center}"
    );
    assert!(
        min_x <= 175,
        "paragraph should start near left margin + 2em indent (~165), got {min_x}"
    );
    assert!(
        max_x < 600,
        "paragraph should stay on the left side, got max_x {max_x}"
    );
}

#[test]
fn embedded_styles_switch_on_keeps_book_alignment() {
    let (embedded_path, _plain_path) = ensure_fixtures();
    let eng = engine();

    // 默认开启书内样式：段落仍然居中（与 QA-002 的实测结论一致）。
    let layout = LayoutSettings::default();
    assert!(
        layout.embedded_styles,
        "embedded styles should default to on"
    );

    let mut doc = eng
        .open(&embedded_path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("open embedded epub");
    let mut buf = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut buf).expect("render page");

    let (min_x, max_x) = find_ink_x_bounds(&buf, 98, 141).expect("find line 1 ink");
    let center = (min_x + max_x) / 2;
    let page_center = PAGE_WIDTH / 2;

    assert!(
        (center as i32 - page_center as i32).abs() <= 10,
        "paragraph should stay centered when embedded styles are on, midpoint {center}"
    );
}

#[test]
fn embedded_styles_can_be_toggled_after_open() {
    let (embedded_path, _plain_path) = ensure_fixtures();
    let eng = engine();

    // 先按默认（开着）打开，再通过 set_layout 关掉：重新排版后居中段落
    // 应当回到左对齐。
    let mut doc = eng
        .open(
            &embedded_path,
            (PAGE_WIDTH, PAGE_HEIGHT),
            &LayoutSettings::default(),
        )
        .expect("open embedded epub");
    let mut buf = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut buf).expect("render page");
    let (min_x, max_x) = find_ink_x_bounds(&buf, 98, 141).expect("find line 1 ink");
    let center = (min_x + max_x) / 2;
    assert!(
        (center as i32 - (PAGE_WIDTH / 2) as i32).abs() <= 10,
        "paragraph should be centered before toggling, midpoint {center}"
    );

    doc.set_layout(&LayoutSettings {
        embedded_styles: false,
        ..LayoutSettings::default()
    })
    .expect("set layout with embedded styles off");

    doc.render_page(0, &mut buf).expect("re-render page");
    let (min_x, max_x) = find_ink_x_bounds(&buf, 98, 141).expect("find line 1 ink after toggle");
    let center = (min_x + max_x) / 2;
    assert!(
        (center as i32 - (PAGE_WIDTH / 2) as i32).abs() > 10,
        "paragraph should NOT stay centered after toggling off, midpoint {center}"
    );
    assert!(
        min_x <= 175,
        "paragraph should start near left margin + 2em indent (~165), got {min_x}"
    );
}
