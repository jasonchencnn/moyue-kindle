//! 缓存目录用不了时，书照样能打开。
//!
//! 缓存只是加速手段：目录建不出来、索引写不进去，缓存就当没开，引擎照常
//! 排版。crengine 那边是 `ldomDocCache::init()` 返回 false 时把缓存实例删掉，
//! 这一层要保证的是「开缓存失败不会把开书也带崩」。
//!
//! 缓存是进程级的设置，所以这个文件单独一个进程（每个集成测试文件一个进程）。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::{cache_enabled, configure_cache, CrengineEngine};
use inkread_engine::{Engine, LayoutSettings};
use inkread_platform::GrayBuffer;

/// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

/// 缓存目录的容量上限。
const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[test]
fn a_cache_directory_that_cannot_be_created_does_not_stop_the_book_from_opening() {
    let root = std::env::temp_dir().join(format!(
        "inkread-engine-cache-unusable-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("建临时目录失败");

    // 缓存目录的父路径是一个普通文件：目录建不出来（ENOTDIR）。
    // 这样不用改权限，谁跑都一样。
    let blocker = root.join("blocker");
    std::fs::write(&blocker, b"not a directory").expect("写临时文件失败");
    let cache_dir = blocker.join("cache");

    let result = configure_cache(&cache_dir, CACHE_MAX_BYTES);
    assert!(
        result.is_err(),
        "缓存目录建不出来时应该报错，而不是假装开好了"
    );
    assert!(!cache_enabled(), "缓存没开");

    // 书照样能打开、能排版、能渲染。
    let engine = CrengineEngine::with_default_fonts().expect("初始化引擎失败");
    let layout = LayoutSettings::default();
    let mut doc = engine
        .open(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/cache.epub"),
            (PAGE_WIDTH, PAGE_HEIGHT),
            &layout,
        )
        .expect("缓存开不了也要能打开书");
    assert!(doc.page_count() > 0, "书应该有页");
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut buffer).expect("渲染失败");

    let _ = std::fs::remove_dir_all(&root);
}
