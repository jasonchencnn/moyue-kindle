//! 运行时改页面尺寸（viewport）的行为测试。
//!
//! 需要 `--features crengine`，并且要有能编译 C++ 引擎的工具链。
//! 跑法见 `scripts/check-engine.sh`。
//!
//! 覆盖的场景：竖屏 → 横屏 → 竖屏（宽高互换，像素数一样）、同尺寸重复改、
//! 连续多次改尺寸、只改字号、开着局部重排时改尺寸、阅读锚点（XPointer）
//! 在改尺寸后还能不能定位、非法尺寸被拒后文档是不是照旧可用。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Document, Engine, LayoutSettings, MAX_PAGE_DIMENSION_PX};
use inkread_platform::GrayBuffer;

/// 竖屏尺寸（Kindle Paperwhite 4）。
const PORTRAIT: (u32, u32) = (1072, 1448);

/// 横屏尺寸：与竖屏像素数相同、宽高互换。宽高对调是错行最容易发生的地方。
const LANDSCAPE: (u32, u32) = (1448, 1072);

/// 逐像素比对时允许的灰度差。
const PIXEL_TOLERANCE: u8 = 8;

/// 允许有差别的像素占比上限（百分比）。
const MAX_DIFF_PERCENT: f64 = 0.5;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path() -> PathBuf {
    manifest_dir().join("fixtures/sample.epub")
}

fn engine() -> CrengineEngine {
    CrengineEngine::with_default_fonts().expect("初始化 crengine 引擎失败")
}

fn open_default(engine: &CrengineEngine) -> Box<dyn Document> {
    engine
        .open(&fixture_path(), PORTRAIT, &LayoutSettings::default())
        .expect("打开测试书失败")
}

/// 两个画面有多少比例的像素不一样。
fn diff_percent(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "两个画面像素数不同");
    let diff = a
        .iter()
        .zip(b.iter())
        .filter(|(x, y)| x.abs_diff(**y) > PIXEL_TOLERANCE)
        .count();
    diff as f64 * 100.0 / a.len() as f64
}

/// 渲染第 0 页。
fn render_first_page(doc: &mut dyn Document, size: (u32, u32)) -> Vec<u8> {
    let mut buffer = GrayBuffer::new(size.0, size.1);
    doc.render_page(0, &mut buffer).expect("渲染第 0 页失败");
    buffer.pixels().to_vec()
}

#[test]
fn changing_the_page_size_re_paginates_and_renders_at_the_new_size() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let portrait_pages = doc.page_count();
    assert!(portrait_pages > 0, "测试书应当至少一页");

    doc.set_layout_with_size(&LayoutSettings::default(), LANDSCAPE)
        .expect("改成横屏尺寸失败");

    // 新的尺寸下能渲染，而且用的是新尺寸的缓冲。
    let landscape = render_first_page(doc.as_mut(), LANDSCAPE);
    assert_eq!(landscape.len(), (LANDSCAPE.0 * LANDSCAPE.1) as usize);

    // 竖屏的缓冲这时必须被挡住：缓冲尺寸与页面尺寸对不上。
    let mut wrong = GrayBuffer::new(PORTRAIT.0, PORTRAIT.1);
    let err = doc
        .render_page(0, &mut wrong)
        .expect_err("旧尺寸的缓冲应当被挡住");
    assert!(
        err.to_string().contains("缓冲区尺寸与页面尺寸不符"),
        "错误信息应当说明尺寸不符，实际：{err}"
    );

    // 横屏的行宽与缓冲行宽一致：画面不是错行的。
    let mut buffer = GrayBuffer::new(LANDSCAPE.0, LANDSCAPE.1);
    doc.render_page(0, &mut buffer).expect("按横屏尺寸渲染失败");
    assert!(buffer.pixels().iter().any(|p| *p < 128), "页面上应当有文字");
}

#[test]
fn the_same_page_count_survives_a_resize_back_and_forth() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let portrait_pages = doc.page_count();
    let portrait = render_first_page(doc.as_mut(), PORTRAIT);

    doc.set_layout_with_size(&LayoutSettings::default(), LANDSCAPE)
        .expect("改成横屏尺寸失败");
    let landscape = render_first_page(doc.as_mut(), LANDSCAPE);
    assert!(
        diff_percent(&portrait, &landscape) > 0.0,
        "横屏的排版应当与竖屏不同"
    );

    // 切回竖屏：像素数一样、宽高互换，这一步最容易错行。
    doc.set_layout_with_size(&LayoutSettings::default(), PORTRAIT)
        .expect("切回竖屏尺寸失败");
    let portrait_again = render_first_page(doc.as_mut(), PORTRAIT);
    assert_eq!(
        doc.page_count(),
        portrait_pages,
        "切回原尺寸后页数应当与原来一致"
    );
    let diff = diff_percent(&portrait, &portrait_again);
    assert!(
        diff <= MAX_DIFF_PERCENT,
        "切回原尺寸后的画面应当与原来一致，差异 {diff:.3}%（上限 {MAX_DIFF_PERCENT}%）"
    );
}

#[test]
fn the_same_size_can_be_applied_again() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let pages = doc.page_count();

    doc.set_layout_with_size(&LayoutSettings::default(), PORTRAIT)
        .expect("按同样的尺寸重排失败");
    assert_eq!(doc.page_count(), pages, "同样的尺寸不该改变页数");

    // 同样的尺寸重排之后，照旧能渲染。
    let pixels = render_first_page(doc.as_mut(), PORTRAIT);
    assert!(pixels.iter().any(|p| *p < 128), "页面上应当有文字");
}

#[test]
fn several_resizes_in_a_row_keep_the_document_consistent() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let sizes = [LANDSCAPE, (1264, 1680), (800, 600), PORTRAIT];

    for size in sizes {
        doc.set_layout_with_size(&LayoutSettings::default(), size)
            .unwrap_or_else(|e| panic!("改成 {size:?} 失败：{e}"));
        assert!(doc.page_count() > 0, "{size:?} 下应当至少一页");
        // 每个尺寸下都能用对应尺寸的缓冲渲染出来。
        let mut buffer = GrayBuffer::new(size.0, size.1);
        doc.render_page(0, &mut buffer)
            .unwrap_or_else(|e| panic!("{size:?} 下渲染失败：{e}"));
        assert_eq!(buffer.pixels().len(), (size.0 * size.1) as usize);
    }
}

#[test]
fn a_font_only_change_keeps_the_page_size() {
    let engine = engine();
    let mut doc = open_default(&engine);

    let bigger = LayoutSettings {
        font_size_px: inkread_engine::MAX_FONT_SIZE_PX,
        ..LayoutSettings::default()
    };
    // 旧接口：只改排版参数，尺寸不动。
    doc.set_layout(&bigger).expect("只改字号重新排版失败");

    // 尺寸没变，缓冲尺寸还是竖屏的。
    let mut buffer = GrayBuffer::new(PORTRAIT.0, PORTRAIT.1);
    doc.render_page(0, &mut buffer).expect("按原尺寸渲染失败");

    // 字号变了但尺寸没变：走旧接口，页数应当变多。
    let mut doc2 = open_default(&engine);
    let small = doc2.page_count();
    doc2.set_layout(&bigger).expect("只改字号重新排版失败");
    assert!(doc2.page_count() > small, "字号调大后页数应该变多");
}

#[test]
fn an_invalid_size_is_rejected_and_the_document_stays_usable() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let pages = doc.page_count();

    for size in [
        (0, 1448),
        (1072, 0),
        (0, 0),
        (79, 1448),
        (1072, 79),
        (inkread_engine::MIN_PAGE_DIMENSION_PX - 1, 1448),
    ] {
        let err = doc
            .set_layout_with_size(&LayoutSettings::default(), size)
            .expect_err("非法尺寸应当被拒绝");
        assert!(
            err.to_string().contains("页面尺寸"),
            "错误信息应当说明页面尺寸非法，实际：{err}"
        );
    }

    // 超大尺寸：Kindle 是 32 位 ARM，size_t 的乘法会回绕。
    let err = doc
        .set_layout_with_size(
            &LayoutSettings::default(),
            (MAX_PAGE_DIMENSION_PX + 1, 1448),
        )
        .expect_err("超大尺寸应当被拒绝");
    assert!(
        err.to_string().contains("页面尺寸"),
        "错误信息应当说明页面尺寸非法，实际：{err}"
    );

    // 被拒之后文档照旧可用：页数没变，原尺寸照样渲染。
    assert_eq!(doc.page_count(), pages, "被拒后页数不该变");
    let pixels = render_first_page(doc.as_mut(), PORTRAIT);
    assert!(pixels.iter().any(|p| *p < 128), "被拒后原尺寸应当照旧渲染");
}

#[test]
fn a_reading_anchor_survives_a_resize() {
    let engine = engine();
    let mut doc = open_default(&engine);

    // 改尺寸之前拿两个阅读锚点：第 0 页和第 1 页的起始位置。
    let first_anchor = doc.page_xpointer(0).expect("取第 0 页 XPointer 失败");
    let anchor = doc.page_xpointer(1).expect("取第 1 页 XPointer 失败");
    assert!(!anchor.is_empty(), "XPointer 不应是空串");

    doc.set_layout_with_size(&LayoutSettings::default(), LANDSCAPE)
        .expect("改成横屏尺寸失败");

    // 锚点要定位到真正包含它的那一页，而不只是「某个合法页码」：
    // 把定位到的页的起始 XPointer 再定位一次，必须回到同一页。
    let page = doc
        .page_of_xpointer(&anchor)
        .expect("改尺寸后锚点应当仍然定位得到");
    assert!(
        page < doc.page_count(),
        "锚点定位到的页码 {page} 应当在新页数 {} 以内",
        doc.page_count()
    );
    let page_start = doc
        .page_xpointer(page)
        .expect("取定位到的页的起始 XPointer 失败");
    assert_eq!(
        doc.page_of_xpointer(&page_start)
            .expect("页起始 XPointer 应当定位得到"),
        page,
        "页起始 XPointer 应当定位回同一页"
    );

    // 文档顺序不能因为改尺寸反过来：第 0 页的锚点不能跑到第 1 页锚点后面。
    let first_page = doc
        .page_of_xpointer(&first_anchor)
        .expect("第 0 页锚点应当仍然定位得到");
    assert!(
        first_page <= page,
        "第 0 页锚点定位到 {first_page} 页，却排在第 1 页锚点的 {page} 页之后"
    );

    // 切回竖屏：分页回到原样，同一个锚点应当回到原来的第 1 页。
    doc.set_layout_with_size(&LayoutSettings::default(), PORTRAIT)
        .expect("切回竖屏尺寸失败");
    assert_eq!(
        doc.page_of_xpointer(&anchor)
            .expect("切回后锚点应当定位得到"),
        1,
        "切回原尺寸后锚点应当回到原来的页码"
    );
}

#[test]
fn a_failed_resize_restores_the_page_geometry() {
    let engine = engine();
    let mut doc = open_default(&engine);
    let pages = doc.page_count();
    let portrait = render_first_page(doc.as_mut(), PORTRAIT);

    // 这套样式让文档排不出任何页：Render() 之后页数为 0，走的就是
    // 「改动已经发生、重排却失败」的回滚路径。
    let broken = LayoutSettings {
        user_css: Some("html, body { display: none !important; }".to_string()),
        ..LayoutSettings::default()
    };
    let err = doc
        .set_layout_with_size(&broken, LANDSCAPE)
        .expect_err("排不出页的尺寸变更应当报错");
    assert!(
        err.to_string().contains("no pages") || err.to_string().contains("页"),
        "错误信息应当说明重排失败，实际：{err}"
    );

    // 回滚之后不用再手动修一遍：文档应当已经回到失败前的样子
    // （竖屏尺寸 + 上一套排版参数），可以直接接着读。
    // 宽高没回滚的话，render_page 的尺寸守卫会报「缓冲区尺寸与页面尺寸不符」；
    // 排版参数没回滚的话，画面就不是原来那一张。
    assert_eq!(doc.page_count(), pages, "回滚后页数应当与原来一致");
    let pixels = render_first_page(doc.as_mut(), PORTRAIT);
    let diff = diff_percent(&portrait, &pixels);
    assert!(
        diff <= MAX_DIFF_PERCENT,
        "回滚后的画面应当与失败前一致，差异 {diff:.3}%（上限 {MAX_DIFF_PERCENT}%）"
    );
}

#[test]
fn a_resize_with_partial_rerendering_enabled_reflows_the_whole_book() {
    // 局部重排要文档缓存开着、且书里有 2 个以上分片。缓存是进程级的，
    // 这个测试文件里只有这一个用例需要它。
    let cache_dir = std::env::temp_dir().join(format!(
        "inkread-engine-viewport-partial-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&cache_dir);
    std::fs::create_dir_all(&cache_dir).expect("建临时缓存目录失败");
    let _ = inkread_engine::crengine::configure_cache(&cache_dir, 64 * 1024 * 1024);

    let engine = engine();
    let mut doc = open_default(&engine);

    // 局部重排要文档缓存开着、且书里有 2 个以上分片。
    if !doc.can_partial_rerender() {
        eprintln!("跳过：这本测试书不支持局部重排");
        return;
    }
    doc.enable_partial_rerender(true).expect("开启局部重排失败");
    assert!(doc.is_partial_rerender_enabled());

    doc.set_layout_with_size(&LayoutSettings::default(), LANDSCAPE)
        .expect("开着局部重排时改尺寸失败");

    // 改尺寸走的是整本重排：不能停在延迟重排状态里。
    assert!(
        !doc.is_rerender_delayed(),
        "改尺寸后应当已经整本排完，不该还停在延迟重排状态"
    );
    assert!(doc.page_count() > 0);

    // 补完全书排版之后，锚点仍然定位得到。
    doc.catchup_render().expect("补完全书排版失败");
    let anchor = doc.page_xpointer(0).expect("取 XPointer 失败");
    let page = doc
        .page_of_xpointer(&anchor)
        .expect("补排后锚点应当仍然定位得到");
    assert!(page < doc.page_count());

    // 局部重排照旧开着。
    assert!(doc.is_partial_rerender_enabled());

    // 同尺寸再改一次：不动局部重排，也不该把文档弄坏（纯改排版语义）。
    doc.set_layout_with_size(&LayoutSettings::default(), LANDSCAPE)
        .expect("同尺寸重排失败");
    assert!(
        doc.is_partial_rerender_enabled(),
        "同尺寸重排不该关掉局部重排"
    );
    assert!(doc.page_count() > 0);

    // 局部重排开着时改尺寸失败：恢复也必须把旧布局整本排回来，否则
    // 文档停在「0 页/延迟重排」上，调用方没法直接接着读。
    let pages_before = doc.page_count();
    let landscape_before = render_first_page(doc.as_mut(), LANDSCAPE);
    let broken = LayoutSettings {
        user_css: Some("html, body { display: none !important; }".to_string()),
        ..LayoutSettings::default()
    };
    doc.set_layout_with_size(&broken, PORTRAIT)
        .expect_err("排不出页的尺寸变更应当报错");
    assert_eq!(doc.page_count(), pages_before, "恢复后页数应当与原来一致");
    assert!(
        doc.is_partial_rerender_enabled(),
        "恢复后局部重排应当还是开着"
    );
    assert!(
        !doc.is_rerender_delayed(),
        "恢复应当整本排完，不能停在延迟重排状态"
    );
    let landscape_after = render_first_page(doc.as_mut(), LANDSCAPE);
    let diff = diff_percent(&landscape_before, &landscape_after);
    assert!(
        diff <= MAX_DIFF_PERCENT,
        "恢复后的画面应当与失败前一致，差异 {diff:.3}%（上限 {MAX_DIFF_PERCENT}%）"
    );
}

#[test]
fn the_progress_callback_of_a_resize_is_monotonic_and_reaches_the_end() {
    let engine = engine();
    let mut doc = open_default(&engine);

    let mut progress: Vec<u32> = Vec::new();
    {
        let mut cb = |value: u32| progress.push(value);
        doc.set_layout_with_size_and_progress(&LayoutSettings::default(), LANDSCAPE, Some(&mut cb))
            .expect("带进度回调改尺寸失败");
    }

    assert!(!progress.is_empty(), "进度回调应当被调用过");
    assert_eq!(
        progress.last().copied(),
        Some(100),
        "进度应当走到 100，实际：{progress:?}"
    );
    for pair in progress.windows(2) {
        assert!(pair[1] >= pair[0], "进度应当单调不减，实际：{progress:?}");
    }
}
