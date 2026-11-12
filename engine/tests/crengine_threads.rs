//! ENG-007：crengine 跨线程使用的实测。
//!
//! 引擎的文档类型是 `Send` 的：阅读器在工作线程里开书，再把文档交给主线程。
//! 前提是所有进入 C shim 的调用都被一把进程级互斥锁串起来
//! （`engine/src/crengine/ffi.rs` 的 `with_engine_lock`）。
//!
//! 这里验两件事：
//! - 两个线程各开一本书、交替渲染若干页，不崩；
//! - 渲染出来的像素和单线程渲染的完全一样——串行化没有串味。

#![cfg(feature = "crengine")]

use std::path::{Path, PathBuf};
use std::thread;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Engine, LayoutSettings};
use inkread_platform::GrayBuffer;

const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;
/// 两个线程交替渲染的页数。
const PAGES: u32 = 4;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample.epub")
}

fn engine() -> CrengineEngine {
    CrengineEngine::with_default_fonts().expect("初始化 crengine 引擎失败")
}

/// 单线程渲染前 `PAGES` 页，返回每一页的像素。
fn render_pages(path: &Path, layout: &LayoutSettings) -> Vec<Vec<u8>> {
    let eng = engine();
    let mut doc = eng
        .open(path, (PAGE_WIDTH, PAGE_HEIGHT), layout)
        .expect("open");
    let mut pages = Vec::new();
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    for page in 0..PAGES {
        buffer.fill(255);
        doc.render_page(page, &mut buffer).expect("render");
        pages.push(buffer.pixels().to_vec());
    }
    pages
}

#[test]
fn two_threads_open_and_render_the_same_book_without_crashing() {
    let path = fixture();
    let layout = LayoutSettings::default();

    // 单线程的基准。
    let baseline = render_pages(&path, &layout);
    assert_eq!(baseline.len(), PAGES as usize);

    // 两个线程各开一本书，交替渲染：谁先谁后不确定，也不该影响结果。
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let layout = layout.clone();
            thread::spawn(move || render_pages(&path, &layout))
        })
        .collect();

    for handle in handles {
        let pages = handle.join().expect("线程 join 失败");
        assert_eq!(pages.len(), baseline.len());
        for (index, (page, expected)) in pages.iter().zip(baseline.iter()).enumerate() {
            assert_eq!(page.len(), expected.len(), "第 {index} 页的缓冲长度不对");
            assert!(page == expected, "第 {index} 页和单线程渲染的结果不一致");
        }
    }
}

#[test]
fn a_document_can_be_opened_on_one_thread_and_rendered_on_another() {
    let path = fixture();
    let layout = LayoutSettings::default();
    let baseline = render_pages(&path, &layout);

    // 阅读器的实际用法：工作线程开书，文档交给主线程渲染。
    let handle = thread::spawn(move || {
        let eng = engine();
        eng.open(&path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
            .expect("在工作线程里开书")
    });

    let mut doc = handle.join().expect("线程 join 失败");
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    for page in 0..PAGES {
        buffer.fill(255);
        doc.render_page(page, &mut buffer).expect("在主线程里渲染");
        assert!(
            buffer.pixels() == baseline[page as usize].as_slice(),
            "换线程渲染的第 {page} 页和单线程渲染的不一致"
        );
    }
}
