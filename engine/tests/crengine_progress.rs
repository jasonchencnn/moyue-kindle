//! crengine 排版进度上报测试（APP-006）。
//!
//! 验证：
//! 1. 开书（冷打开）时，上报的进度单调不减，最后到达 100；
//! 2. 改字号（set_layout）重排时，上报的进度单调不减，最后到达 100；
//! 3. 命中缓存、不需要排版时，直接到 100，不经过任何中间值；
//! 4. DOM 读自缓存但排版参数改变触发重排时，上报进度单调不减且最后到达 100。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::{cache_enabled, configure_cache, CrengineEngine};
use inkread_engine::{Engine, LayoutSettings};

const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;
const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path() -> PathBuf {
    manifest_dir().join("fixtures/cache_css.epub")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("inkread-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录失败");
    dir
}

fn assert_monotonic_to_100(progress: &[u32]) {
    assert!(!progress.is_empty(), "进度上报不应为空");
    let mut prev = 0;
    for &p in progress {
        assert!(
            p >= prev,
            "进度应当单调不减：前值 {prev}，当前值 {p}（完整序列：{progress:?}）"
        );
        assert!(p <= 100, "进度不应超过 100：当前值 {p}");
        prev = p;
    }
    assert_eq!(*progress.last().unwrap(), 100, "最终进度必须到达 100");
}

#[test]
fn progress_monotonic_and_cache_hit_skips_intermediate() {
    let root = temp_dir("progress-test");
    let cache_dir = root.join("cache");
    configure_cache(&cache_dir, CACHE_MAX_BYTES).expect("初始化文档缓存失败");
    assert!(cache_enabled(), "缓存应该已开启");

    let engine = CrengineEngine::with_default_fonts().expect("初始化引擎失败");
    let book = fixture_path();
    let default_layout = LayoutSettings::default();

    // 1. 首次冷开：必须完整执行排版，进度单调不减到达 100
    let mut cold_progress = Vec::new();
    let doc = engine
        .open_with_progress(
            &book,
            (PAGE_WIDTH, PAGE_HEIGHT),
            &default_layout,
            Some(&mut |p| cold_progress.push(p)),
        )
        .expect("首次打开测试书失败");

    assert!(!doc.opened_from_cache(), "首次打开不应读缓存");
    assert!(doc.reformatted(), "首次打开必须排版");
    assert_monotonic_to_100(&cold_progress);
    assert!(
        cold_progress.len() > 1,
        "冷开应当有中间进度：{cold_progress:?}"
    );

    // 2. 第二次打开（相同参数 default_layout 打开，命中缓存且免重排）：
    // 命中缓存时不经过中间值，直接到 100
    let mut cache_hit_progress = Vec::new();
    let doc_cached = engine
        .open_with_progress(
            &book,
            (PAGE_WIDTH, PAGE_HEIGHT),
            &default_layout,
            Some(&mut |p| cache_hit_progress.push(p)),
        )
        .expect("第二次打开测试书失败");

    assert!(doc_cached.opened_from_cache(), "第二次打开应当从缓存读出");
    assert!(!doc_cached.reformatted(), "第二次打开必须命中缓存免重排");
    assert_eq!(
        cache_hit_progress,
        vec![100],
        "命中缓存免重排时不经过任何中间值，直接到 100"
    );

    // 3. 改大字号重新打开：DOM 读自缓存但排版参数不一致触发重排，进度单调不减到达 100
    let large_layout = LayoutSettings {
        font_size_px: 56,
        ..Default::default()
    };
    let mut cache_reformat_progress = Vec::new();
    let mut doc_reformat = engine
        .open_with_progress(
            &book,
            (PAGE_WIDTH, PAGE_HEIGHT),
            &large_layout,
            Some(&mut |p| cache_reformat_progress.push(p)),
        )
        .expect("改字号重开测试书失败");

    assert!(
        doc_reformat.opened_from_cache(),
        "DOM 应当读自缓存（省去文件解析）"
    );
    assert!(doc_reformat.reformatted(), "参数改变应当触发重排");
    assert_monotonic_to_100(&cache_reformat_progress);
    assert!(
        cache_reformat_progress.len() > 1,
        "重排应当有中间进度：{cache_reformat_progress:?}"
    );

    // 4. 在已有文档上调用 set_layout_with_progress：
    // 进度单调不减到达 100，且包含中间值
    let mut restyle_progress = Vec::new();
    doc_reformat
        .set_layout_with_progress(&default_layout, Some(&mut |p| restyle_progress.push(p)))
        .expect("改回原字号重排失败");
    assert_monotonic_to_100(&restyle_progress);
    assert!(
        restyle_progress.len() > 1,
        "改字号重排应当有中间进度：{restyle_progress:?}"
    );
}
