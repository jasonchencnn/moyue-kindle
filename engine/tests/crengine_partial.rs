//! 测试 koreader/crengine 的局部重排能力（ADR-015，ENG-010）。
//!
//! 验证：
//! 1. 开启缓存且包含多个 DocFragment 的文档支持局部重排（`can_partial_rerender()` == true）；
//! 2. 启用局部重排后修改排版参数，文档进入延迟重排状态（`is_rerender_delayed()` == true）；
//! 3. 渲染当前页时仅对当前分片执行排版，当前页的 XPointer 与全量重排严格一致；
//! 4. 调用 `catchup_render()` 后退出延迟状态，全书总页数与全量重排结果完全一致。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::{configure_cache, CrengineEngine};
use inkread_engine::{Engine, LayoutSettings};
use inkread_platform::GrayBuffer;

const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;
const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample.epub")
}

fn hlm_path() -> Option<PathBuf> {
    let path = dirs_home().join(".cache/aristotle/testbooks/hlm.epub");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

fn temp_cache_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "inkread-engine-partial-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录失败");
    dir
}

#[test]
fn test_partial_rerender_sample_epub() {
    let cache_dir = temp_cache_dir("sample");
    let _ = configure_cache(&cache_dir, CACHE_MAX_BYTES);

    let engine = CrengineEngine::with_default_fonts().expect("初始化引擎失败");
    let book = fixture_path();

    let initial_layout = LayoutSettings {
        font_size_px: 46,
        ..LayoutSettings::default()
    };

    // 1. 打开文档
    let mut doc = engine
        .open(&book, (PAGE_WIDTH, PAGE_HEIGHT), &initial_layout)
        .expect("打开 sample.epub 失败");

    // 2. 检查能否局部重排（sample.epub 包含多个 DocFragment 且缓存已开启）
    assert!(
        doc.can_partial_rerender(),
        "多分片文档且启用缓存时应支持局部重排"
    );

    // 3. 开启局部重排
    doc.enable_partial_rerender(true).expect("开启局部重排失败");
    assert!(doc.is_partial_rerender_enabled(), "应处于开启局部重排状态");

    // 4. 改字号触发延迟重排
    let new_layout = LayoutSettings {
        font_size_px: 56,
        ..initial_layout.clone()
    };
    doc.set_layout(&new_layout).expect("改排版参数失败");
    assert!(doc.is_rerender_delayed(), "改排版后应处于延迟重排状态");

    // 5. 渲染当前正在阅读的第 0 页，触发当前分片的局部排版
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut buffer).expect("渲染第 0 页失败");

    let partial_xp = doc.page_xpointer(0).expect("获取第 0 页 XPointer 失败");

    // 6. 对照全量重排
    let mut full_doc = engine
        .open(&book, (PAGE_WIDTH, PAGE_HEIGHT), &new_layout)
        .expect("打开全量对比文档失败");
    let mut full_buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    full_doc
        .render_page(0, &mut full_buffer)
        .expect("全量渲染第 0 页失败");
    let full_xp = full_doc
        .page_xpointer(0)
        .expect("获取全量第 0 页 XPointer 失败");

    assert_eq!(
        partial_xp, full_xp,
        "局部重排后当前页的 XPointer 与全量重排应严格一致"
    );

    // 7. 后台补完全书
    doc.catchup_render().expect("后台补全排版失败");
    assert!(
        !doc.is_rerender_delayed(),
        "补全排版后不应再处于延迟重排状态"
    );
    assert_eq!(
        doc.page_count(),
        full_doc.page_count(),
        "补全排版后总页数应与全量重排一致"
    );

    // 8. 补全后所有页的 XPointer 与全量重排一致
    for p in 0..doc.page_count() {
        assert_eq!(
            doc.page_xpointer(p).unwrap(),
            full_doc.page_xpointer(p).unwrap(),
            "补全后第 {p} 页 XPointer 与全量重排一致"
        );
    }
}

#[test]
fn test_partial_rerender_hlm() {
    let Some(hlm) = hlm_path() else {
        eprintln!("跳过：~/.cache/aristotle/testbooks/hlm.epub 不存在");
        return;
    };

    let cache_dir = temp_cache_dir("hlm");
    let _ = configure_cache(&cache_dir, CACHE_MAX_BYTES);

    let engine = CrengineEngine::with_default_fonts().expect("初始化引擎失败");

    let initial_layout = LayoutSettings {
        font_size_px: 46,
        ..LayoutSettings::default()
    };

    let mut doc = engine
        .open(&hlm, (PAGE_WIDTH, PAGE_HEIGHT), &initial_layout)
        .expect("打开 hlm.epub 失败");

    assert!(doc.can_partial_rerender(), "红楼梦必须支持局部重排");

    doc.enable_partial_rerender(true).expect("开启局部重排失败");

    let new_layout = LayoutSettings {
        font_size_px: 56,
        ..initial_layout.clone()
    };

    doc.set_layout(&new_layout).expect("改排版参数失败");
    assert!(doc.is_rerender_delayed(), "改排版后应处于延迟状态");

    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(5, &mut buffer).expect("渲染第 5 页失败");

    let partial_xp = doc.page_xpointer(5).expect("获取第 5 页 XPointer 失败");

    doc.catchup_render().expect("后台补全排版失败");
    assert!(!doc.is_rerender_delayed());

    let final_pages = doc.page_count();
    assert!(final_pages > 2000, "红楼梦 56px 下应有超过 2000 页");
    let catchup_xp = doc
        .page_xpointer(5)
        .expect("获取补全后第 5 页 XPointer 失败");
    assert_eq!(
        partial_xp, catchup_xp,
        "补全前后第 5 页 XPointer 应当保持一致"
    );
}
