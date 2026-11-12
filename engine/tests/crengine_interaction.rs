//! ENG-004：目录、词矩形、链接的实测。
//!
//! 需要 `--features crengine`，并且要有能编译 C++ 引擎的工具链。
//! 跑法见 `scripts/check-engine.sh`。
//!
//! 测试书是 `fixtures/sample.epub`（由 `fixtures/make_sample_epub.py` 生成）：
//! 三章正文、一个嵌在第三章里的注释（目录里是第二层），注释段落前面有
//! 一个书内锚点链接和一个书外 URL 链接。

#![cfg(feature = "crengine")]

use std::path::PathBuf;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Document, Engine, LayoutSettings, LinkTarget, Rect};
use inkread_platform::GrayBuffer;

/// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

/// 黄金图与墨迹检查都依赖具体字体，字体不在就跳过这一项。
const CJK_FONT_FACE: &str = "Songti SC";

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
        .open(
            &fixture_path(),
            (PAGE_WIDTH, PAGE_HEIGHT),
            &LayoutSettings::default(),
        )
        .expect("打开测试书失败")
}

/// 矩形里有没有墨迹（灰度小于 250 就算有）。
fn has_ink(buffer: &GrayBuffer, rect: &Rect) -> bool {
    for y in rect.y.max(0)..rect.bottom().min(PAGE_HEIGHT as i32) {
        for x in rect.x.max(0)..rect.right().min(PAGE_WIDTH as i32) {
            if buffer.get_pixel(x, y).map(|p| p < 250).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// 是不是一个汉字。
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

#[test]
fn toc_lists_titles_levels_and_pages() {
    let engine = engine();
    let doc = open_default(&engine);
    let toc = doc.toc().expect("取目录失败");

    let titles: Vec<&str> = toc.iter().map(|entry| entry.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "中文段落",
            "English paragraph",
            "第三章 · 分页检查",
            "第三章 · 注释"
        ],
        "目录标题应当与书里的目录一致"
    );

    let levels: Vec<u32> = toc.iter().map(|entry| entry.level).collect();
    assert_eq!(
        levels,
        [0, 0, 0, 1],
        "层级应当从 0 起，嵌在第三章里的注释是第 1 层"
    );

    for pair in toc.windows(2) {
        assert!(
            pair[0].page <= pair[1].page,
            "目录页码应当单调不减：{} -> {}",
            pair[0].page,
            pair[1].page
        );
    }

    // 用目录项的 XPointer 定位回去，页码要一致。
    for entry in &toc {
        let page = doc
            .page_of_xpointer(&entry.xpointer)
            .expect("按 XPointer 定位页码失败");
        assert_eq!(
            page, entry.page,
            "目录项 {:?} 的 XPointer 定位到了第 {page} 页，应当是第 {} 页",
            entry.title, entry.page
        );
    }
}

#[test]
fn chapter_of_page_follows_the_section_boundaries() {
    let engine = engine();
    let doc = open_default(&engine);
    let toc = doc.toc().expect("取目录失败");
    assert!(toc.len() >= 2, "测试书应当有多个目录项");

    // 每一节的首页都落在自己身上。
    for (index, entry) in toc.iter().enumerate() {
        assert_eq!(
            doc.chapter_of_page(entry.page).expect("查章节失败"),
            Some(index),
            "第 {} 页应当属于目录项 {index}（{}）",
            entry.page,
            entry.title
        );
    }

    // 一节的最后一页仍属于这一节，下一节的首页才换过去。
    for index in 0..toc.len() - 1 {
        let next_page = toc[index + 1].page;
        if next_page > toc[index].page {
            assert_eq!(
                doc.chapter_of_page(next_page - 1).expect("查章节失败"),
                Some(index),
                "第 {} 页应当还属于目录项 {index}（{}）",
                next_page - 1,
                toc[index].title
            );
        }
    }

    // 最后一节目录项管到书的最后一页。
    let last_page = doc.page_count() - 1;
    assert_eq!(
        doc.chapter_of_page(last_page).expect("查章节失败"),
        Some(toc.len() - 1),
        "最后一页应当属于最后一个目录项"
    );
}

#[test]
fn word_rects_are_inside_the_page_and_never_overlap() {
    let engine = engine();
    let doc = open_default(&engine);

    for page in 0..doc.page_count() {
        let words = doc.page_words(page).expect("取词失败");
        assert!(!words.is_empty(), "第 {page} 页应当有词");

        let mut rects: Vec<Rect> = Vec::new();
        for word in &words {
            assert!(!word.rects.is_empty(), "词 {:?} 应当有矩形", word.text);
            for rect in &word.rects {
                assert!(
                    rect.x >= 0 && rect.y >= 0,
                    "词 {:?} 的矩形 {:?} 越界",
                    word.text,
                    rect
                );
                assert!(
                    rect.right() <= PAGE_WIDTH as i32,
                    "词 {:?} 的矩形 {:?} 超出页面宽度",
                    word.text,
                    rect
                );
                assert!(
                    rect.bottom() <= PAGE_HEIGHT as i32,
                    "词 {:?} 的矩形 {:?} 超出页面高度",
                    word.text,
                    rect
                );
                rects.push(*rect);
            }
        }

        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                assert!(
                    !rects[i].intersects(&rects[j]),
                    "第 {page} 页的词矩形重叠：{:?} 与 {:?}",
                    rects[i],
                    rects[j]
                );
            }
        }
    }
}

#[test]
fn words_are_split_by_the_engine_rules() {
    let engine = engine();
    let doc = open_default(&engine);

    // 第 0 页是中文：汉字一个字一个词。
    let words = doc.page_words(0).expect("取词失败");
    let hanzi: Vec<&str> = words
        .iter()
        .filter(|word| word.text.chars().next().map(is_cjk).unwrap_or(false))
        .map(|word| word.text.as_str())
        .collect();
    assert!(
        hanzi.len() >= 10,
        "中文页应当能拿到一堆单字词，实际 {} 个：{:?}",
        hanzi.len(),
        &hanzi[..hanzi.len().min(10)]
    );
    for word in &hanzi {
        assert_eq!(
            word.chars().count(),
            1,
            "汉字应当一个字一个词，实际 {:?}",
            word
        );
    }

    // 第 1 页是英文：按空格断词。
    let words = doc.page_words(1).expect("取词失败");
    let latin: Vec<&str> = words
        .iter()
        .map(|word| word.text.as_str())
        .filter(|text| text.chars().all(|c| c.is_ascii_alphabetic()))
        .collect();
    assert!(
        latin.contains(&"display"),
        "英文页应当按空格断词，实际 {:?}",
        &latin[..latin.len().min(10)]
    );
}

#[test]
fn word_rects_land_on_the_rendered_ink() {
    let engine = engine();
    if !engine
        .font_faces()
        .iter()
        .any(|face| face.as_str() == CJK_FONT_FACE)
    {
        eprintln!("跳过墨迹比对：没有 {CJK_FONT_FACE} 字体");
        return;
    }

    let mut doc = open_default(&engine);
    for page in 0..doc.page_count() {
        let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
        doc.render_page(page, &mut buffer).expect("渲染失败");
        let words = doc.page_words(page).expect("取词失败");
        for word in &words {
            for rect in &word.rects {
                assert!(
                    has_ink(&buffer, rect),
                    "第 {page} 页的词 {:?} 的矩形 {:?} 里没有墨迹",
                    word.text,
                    rect
                );
            }
        }
    }
}

#[test]
fn links_resolve_internal_and_external_targets() {
    let engine = engine();
    let doc = open_default(&engine);
    let toc = doc.toc().expect("取目录失败");

    let mut internal = None;
    let mut external = None;
    for page in 0..doc.page_count() {
        for link in doc.page_links(page).expect("取链接失败") {
            for rect in &link.rects {
                assert!(rect.x >= 0 && rect.y >= 0, "链接矩形 {:?} 越界", rect);
                assert!(
                    rect.right() <= PAGE_WIDTH as i32 && rect.bottom() <= PAGE_HEIGHT as i32,
                    "链接矩形 {:?} 超出页面",
                    rect
                );
            }
            match &link.target {
                LinkTarget::Internal {
                    xpointer,
                    page: target,
                } => {
                    assert!(!xpointer.is_empty(), "书内链接应当给出目标 XPointer");
                    let target = target.expect("书内链接应当能定位到目标页");
                    assert_eq!(
                        doc.page_of_xpointer(xpointer)
                            .expect("按 XPointer 定位页码失败"),
                        target,
                        "书内链接的目标 XPointer 定位到的页码与给出的不一致"
                    );
                    internal = Some((page, link.clone(), target));
                }
                LinkTarget::External { url } => {
                    assert!(
                        url.starts_with("https://"),
                        "外部链接应当是 URL，实际 {url}"
                    );
                    external = Some((page, link.clone()));
                }
            }
        }
    }

    let (internal_page, internal_link, internal_target) =
        internal.expect("测试书应当有一个书内链接");
    assert!(
        internal_link
            .rects
            .iter()
            .any(|rect| rect.width > 0 && rect.height > 0),
        "书内链接应当有矩形"
    );

    let (external_page, external_link) = external.expect("测试书应当有一个外部链接");
    assert_eq!(external_link.href, "https://example.com/inkread");
    assert!(external_page < doc.page_count());

    // 书内链接指向的就是目录里那条「注释」，两边的页码要一致。
    let note = toc
        .iter()
        .find(|entry| entry.title.contains("注释"))
        .expect("目录里应当有注释这一节");
    assert_eq!(
        note.page, internal_target,
        "书内链接的目标页与目录里注释的页码应当一致"
    );
    assert!(internal_page < note.page, "链接所在的页应当在目标之前");
}
