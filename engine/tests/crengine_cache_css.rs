//! 带内嵌/外链 CSS 的电子书在 crengine 文档缓存机制下的复用测试（ENG-009）。
//!
//! 背景：
//! ENG-008 接上了 crengine 缓存，但发现带内嵌与外链 CSS 的书籍（如《紅樓夢》）
//! 在重开读回缓存时，checkRenderContext() 样式哈希不一致，导致只省了解析但每次仍会全文重排。
//! ENG-009 修复了属性设置时序：先 Resize 和 apply_layout 再 LoadDocument，
//! 使得读缓存时能在完全一致的视口和排版属性下加载节点字体与级联样式，样式哈希完全匹配，
//! 真正达成「免重排」命中。
//!
//! 本测试验证：
//! 1. 带内嵌/外链 CSS 的 fixture 首次打开写入缓存；
//! 2. 第二次打开确认读缓存且**确认没有重排**（`opened_from_cache` 为 true，`reformatted` 为 false）；
//! 3. 两次打开的页数、每页 XPointer、逐页像素指纹严格一致；
//! 4. 修改字号后重新打开，DOM 从缓存载入但触发重排（`reformatted` 为 true），页数发生正确改变；
//! 5. 再次以新字号打开，确认新字号下也能免重排命中。

#![cfg(feature = "crengine")]

use std::path::{Path, PathBuf};

use inkread_engine::crengine::{cache_enabled, configure_cache, CrengineEngine};
use inkread_engine::{Engine, LayoutSettings};
use inkread_platform::GrayBuffer;

const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;
const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 包含外链 style.css 与内嵌 <style> 块、超过 64 KB 的测试书。
fn fixture_path() -> PathBuf {
    manifest_dir().join("fixtures/cache_css.epub")
}

#[derive(Debug, PartialEq, Eq)]
struct BookSnapshot {
    pages: u32,
    xpointers: Vec<String>,
    fingerprints: Vec<u64>,
    from_cache: bool,
    reformatted: bool,
}

fn open_and_probe(engine: &CrengineEngine, path: &Path, layout: &LayoutSettings) -> BookSnapshot {
    let mut doc = engine
        .open(path, (PAGE_WIDTH, PAGE_HEIGHT), layout)
        .expect("打开测试书失败");
    let pages = doc.page_count();
    assert!(pages > 0, "测试书应该有页");
    let mut xpointers = Vec::with_capacity(pages as usize);
    let mut fingerprints = Vec::with_capacity(pages as usize);
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    for page in 0..pages {
        xpointers.push(doc.page_xpointer(page).expect("取 XPointer 失败"));
        doc.render_page(page, &mut buffer).expect("渲染失败");
        fingerprints.push(fingerprint(buffer.pixels()));
    }
    BookSnapshot {
        pages,
        xpointers,
        fingerprints,
        from_cache: doc.opened_from_cache(),
        reformatted: doc.reformatted(),
    }
}

fn fingerprint(pixels: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in pixels {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("inkread-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录失败");
    dir
}

#[test]
fn book_with_css_hits_render_cache_without_reformatting_on_second_open() {
    let root = temp_dir("cache-css");
    let cache_dir = root.join("cache");
    configure_cache(&cache_dir, CACHE_MAX_BYTES).expect("初始化文档缓存失败");
    assert!(cache_enabled(), "缓存应该已开启");

    let engine = CrengineEngine::with_default_fonts().expect("初始化引擎失败");
    let book = fixture_path();
    let default_layout = LayoutSettings::default();

    // 1. 首次打开：无缓存，必须完整执行排版并写入缓存
    let first = open_and_probe(&engine, &book, &default_layout);
    assert!(!first.from_cache, "首次打开不应读缓存");
    assert!(first.reformatted, "首次打开必须执行排版");
    assert!(first.pages > 0, "首次排版页数应大于 0");

    // 2. 第二次打开：同一本书、相同排版参数，必须命中缓存且绝不重排
    let second = open_and_probe(&engine, &book, &default_layout);
    assert!(second.from_cache, "第二次打开应该从缓存读出");
    assert!(
        !second.reformatted,
        "第二次打开必须命中排版缓存，确认没有重排"
    );
    assert_eq!(first.pages, second.pages, "两次打开的页数必须完全一致");
    assert_eq!(
        first.xpointers, second.xpointers,
        "两次打开的每页 XPointer 必须完全一致"
    );
    assert_eq!(
        first.fingerprints, second.fingerprints,
        "两次打开渲染出的页面像素必须完全一致"
    );

    // 3. 修改排版参数（字号由默认的 46 改为 56）：
    // DOM 从缓存快速载入，但由于样式与视口上下文改变，必须触发重排且结果正确
    let large_layout = LayoutSettings {
        font_size_px: 56,
        ..Default::default()
    };
    let third = open_and_probe(&engine, &book, &large_layout);
    assert!(third.from_cache, "改字号打开仍应复用缓存里的 DOM");
    assert!(third.reformatted, "改字号后必须触发重排");
    assert_ne!(first.pages, third.pages, "字号放大后总页数应当发生显著改变");

    // 4. 以新字号再次打开：应当再次命中新字号下的排版缓存，免重排
    let fourth = open_and_probe(&engine, &book, &large_layout);
    assert!(fourth.from_cache, "再次打开应该读缓存");
    assert!(
        !fourth.reformatted,
        "以相同新字号打开必须命中缓存，确认没有重排"
    );
    assert_eq!(third.pages, fourth.pages, "新字号两次打开的页数必须一致");
    assert_eq!(
        third.xpointers, fourth.xpointers,
        "新字号两次打开的 XPointer 必须一致"
    );
    assert_eq!(
        third.fingerprints, fourth.fingerprints,
        "新字号两次打开的像素必须一致"
    );

    let _ = std::fs::remove_dir_all(&root);
}
