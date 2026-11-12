//! crengine 文档槽位的上限与释放。
//!
//! crengine 的文档实例表只有 16 个槽位，每本打开的文档占一个（打开时还会
//! 临时多占一个解析文档）。超过 14 本时引擎会 SIGSEGV（QA-101 §5），所以
//! 打开入口要在超限时返回错误。这里验两件事：
//! - 反复开关同一本书不会漏掉槽位；
//! - 开到上限再开一本会得到错误，关掉一本后又能打开。
//!
//! 两个测试都会长时间占着文档，必须串行：并行时一个测试的 14 本会把另一个
//! 测试挡在上限外。

#![cfg(feature = "crengine")]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use inkread_engine::crengine::{CrengineEngine, MAX_OPEN_DOCUMENTS};
use inkread_engine::{Engine, LayoutSettings};

const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample.epub")
}

fn engine() -> CrengineEngine {
    CrengineEngine::with_default_fonts().expect("初始化 crengine 引擎失败")
}

/// 串行化本文件里的测试：它们都会同时保留多个文档。
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

fn open(engine: &CrengineEngine, path: &Path) -> Result<Box<dyn inkread_engine::Document>, String> {
    engine
        .open(path, (PAGE_WIDTH, PAGE_HEIGHT), &LayoutSettings::default())
        .map_err(|err| err.to_string())
}

#[test]
fn opening_and_closing_the_same_book_fifty_times_releases_every_slot() {
    let _serial = serial();
    let path = fixture();
    let engine = engine();
    for index in 0..50 {
        let doc = open(&engine, &path).unwrap_or_else(|err| panic!("第 {index} 次打开失败：{err}"));
        assert!(doc.page_count() >= 1, "第 {index} 次打开没有页");
        drop(doc);
    }
}

#[test]
fn the_open_document_limit_returns_an_error_and_frees_on_close() {
    let _serial = serial();
    let path = fixture();
    let engine = engine();
    let mut docs = Vec::new();
    for index in 0..MAX_OPEN_DOCUMENTS {
        let doc =
            open(&engine, &path).unwrap_or_else(|err| panic!("第 {} 本打开失败：{err}", index + 1));
        docs.push(doc);
    }

    let err = match open(&engine, &path) {
        Ok(_) => panic!("第 {} 本应当被上限拦下", MAX_OPEN_DOCUMENTS + 1),
        Err(err) => err,
    };
    assert!(
        err.contains(&MAX_OPEN_DOCUMENTS.to_string()),
        "错误应当说明上限 {MAX_OPEN_DOCUMENTS}：{err}"
    );

    docs.pop();
    drop(open(&engine, &path).expect("关掉一本后应当能再打开"));
}
