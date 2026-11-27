// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crengine path of the renderer, on a real book.
//!
//! The book is 《紅樓夢》, the one the reader tests use; it and the fonts of
//! the device package live in the cache, not in the repository. The tests
//! skip themselves when they are not there, so the suite still runs on a
//! machine without them.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use aristotle_library::{Locator, Progress};

use crate::book::{Book, Engine, Mark, Position};
use crate::crengine::CrengineDoc;
use crate::locator;
use crate::page::Chrome;
use crate::style::Style;

/// crengine keeps 16 process-wide document slots, so the tests that open
/// books run one at a time. Without this the default parallel harness can
/// hold 15 or more documents and catch SIGSEGV (QA-101 §5).
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

/// The test book, in the cache.
const BOOK: &str = "testbooks/hlm.epub";

/// The cache of the reader's tools, where the book and the device package
/// are.
fn cache() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let cache = PathBuf::from(home).join(".cache/aristotle");
    cache.join(BOOK).is_file().then_some(cache)
}

/// The data directory of the device package: the fonts the renderer uses.
fn data_dir(cache: &Path) -> Option<PathBuf> {
    let dir = cache.join("render/device/data");
    dir.join("serif/Literata-Regular.ttf")
        .is_file()
        .then_some(dir)
}

/// Make the device package's data directory the current one, as the reader
/// does on the device: the renderer finds its fonts relative to it.
fn use_data_dir(cache: &Path) -> Option<PathBuf> {
    let dir = data_dir(cache)?;
    std::env::set_current_dir(&dir).ok()?;
    Some(dir)
}

/// The test book, opened with crengine. `None` when the book or the fonts
/// are not there.
fn open_book(cache: &Path, style: &Style) -> Option<Book> {
    use_data_dir(cache)?;
    Book::open(&cache.join(BOOK), style, None).ok()
}

#[test]
fn a_saved_xpointer_opens_the_same_page() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let Some(book) = open_book(&cache, &style) else {
        return;
    };
    assert_eq!(book.engine(), Engine::Crengine, "EPUBs go to crengine");
    let page = 10.min(book.len().saturating_sub(1));

    // Where the reader is, as the renderer names it.
    let position = book.position_at(page);
    let Position::XPointer { ref xpointer } = position else {
        panic!("the position should be an XPointer, got {position:?}");
    };
    assert!(!xpointer.is_empty());

    // Saved in the library index...
    let saved = locator::locator(&position);
    assert!(
        matches!(saved, Locator::XPointer { .. }),
        "the progress should be an XPointer locator, got {saved:?}"
    );
    let progress = Progress {
        fraction: 0.5,
        locator: saved,
        updated: 1,
        source: Some("aristotle".into()),
        page: None,
    };

    // ...and read back.
    let positions = locator::positions(Engine::Crengine, Some(&progress), false);
    let back = positions.iter().find_map(|p| book.location_of(p));
    assert_eq!(back, Some(page), "the same page should come back");
}

#[test]
fn the_same_xpointer_finds_the_same_text_after_a_font_change() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    use_data_dir(&cache);
    let path = cache.join(BOOK);
    let mut style = Style::pw4();

    let Some(small) = CrengineDoc::open(&path, &style).ok() else {
        return;
    };
    // A word a few pages into the book.
    let page = 5.min(small.page_count().saturating_sub(1));
    let Some(word) = small.page_words(page).first().cloned() else {
        return;
    };
    let xpointer = word.start_xpointer.clone();

    // The same book, set in a larger font.
    style.font_size = 14.0;
    let Some(large) = CrengineDoc::open(&path, &style).ok() else {
        return;
    };
    let Some(page) = large.page_of_xpointer(&xpointer) else {
        panic!("the XPointer should still point at a page");
    };
    // The word is on that page: the same text, wherever the new layout puts
    // it.
    let words = large.page_words(page);
    assert!(
        words.iter().any(|w| w.start_xpointer == xpointer),
        "the word should be on page {page}"
    );
}

#[test]
fn an_old_spine_progress_opens_the_book_by_fraction() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let Some(book) = open_book(&cache, &style) else {
        return;
    };
    // A progress Plato's engine wrote: a spine offset crengine-ng knows
    // nothing about.
    let old = Progress {
        fraction: 0.25,
        locator: Locator::Epub {
            spine: 3,
            offset: 1200,
        },
        updated: 1,
        source: Some("aristotle".into()),
        page: None,
    };
    let positions = locator::positions(Engine::Crengine, Some(&old), false);
    assert_eq!(positions, vec![Position::Fraction(0.25)]);
    let page = positions
        .iter()
        .find_map(|p| book.location_of(p))
        .expect("the fraction should place the book");
    assert!(page > 0, "a quarter into the book is not the first page");
    assert!(page < book.len(), "and not past the end");
}

#[test]
fn an_xpointer_that_cannot_be_located_opens_the_book_by_fraction() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let Some(book) = open_book(&cache, &style) else {
        return;
    };
    // A progress whose XPointer points nowhere: the book was replaced by a
    // newer edition, say. The engine says it cannot place it, and the reader
    // falls back to the fraction instead of jumping to the first page.
    let stale = Progress {
        fraction: 0.25,
        locator: Locator::XPointer {
            xpointer: "/body/DocFragment[9999]/body/div[1]/p[1]/text().0".into(),
        },
        updated: 1,
        source: Some("aristotle".into()),
        page: None,
    };
    let positions = locator::positions(Engine::Crengine, Some(&stale), false);
    assert_eq!(positions.len(), 2, "the XPointer and the fraction");
    assert_eq!(
        book.location_of(&positions[0]),
        None,
        "the XPointer points nowhere and cannot be placed"
    );

    // So the fraction takes over.
    let page = positions
        .iter()
        .find_map(|p| book.location_of(p))
        .expect("the fraction should place the book");
    assert!(page > 0, "a quarter into the book is not the first page");
    assert!(page < book.len(), "and not past the end");
}

#[test]
fn the_table_of_contents_is_the_engines_own() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let Some(mut book) = open_book(&cache, &style) else {
        return;
    };
    let toc = book.crengine_toc().to_vec();
    assert!(!toc.is_empty(), "the book has a table of contents");

    // The same entries, in the same order, with the same titles.
    let items = book.toc_items();
    assert_eq!(items.len(), toc.len());
    for (item, entry) in items.iter().zip(toc.iter()) {
        assert_eq!(item.title, entry.title.trim());
        assert_eq!(item.depth, entry.level as usize);
        assert_eq!(item.page, Some(entry.page as usize));
    }

    // The chapter name in the footer is the title of the entry the page
    // belongs to.
    let page = toc[0].page as usize;
    let index = crate::crengine::chapter_of_page(&toc, page as u32)
        .expect("the first entry covers its own page");
    assert_eq!(
        book.chapter_title(page).as_deref(),
        Some(toc[index].title.trim()),
        "the footer should name the chapter"
    );
}

#[test]
fn test_integration_partial_rerender_turn_and_catchup_hlm() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let _ = crate::crengine::configure_cache(&cache.join("crengine-cache"));
    let mut style = Style::pw4();
    style.font_size = 10.0;

    let Some(mut book) = open_book(&cache, &style) else {
        return;
    };
    assert_eq!(book.engine(), Engine::Crengine);
    assert!(
        book.can_partial_rerender(),
        "hlm.epub with cache must support partial rerendering"
    );

    // 锚点页：第 5 页
    let anchor_pos = book.position_at(5);
    let Position::XPointer { ref xpointer } = anchor_pos else {
        panic!("anchor position must be XPointer");
    };
    assert!(!xpointer.is_empty());

    // 1. 改字号：10.0 pt -> 14.0 pt
    let mut new_style = style.clone();
    new_style.font_size = 14.0;

    book.enable_partial_rerender(true)
        .expect("enable partial rerender");
    book.set_layout(&new_style).expect("set layout");
    assert!(
        book.is_rerender_delayed(),
        "book must be in delayed layout state"
    );

    // 2. 锚点页可读：定位并渲染锚点所在页
    let anchor_page = book
        .location_of(&anchor_pos)
        .expect("anchor page must be found");
    let mut chrome = Chrome::load(&new_style).expect("chrome fonts must load");
    let page0 = book
        .render_location_with(anchor_page, &mut chrome, None)
        .expect("render anchor page");
    assert!(page0.width > 0 && page0.height > 0);

    // 3. 翻 3 页（在降级状态下连续翻页）
    let mut curr_page = anchor_page;
    for _step in 1..=3 {
        let next = book.next_page(curr_page).expect("must have next page");
        let turned_page = book
            .render_location_with(next, &mut chrome, None)
            .expect("render next page");
        assert!(turned_page.width > 0 && turned_page.height > 0);
        curr_page = next;
    }
    assert_eq!(curr_page, anchor_page + 3, "翻 3 页后页码应前进 3");

    // 4. 后台补全
    book.catchup_render().expect("catchup render must succeed");
    assert!(
        !book.is_rerender_delayed(),
        "must not be in delayed state after catchup"
    );

    // 5. 对照全量重排：打开新文档做对比
    let full_book = open_book(&cache, &new_style).expect("open full book for comparison");
    let partial_catchup_page = book
        .location_of(&anchor_pos)
        .expect("find anchor in partial book");
    let full_page = full_book
        .location_of(&anchor_pos)
        .expect("find anchor in full book");

    assert_eq!(
        partial_catchup_page, full_page,
        "补全后锚点 XPointer 所在页 ({partial_catchup_page}) 与全量重排结果 ({full_page}) 必须严格一致"
    );
    assert_eq!(
        book.len(),
        full_book.len(),
        "补全后总页数 ({}) 与全量重排 ({}) 必须严格一致",
        book.len(),
        full_book.len()
    );
    assert_eq!(
        book.position_at(partial_catchup_page),
        full_book.position_at(full_page),
        "补全后锚点页 XPointer 必须与全量重排严格一致"
    );
}

#[test]
fn test_degraded_toc_jump_matches_catchup_xpointer() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let _ = crate::crengine::configure_cache(&cache.join("crengine-cache"));
    let mut style = Style::pw4();
    style.font_size = 10.0;

    let Some(mut book) = open_book(&cache, &style) else {
        return;
    };
    assert_eq!(book.engine(), Engine::Crengine);
    assert!(book.can_partial_rerender());

    // 1. 改字号并进入降级延迟状态
    let mut new_style = style.clone();
    new_style.font_size = 14.0;
    book.enable_partial_rerender(true).expect("enable partial");
    book.set_layout(&new_style).expect("set layout");
    assert!(book.is_rerender_delayed());

    // 降级状态下，所有目录条目的页码列均不显示数字（page 字段为 None）
    let degraded_items = book.toc_items();
    assert!(!degraded_items.is_empty());
    for item in &degraded_items {
        assert_eq!(
            item.page, None,
            "降级状态下目录条目不应显示数字页码: {:?}",
            item.title
        );
    }

    // 2. 选择一个未排版的分片章节条目（例如最后一个目录项）
    let target_entry_idx = degraded_items.len() - 1;
    let target_toc_title = degraded_items[target_entry_idx].title.clone();

    // 3. 在降级状态下调用 toc_page 进行跳转，应当按 XPointer 定位并按需局部排版
    let jump_page = book
        .toc_page(target_entry_idx)
        .expect("降级状态下 toc_page 跳转应当成功");
    let jump_pos = book.position_at(jump_page);
    let Position::XPointer { ref xpointer } = jump_pos else {
        panic!("落点必须是 XPointer");
    };
    assert!(!xpointer.is_empty());

    // 4. 后台补全
    book.catchup_render().expect("catchup render");
    assert!(!book.is_rerender_delayed());

    // 补全后目录条目恢复显示页码数字
    let caught_up_items = book.toc_items();
    assert_eq!(caught_up_items.len(), degraded_items.len());
    for item in &caught_up_items {
        assert!(
            item.page.is_some(),
            "补全后目录条目应当具有准确页码: {:?}",
            item.title
        );
    }

    // 5. 补全后同一条目定位到的 XPointer 必须与降级跳转时的落点 XPointer 严格一致
    let catchup_page = book
        .toc_page(target_entry_idx)
        .expect("补全后 toc_page 应当成功");
    let catchup_pos = book.position_at(catchup_page);

    assert_eq!(
        jump_pos, catchup_pos,
        "降级状态跳转未排版章节 ({target_toc_title}) 的落点 XPointer 必须与补全后一致"
    );
}

const INTERACTIVE_BOOK: &str = "testbooks/interactive.epub";

fn open_interactive_book(cache: &Path, style: &Style) -> Option<Book> {
    use_data_dir(cache)?;
    Book::open(&cache.join(INTERACTIVE_BOOK), style, None).ok()
}

#[test]
fn test_interactive_epub_words_and_links_in_bounds_and_ordered() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");
    let (top, body_h) = style.body();

    let words = book.words(0);
    assert!(!words.is_empty(), "crengine 书第 0 页 words 应当非空");
    for w in &words {
        assert!(
            w.rect.y0 >= top && w.rect.y1 <= top + body_h as i32 + 5,
            "词矩形 y 应当落在版心内: {:?}, top={}, body_h={}",
            w.rect,
            top,
            body_h
        );
        assert!(
            w.rect.x0 >= 0 && w.rect.x1 <= style.width as i32 + 5,
            "词矩形 x 应当落在页面宽度内: {:?}",
            w.rect
        );
        assert!(w.start_xpointer.is_some(), "词应当携带 start_xpointer");
        assert!(w.end_xpointer.is_some(), "词应当携带 end_xpointer");
    }

    // 词偏移单调递增
    for pair in words.windows(2) {
        assert!(
            pair[0].offset < pair[1].offset,
            "词 offset 必须严格单调递增: {} < {}",
            pair[0].offset,
            pair[1].offset
        );
    }

    let links = book.links(0);
    assert!(!links.is_empty(), "crengine 书第 0 页 links 应当非空");
    for l in &links {
        assert!(
            l.rect.y0 >= top && l.rect.y1 <= top + body_h as i32 + 5,
            "链接矩形 y 应当落在版心内: {:?}",
            l.rect
        );
    }
}

#[test]
fn test_interactive_epub_link_page_resolves_internal_target() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");

    let links = book.links(0);
    let sec_link = links
        .iter()
        .find(|l| l.uri.contains("target-sec"))
        .expect("应当找到内链 target-sec");

    let target_page = book
        .link_page(0, &sec_link.uri)
        .expect("link_page 应成功返回书内锚点目标页");
    assert!(target_page > 0, "目标页应当在后续章节页面");

    let target_words = book.words(target_page);
    let target_text: String = target_words.iter().map(|w| w.text.as_str()).collect();
    assert!(
        target_text.contains("第二章锚点目标"),
        "目标页正文应当包含落点标题，实际正文: {target_text}"
    );

    // 外部链接 link_page 应当返回 None
    let ext_link = links
        .iter()
        .find(|l| l.uri.starts_with("https://"))
        .expect("应当找到外链");
    assert_eq!(
        book.link_page(0, &ext_link.uri),
        None,
        "外链 link_page 应当返回 None"
    );
}

#[test]
fn test_interactive_epub_footnote_retrieves_body() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");

    let links = book.links(0);
    let fn_link = links
        .iter()
        .find(|l| l.uri.contains("fn-1"))
        .expect("应当找到脚注链接 fn-1");

    let note = book
        .footnote(fn_link, "[1]")
        .expect("footnote 应成功提取脚注正文");
    let plain = note.text();
    assert!(
        plain.contains("这是第一条测试脚注的正文内容"),
        "提取的脚注内容应当与目标 aside 一致，实际: {plain}"
    );
}

#[test]
fn test_interactive_epub_page_footnotes_and_has_page_footnotes() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");

    assert!(
        book.has_page_footnotes(0),
        "含脚注的页面 has_page_footnotes 应返回 true"
    );
    let notes = book.page_footnotes(0);
    assert!(
        !notes.is_empty(),
        "page_footnotes 应提取出当前页的所有自带注释"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.text.text().contains("这是第一条测试脚注的正文内容")),
        "page_footnotes 应包含目标脚注正文，实际: {notes:?}"
    );
    // 同一 URI 不应重复列出
    let mut uris: Vec<&str> = notes.iter().map(|n| n.link.uri.as_str()).collect();
    uris.sort_unstable();
    let unique = {
        let mut u = uris.clone();
        u.dedup();
        u.len()
    };
    assert_eq!(uris.len(), unique, "page_footnotes 应按 URI 去重");
}

#[test]
fn test_interactive_epub_highlight_xpointer_locates_same_text_after_font_change() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let mut style = Style::pw4();
    style.font_size = 10.0;
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");

    let words = book.words(0);
    assert!(words.len() >= 20, "第 0 页应有足够的词用于选区测试");

    // 选取第 4 到第 12 个词
    let s_word = &words[4];
    let e_word = &words[12];
    let original_text: String = words[4..=12].iter().map(|w| w.text.as_str()).collect();

    let start_xp = s_word.start_xpointer.clone().unwrap();
    let end_xp = e_word.end_xpointer.clone().unwrap();

    let s_pos = Position::XPointer {
        xpointer: start_xp.clone(),
    };
    let e_pos = Position::XPointer {
        xpointer: end_xp.clone(),
    };

    let s_loc_orig = book.mark_start_location(&s_pos).expect("定位起始标记");
    let e_loc_orig = book.mark_end_location(&e_pos).expect("定位结束标记");
    assert_eq!(s_loc_orig, s_word.offset);
    assert_eq!(e_loc_orig, e_word.offset);

    // 改变字号重新排版
    let mut new_style = style.clone();
    new_style.font_size = 14.0;
    book.set_layout(&new_style).expect("重新排版");

    // 重新通过 XPointer 定位
    let (s_loc_new, e_loc_new) = book
        .mark_range(&s_pos, &e_pos)
        .expect("改字号后定位标记范围");

    let new_page = s_loc_new >> 16;
    let words_new = book.words(new_page);
    let marked_words: Vec<_> = words_new
        .into_iter()
        .filter(|w| w.offset >= s_loc_new && w.offset <= e_loc_new)
        .collect();
    let relocated_text: String = marked_words.iter().map(|w| w.text.as_str()).collect();

    assert_eq!(
        relocated_text, original_text,
        "改字号后高亮 XPointer 重新提取出的文本应当与原高亮完全一致"
    );
}

#[test]
fn test_interactive_epub_select_highlight_archive_and_reopen() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");

    let words = book.words(0);
    assert!(words.len() >= 20);

    // 1. 模拟长按选词：选中第 4 到第 12 个词
    let s_word = &words[4];
    let e_word = &words[12];
    let original_text: String = words[4..=12].iter().map(|w| w.text.as_str()).collect();

    let s_pos = Position::XPointer {
        xpointer: s_word.start_xpointer.clone().unwrap(),
    };
    let e_pos = Position::XPointer {
        xpointer: e_word.end_xpointer.clone().unwrap(),
    };

    // 2. 存档：按 KOReader 契约及 locator::encode 格式存入 Annotation
    let ann = aristotle_library::index::Annotation {
        id: aristotle_library::index::generate_annotation_id(),
        kind: aristotle_library::index::AnnotationKind::Highlight,
        text: Some(original_text.clone()),
        page: Some("1".to_string()),
        location: None,
        position: Some((locator::encode(&s_pos), locator::encode(&e_pos))),
        created: Some("2026-11-04 12:00:00".to_string()),
        modified: None,
        color: None,
        style: None,
        source: "aristotle".to_string(),
        chapter: book.chapter_title(0),
        note: None,
    };

    let temp_dir = std::env::temp_dir().join(format!("app003-ann-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let index_file = temp_dir.join("index.json");

    let mut index = aristotle_library::index::Index::default();
    let record = aristotle_library::index::BookRecord {
        id: "test-interactive".to_string(),
        file: aristotle_library::index::FileStamp {
            path: "testbooks/interactive.epub".to_string(),
            size: 1000,
            mtime: 0,
            mtime_ns: 0,
        },
        format: aristotle_library::Format::Epub,
        meta: Default::default(),
        cover: None,
        added: 0,
        last_opened: None,
        status: Default::default(),
        progress: None,
        settings: Default::default(),
        annotations: vec![ann],
        annotation_tombstones: Vec::new(),
    };
    index.books.push(record);
    index.save(&index_file).expect("保存索引");

    // 3. 重开书：从磁盘加载索引，重构高亮
    let loaded_index = aristotle_library::index::Index::load(&index_file).expect("读取索引");
    let loaded_ann = &loaded_index.books[0].annotations[0];
    let (s_enc, e_enc) = loaded_ann.position.as_ref().unwrap();
    let decoded_s = locator::decode(s_enc).expect("解码起始位置");
    let decoded_e = locator::decode(e_enc).expect("解码结束位置");

    let mut reopened_book = open_interactive_book(&cache, &style).expect("重开 interactive.epub");
    let (s_loc, e_loc) = reopened_book
        .mark_range(&decoded_s, &decoded_e)
        .expect("重开后定位标记范围");

    let reopened_page = s_loc >> 16;
    let reopened_words = reopened_book.words(reopened_page);
    let marked_words: Vec<_> = reopened_words
        .into_iter()
        .filter(|w| w.offset >= s_loc && w.offset <= e_loc)
        .collect();
    let restored_text: String = marked_words.iter().map(|w| w.text.as_str()).collect();

    assert_eq!(
        restored_text, original_text,
        "重开后高亮文字必须与初次长按选中的完全一致"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn generate_app003_host_screenshots() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    // Open the book first: it points the renderer at the package's data
    // directory, which the chrome fonts are loaded from.
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");
    let mut chrome = Chrome::load(&style).expect("chrome fonts must load");
    let words = book.words(0);

    let out_dir = std::env::temp_dir().join("app003-screenshots");
    std::fs::create_dir_all(&out_dir).unwrap();

    // 1. 高亮页面：取第 4 到第 12 个词并绘制高亮底色
    let mut page_hl = book.render_page(0, &mut chrome).expect("渲染第 0 页");
    let marked_words = &words[4..=12];
    for w in marked_words {
        let (w_w, h_h) = (page_hl.width as i32, page_hl.height as i32);
        let x0 = w.rect.x0.clamp(0, w_w) as usize;
        let x1 = w.rect.x1.clamp(0, w_w) as usize;
        let y0 = (w.rect.y0 - 2).clamp(0, h_h) as usize;
        let y1 = (w.rect.y1 + 2).clamp(0, h_h) as usize;
        for y in y0..y1 {
            let row = y * page_hl.stride;
            for p in &mut page_hl.data[row + x0..row + x1] {
                *p = (*p as u16 * 190 / 255) as u8;
            }
        }
    }
    page_hl.write_raw(&out_dir.join("highlight.raw")).unwrap();

    // 2. 夜间模式页面：图片在第 1 页（第 0 页只有文字）。日间版本一并保存，
    //    用来对照图片在整页反色后是否回到日间的颜色。
    let page_day = book
        .render_page(1, &mut chrome)
        .expect("渲染第 1 页（日间）");
    page_day.write_raw(&out_dir.join("day.raw")).unwrap();
    book.set_night(true);
    let mut page_night = book
        .render_page(1, &mut chrome)
        .expect("渲染第 1 页（夜间图片反色）");
    // 整页反色
    for p in &mut page_night.data {
        *p = 255 - *p;
    }
    page_night.write_raw(&out_dir.join("night.raw")).unwrap();
}

#[test]
fn test_resolve_marks_does_not_call_page_words() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");

    let initial_calls = book.page_words_calls();
    assert_eq!(initial_calls, 0, "开书时不应调用 page_words");

    // 模拟 20 条高亮，覆盖各个段落
    let marks: Vec<(Position, Position)> = (0..20)
        .map(|i| {
            let start = Position::XPointer {
                xpointer: format!("/body/DocFragment[1]/body/p[{}]/text().0", (i % 3) + 1),
            };
            let end = Position::XPointer {
                xpointer: format!("/body/DocFragment[1]/body/p[{}]/text().10", (i % 3) + 1),
            };
            (start, end)
        })
        .collect();

    for (i, (s, e)) in marks.iter().enumerate() {
        let mark = book.resolve_mark(Some(i), s, e);
        assert!(mark.is_some(), "高亮应成功通过 XPointer 解析起止页");
        if let Some(crate::book::Mark::Crengine(cm)) = mark {
            assert!(cm.start_page <= cm.end_page);
        } else {
            panic!("预期 Crengine 标记");
        }
    }

    assert_eq!(
        book.page_words_calls(),
        initial_calls,
        "解析 20 条高亮的起止页过程不应调用 page_words"
    );
}

#[test]
fn test_page_words_calls_unrelated_to_mark_count() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");

    let calls_before = book.page_words_calls();

    let mut resolved_marks = Vec::new();
    for i in 0..20 {
        let s = Position::XPointer {
            xpointer: format!("/body/DocFragment[1]/body/p[{}]/text().0", (i % 3) + 1),
        };
        let e = Position::XPointer {
            xpointer: format!("/body/DocFragment[1]/body/p[{}]/text().5", (i % 3) + 1),
        };
        if let Some(m) = book.resolve_mark(Some(i), &s, &e) {
            resolved_marks.push(m);
        }
    }
    assert_eq!(resolved_marks.len(), 20);
    assert_eq!(
        book.page_words_calls(),
        calls_before,
        "生成 20 条高亮后 page_words 调用次数必须保持为 0"
    );

    let mut chrome = Chrome::load(&style).expect("chrome fonts must load");
    let _page = book.render_page(1, &mut chrome).expect("渲染第 1 页");
    assert_eq!(
        book.page_words_calls(),
        0,
        "直接渲染页面时不调用 page_words"
    );
}

#[test]
fn test_timing_page_render_with_and_without_marks() {
    let _serial = serial();
    use std::time::Instant;
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");
    let mut chrome = Chrome::load(&style).expect("chrome fonts must load");

    // 预热渲染
    let _ = book.render_page(1, &mut chrome).unwrap();

    // 1. 无高亮渲染 20 次
    let t0 = Instant::now();
    for _ in 0..20 {
        let _ = book.render_page(1, &mut chrome).unwrap();
    }
    let elapsed_no_marks = t0.elapsed();

    // 2. 有 20 条在其他页面（第 0 页）的高亮，渲染第 1 页
    let marks: Vec<(Position, Position)> = (0..20)
        .map(|i| {
            let s = Position::XPointer {
                xpointer: format!("/body/DocFragment[1]/body/p[{}]/text().0", (i % 3) + 1),
            };
            let e = Position::XPointer {
                xpointer: format!("/body/DocFragment[1]/body/p[{}]/text().5", (i % 3) + 1),
            };
            (s, e)
        })
        .collect();

    let mut resolved_marks = Vec::new();
    for (i, (s, e)) in marks.iter().enumerate() {
        if let Some(m) = book.resolve_mark(Some(i), s, e) {
            resolved_marks.push(m);
        }
    }
    assert_eq!(resolved_marks.len(), 20);

    let t1 = Instant::now();
    for _ in 0..20 {
        let _ = book.render_page(1, &mut chrome).unwrap();
    }
    let elapsed_with_marks = t1.elapsed();

    println!(
        "Host timing (20 renders of page 1): no marks = {:.2} ms, with 20 marks on page 0 = {:.2} ms",
        elapsed_no_marks.as_secs_f64() * 1000.0,
        elapsed_with_marks.as_secs_f64() * 1000.0
    );
    assert_eq!(book.page_words_calls(), 0);
}

#[test]
fn test_mark_word_index_matching_and_fallbacks() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let mut book = open_interactive_book(&cache, &style).expect("打开 interactive.epub");
    let words = book.words(0);
    assert!(words.len() >= 10);

    // 精确匹配
    let exact_xp = words[3].start_xpointer.as_deref().unwrap();
    let start_idx = crate::book::mark_start_word_index(&words, Some(exact_xp));
    assert_eq!(start_idx, 3);

    let exact_end_xp = words[5].end_xpointer.as_deref().unwrap();
    let end_idx = crate::book::mark_end_word_index(&words, Some(exact_end_xp));
    assert_eq!(end_idx, 5);

    // 缺失 XPointer 时的默认行为
    assert_eq!(crate::book::mark_start_word_index(&words, None), 0);
    assert_eq!(
        crate::book::mark_end_word_index(&words, None),
        words.len() - 1
    );
}

/// H-013: after a font change with partial rerendering, the reader must
/// land on the text it was reading, not on the old page number in the new
/// size (which shows earlier text), and a highlight there must be found.
#[test]
fn a_partial_relayout_keeps_the_place_and_its_highlight() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    // The same cache directory as the other tests (it can be set once).
    let _ = crate::crengine::configure_cache(&cache.join("crengine-cache"));
    // Open twice: partial rerendering needs the book's cache file.
    drop(open_book(&cache, &style).expect("first open"));
    let mut book = open_book(&cache, &style).expect("open from cache");
    assert!(
        book.can_partial_rerender(),
        "the cached book can be partially laid out"
    );
    let mut chrome = Chrome::load(&style).expect("chrome fonts");
    let here = 94;
    book.render_page(here, &mut chrome).expect("render");
    let anchor = book.position_at(here);
    let Position::XPointer { xpointer } = &anchor else {
        panic!("crengine positions are XPointers: {anchor:?}");
    };
    let paragraph = xpointer.split("/text()").next().unwrap().to_string();

    let mut bigger = style.clone();
    bigger.font_size *= 1.3;
    book.enable_partial_rerender(true).expect("partial");
    book.set_layout(&bigger).expect("layout");
    let page = book.settled_location_of(&anchor).expect("placed");
    book.render_page(page, &mut chrome)
        .expect("render new page");
    assert_eq!(book.location_of(&anchor), Some(page), "the place stays put");
    let Position::XPointer { xpointer: shown } = book.position_at(page) else {
        panic!("XPointer expected");
    };
    assert!(
        shown.starts_with(&paragraph),
        "the page shows the paragraph read before ({paragraph}), not {shown}"
    );

    // A highlight on that page is found there, before and after catch-up.
    let words = book.words(page);
    let (s, e) = (
        words[10].start_xpointer.clone(),
        words[14].end_xpointer.clone(),
    );
    let (s, e) = (s.unwrap(), e.unwrap());
    let mark = |book: &Book| {
        book.resolve_mark(
            None,
            &Position::XPointer {
                xpointer: s.clone(),
            },
            &Position::XPointer {
                xpointer: e.clone(),
            },
        )
    };
    let Some(Mark::Crengine(m)) = mark(&book) else {
        panic!("highlight unresolved");
    };
    assert_eq!((m.start_page, m.end_page), (page, page));
    book.catchup_render().expect("catch-up");
    let Some(Mark::Crengine(m)) = mark(&book) else {
        panic!("highlight unresolved after catch-up");
    };
    assert_eq!((m.start_page, m.end_page), (page, page));
}

#[test]
fn test_integration_font_selection_combinations_and_partial_rerender() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    use_data_dir(&cache);
    let _ = crate::crengine::configure_cache(&cache.join("crengine-cache"));

    // Register test fonts: Charis, Gentium Book, Noto Serif SC
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../")
        .canonicalize()
        .unwrap();
    let charis = repo_root.join("aristotle/crates/aristotle-ui/assets/fonts/Charis-Regular.ttf");
    let gentium =
        repo_root.join("aristotle/crates/aristotle-ui/assets/fonts/GentiumBook-Regular.ttf");
    // A second Chinese face is not shipped; the test needs one in the cache
    // (NotoSerifSC-Regular.otf, OFL, from notofonts/noto-cjk) and is skipped
    // without it, like the cached book above.
    let noto_serif = cache.join("fonts/NotoSerifSC-Regular.otf");
    if !noto_serif.is_file() {
        eprintln!("skipped: {} is missing", noto_serif.display());
        return;
    }
    let _ = crate::crengine::register_fonts(&[charis, gentium, noto_serif]);
    // The family name depends on the release of the file ("Noto Serif SC" in
    // the subset OTF, "Noto Serif CJK SC" in the OTC builds): take whichever
    // serif Chinese face got registered.
    let Some(serif_sc) = crate::crengine::registered_faces()
        .into_iter()
        .find(|face| face.starts_with("Noto Serif") && face.ends_with("SC"))
    else {
        panic!("the Noto Serif SC file did not register a face");
    };

    let style = Style::pw4();

    // 1. 集成测试：两次 open 同一本书（命中 cache 时），切换西文字体或中文字体，can_partial_rerender() == true 成立，重排后 XPointer 锚点保持有效并出图
    drop(open_book(&cache, &style).expect("first open"));
    let mut book = open_book(&cache, &style).expect("open from cache");
    assert!(
        book.can_partial_rerender(),
        "second open of cached book must support partial rerender"
    );
    let mut chrome = Chrome::load(&style).expect("chrome fonts");
    let here = 50;
    book.render_page(here, &mut chrome)
        .expect("render initial page");
    let anchor = book.position_at(here);

    // Switch Latin font & Chinese font
    let mut new_style = style.clone();
    new_style.font_family = "Charis".into();
    new_style.cjk_font_family = serif_sc.clone();
    book.enable_partial_rerender(true).expect("enable partial");
    book.set_layout(&new_style).expect("layout");
    let page = book.settled_location_of(&anchor).expect("placed");
    let page_rendered = book
        .render_page(page, &mut chrome)
        .expect("render new page");
    assert_eq!(
        book.location_of(&anchor),
        Some(page),
        "anchor stays valid after font switch"
    );
    assert!(
        !page_rendered.data.is_empty(),
        "page must render successfully"
    );

    // 2. 出图测试：在 mixed.epub 上渲染 3 种西文字体 x 2 种中文字体共 6 种组合
    let mixed_path = repo_root.join("engine/fixtures/mixed.epub");

    let combos = [
        ("Literata", "Noto Sans SC", "literata_noto_sans"),
        ("Literata", serif_sc.as_str(), "literata_noto_serif"),
        ("Charis", "Noto Sans SC", "charis_noto_sans"),
        ("Charis", serif_sc.as_str(), "charis_noto_serif"),
        ("Gentium Book", "Noto Sans SC", "gentium_noto_sans"),
        ("Gentium Book", serif_sc.as_str(), "gentium_noto_serif"),
    ];

    let mut buffers: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    for (latin, cjk, name) in &combos {
        let mut s = Style::pw4();
        s.font_family = (*latin).into();
        s.cjk_font_family = (*cjk).into();
        let mut doc = Book::open(&mixed_path, &s, None).expect("open mixed.epub");
        let mut doc_chrome = Chrome::load(&s).expect("chrome fonts");
        let page = doc
            .render_page(0, &mut doc_chrome)
            .expect("render page 0 of mixed.epub");
        buffers.insert(name.to_string(), page.data.clone());
    }

    // 像素差异比对断言：
    // 当只切换中文字体时：中文字形变化，总像素差 > 0
    let l_sans = &buffers["literata_noto_sans"];
    let l_serif = &buffers["literata_noto_serif"];
    let diff_cjk = l_sans
        .iter()
        .zip(l_serif.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(diff_cjk > 0, "CJK font switch must produce pixel diff");

    // 当只切换西文字体时：西文字形变化，总像素差 > 0
    let c_sans = &buffers["charis_noto_sans"];
    let diff_latin = l_sans
        .iter()
        .zip(c_sans.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(diff_latin > 0, "Latin font switch must produce pixel diff");

    let g_sans = &buffers["gentium_noto_sans"];
    let diff_gentium = l_sans
        .iter()
        .zip(g_sans.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        diff_gentium > 0,
        "Gentium Book font switch must produce pixel diff"
    );
}

#[test]
fn test_open_txt_book_with_chapters_and_gbk() {
    let _serial = serial();
    let Some(cache_path) = cache() else {
        return;
    };
    use_data_dir(&cache_path);

    let temp = std::env::temp_dir().join("test_open_txt_crengine");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();

    let txt_path = temp.join("test_novel.txt");
    let content = "《玄幻风云》\n作者：墨阅开发者\n\n第一章 初出茅庐\n\
        少侠步入江湖，只见天地苍茫，四方云动。\n\n\
        第二章 试炼之峰\n\
        攀登险峰，面对重重考验，心意如磐石。\n\n\
        第三章 决胜归来\n\
        最终突破境界，载誉而归。\n";

    let (encoded, _, _) = encoding_rs::GBK.encode(content);
    std::fs::write(&txt_path, encoded).unwrap();

    let style = Style::pw4();
    let cache_dir = temp.join("cache");
    let mut book = Book::open(&txt_path, &style, Some(&cache_dir)).expect("open TXT book");

    assert_eq!(book.engine(), crate::book::Engine::Crengine);
    assert_eq!(book.title(), Some("玄幻风云"));
    assert!(!book.is_empty());

    let items = book.toc_items();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].title, "第一章 初出茅庐");
    assert_eq!(items[1].title, "第二章 试炼之峰");
    assert_eq!(items[2].title, "第三章 决胜归来");

    let mut chrome = crate::page::Chrome::load(&style).expect("chrome");
    let page = book.render_page(0, &mut chrome).expect("render page 0");
    assert_eq!(page.width, style.width);

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_open_txt_book_with_gb18030_four_byte_chars() {
    let _serial = serial();
    let Some(cache_path) = cache() else {
        return;
    };
    use_data_dir(&cache_path);

    let temp = std::env::temp_dir().join("test_open_txt_gb18030");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();

    let txt_path = temp.join("rare_chars.txt");
    let content = "《生僻字测试》\n作者：文字学\n\n第一章 四字节生僻字\n\
        这里有GB18030特有的生僻汉字：㐮、䶮、𬎆、𫖯，以及更多扩展区字。\n";

    let (encoded, _, _) = encoding_rs::GB18030.encode(content);
    std::fs::write(&txt_path, encoded).unwrap();

    let style = Style::pw4();
    let cache_dir = temp.join("cache");
    let mut book = Book::open(&txt_path, &style, Some(&cache_dir)).expect("open GB18030 TXT book");

    assert_eq!(book.engine(), crate::book::Engine::Crengine);
    assert_eq!(book.title(), Some("生僻字测试"));
    assert_eq!(book.toc_items().len(), 1);

    let mut chrome = crate::page::Chrome::load(&style).expect("chrome");
    let page = book.render_page(0, &mut chrome).expect("render page 0");
    assert_eq!(page.width, style.width);

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_bench_10mb_txt_cold_and_warm_open() {
    let _serial = serial();
    let Some(cache_path) = cache() else {
        return;
    };
    use_data_dir(&cache_path);

    let txt_path = std::path::Path::new("/tmp/txt_test/large_10mb.txt");
    if !txt_path.exists() {
        return;
    }
    let style = Style::pw4();
    let cache_dir = std::env::temp_dir().join("bench_large_txt_cache");
    let _ = std::fs::remove_dir_all(&cache_dir);

    // Cold open: parse TXT + auto-split chapters + uncompressed EPUB write + crengine open
    let t0 = std::time::Instant::now();
    let book_cold = Book::open(txt_path, &style, Some(&cache_dir)).expect("cold open");
    let t_cold = t0.elapsed();
    let pages = book_cold.len();
    let chapters = book_cold.toc_items().len();

    // Warm open: cache hit on EPUB + crengine open
    let t1 = std::time::Instant::now();
    let _book_warm = Book::open(txt_path, &style, Some(&cache_dir)).expect("warm open");
    let t_warm = t1.elapsed();

    println!(
        "\n=== BENCHMARK 10MB TXT ===\nCold open: {:?}\nWarm open: {:?}\nTotal pages: {}\nTotal chapters: {}\n===========================",
        t_cold, t_warm, pages, chapters
    );

    let _ = std::fs::remove_dir_all(&cache_dir);
}

fn save_page_png(page: &crate::page::Page, path: &Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), page.width, page.height);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    let mut data = Vec::with_capacity((page.width * page.height) as usize);
    for y in 0..page.height as usize {
        let row = y * page.stride;
        data.extend_from_slice(&page.data[row..row + page.width as usize]);
    }
    w.write_image_data(&data).unwrap();
}

#[test]
fn generate_txt001_screenshots() {
    let _serial = serial();
    let Some(cache_path) = cache() else {
        return;
    };
    use_data_dir(&cache_path);

    // Pictures go to a temporary folder unless ARISTOTLE_TEST_SHOTS names one
    // (docs/tasks/TXT-001/shots was made that way): a plain test run must not
    // rewrite files under version control.
    let shots_dir = std::env::var_os("ARISTOTLE_TEST_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("txt001_shots"));
    std::fs::create_dir_all(&shots_dir).unwrap();

    let temp = std::env::temp_dir().join("txt001_shots_data");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();

    let txt_path = temp.join("红楼梦.txt");
    // 公有领域文本：清·曹雪芹《红楼梦》（zh.wikisource.org/wiki/紅樓夢）
    let novel_content = r#"《红楼梦》
作者：曹雪芹

第一回 甄士隐梦幻识通灵 贾雨村风尘怀闺秀
此开卷第一回也。作者自云：因曾历过一番梦幻之后，故将真事隐去，而借通灵之说，撰此《石头记》一书也。故曰“甄士隐”云云。但书中所记何事何人？自又云：“今风尘碌碌，一事无成，忽念及当日所有之女子，一一细考较去，觉其行止见识，皆出于我之上。何我堂堂须眉，诚不若彼裙钗哉？实愧则有馀，悔又无益之大无可如何之日也！当此，则自欲将已往所赖天恩祖德，锦衣纨绔之时，饫甘靥肥之日，背父兄教育之恩，负师友规谈之德，以至今日一技无成，半生潦倒之罪，编述一集，以告天下人：我之罪固不蒙蔽，但彼钗钏辈，亦皆自有人段，不可因我之不肖，自护己短，而使彼等之往迹，亦蒙泯灭隐没而不称也。”

看官你道此书从何而来？说起根由虽近荒唐，细按则深有趣味。待在下将此梦公之于世，以供阅者一哂。

按那石上书云：当年地陷东南，女娲氏炼石补天，娲皇氏在荒渺山无稽崖炼成高经十二丈、方经二十四丈顽石三万六千五百零一块。娲皇氏只用了三万六千五百块，只单单剩下一块未用，便弃在青埂峰下。谁知此石自经煅炼之后，灵性已通，自去自来，可大可小。因见众石俱得补天，独自己无材不堪入选，遂自怨自叹，日夜悲号惭愧。

第二回 贾夫人仙逝扬州城 冷子兴演说荣国府
却说封肃因听见公差传唤，忙出来陪笑启问。那些人只嚷道：“快请出甄爷来！”封肃忙陪笑道：“小人姓封，并不姓甄。只有当日小婿姓甄，今已出家一二年了，不知公差大人找他何事？”

第三回 金陵城起复贾雨村 荣国府收养林黛玉
却说雨村因进了进士，便选入大如州知府。虽才干优长，未免有些贪酷之弊；且又仗着才情，轻侮同僚，因此被同僚参了一本，革职回籍。

第四回 薄命女偏逢薄命郎 葫芦僧乱判葫芦案
却说雨村便徇情枉法，胡乱判断了此案。冯家得了许多烧埋银子，倒也无甚异议了。
"#;

    let (encoded, _, _) = encoding_rs::GBK.encode(novel_content);
    std::fs::write(&txt_path, encoded).unwrap();

    let style = Style::pw4();
    let cache_dir = temp.join("cache");
    let mut book = Book::open(&txt_path, &style, Some(&cache_dir)).expect("open GBK novel");
    let mut chrome = crate::page::Chrome::load(&style).expect("chrome");

    // 1. 正文第 1 页 (page 0)
    let page1 = book.render_page(0, &mut chrome).expect("render page 0");
    save_page_png(&page1, &shots_dir.join("page1.png"));

    // 2. TOC 页面：渲染该书的目录页
    // 利用 Crengine 渲染带有标准样式的 TOC 结构页
    let toc_epub_path = temp.join("toc_preview.epub");
    let toc_items = book.toc_items();
    let mut toc_html = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh">
<head>
  <title>目录</title>
  <style>
    body { font-family: "Noto Sans SC", sans-serif; padding: 20px 40px; }
    h1 { text-align: center; font-size: 28px; margin-bottom: 30px; border-bottom: 2px solid #333; padding-bottom: 15px; }
    .toc-item { font-size: 20px; line-height: 2.6; border-bottom: 1px dotted #aaa; }
    .title { float: left; }
    .page { float: right; color: #444; }
    .clear { clear: both; }
  </style>
</head>
<body>
  <h1>目　录</h1>
"#,
    );
    for item in &toc_items {
        let p_num = item.page.map(|p| p + 1).unwrap_or(1);
        toc_html.push_str(&format!(
            r#"  <div class="toc-item"><span class="title">{}</span><span class="page">第 {} 页</span><div class="clear"></div></div>
"#,
            crate::txt::xml_escape(&item.title),
            p_num
        ));
    }
    toc_html.push_str("</body></html>");

    // 写入临时 EPUB 生成 TOC 页面截图
    use std::io::Write;
    let mut zip = zip::write::ZipWriter::new(std::fs::File::create(&toc_epub_path).unwrap());
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", opts).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    zip.start_file("META-INF/container.xml", opts).unwrap();
    zip.write_all(b"<?xml version=\"1.0\"?><container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>").unwrap();
    zip.start_file("toc.xhtml", opts).unwrap();
    zip.write_all(toc_html.as_bytes()).unwrap();
    zip.start_file("content.opf", opts).unwrap();
    let opf_xml = "<?xml version=\"1.0\"?><package xmlns=\"http://www.idpf.org/2007/opf\" version=\"2.0\" unique-identifier=\"uid\"><metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier id=\"uid\">toc</dc:identifier><dc:title>目录</dc:title><dc:language>zh</dc:language></metadata><manifest><item id=\"toc\" href=\"toc.xhtml\" media-type=\"application/xhtml+xml\"/></manifest><spine><itemref idref=\"toc\"/></spine></package>";
    zip.write_all(opf_xml.as_bytes()).unwrap();
    zip.finish().unwrap();

    let mut toc_book = Book::open(&toc_epub_path, &style, None).expect("open toc epub");
    let toc_page = toc_book
        .render_page(0, &mut chrome)
        .expect("render toc page");
    save_page_png(&toc_page, &shots_dir.join("toc.png"));

    let _ = std::fs::remove_dir_all(&temp);
}

fn save_side_by_side_png(pages: &[&crate::page::Page], path: &Path) {
    if pages.is_empty() {
        return;
    }
    let h = pages[0].height;
    let divider = 10usize;
    let total_w = pages.iter().map(|p| p.width as usize).sum::<usize>()
        + divider * (pages.len().saturating_sub(1));
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), total_w as u32, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    let mut data = Vec::with_capacity(total_w * h as usize);
    for y in 0..h as usize {
        for (i, page) in pages.iter().enumerate() {
            if i > 0 {
                data.resize(data.len() + divider, 0x88);
            }
            let row = y * page.stride;
            data.extend_from_slice(&page.data[row..row + page.width as usize]);
        }
    }
    w.write_image_data(&data).unwrap();
}

#[test]
fn test_cjk_typesetting_variations_and_generate_shots() {
    let _serial = serial();
    let Some(cache_path) = cache() else {
        return;
    };
    use_data_dir(&cache_path);

    let shots_dir = std::env::var_os("ARISTOTLE_TEST_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("type002_shots"));
    std::fs::create_dir_all(&shots_dir).unwrap();

    let temp = std::env::temp_dir().join("type002_shots_data");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();

    let txt_path = temp.join("红楼梦_cjk.txt");
    let novel_content = r#"第一回 甄士隐梦幻识通灵 贾雨村风尘怀闺秀
此开卷第一回也。作者自云：因曾历过一番梦幻之后，故将真事隐去，而借通灵之说，撰此《石头记》一书也。故曰“甄士隐”云云。但书中所记何事何人？

自又云：“今风尘碌碌，一事无成，忽念及当日所有之女子，一一细考较去，觉其行止见识，皆出于我之上。何我堂堂须眉，诚不若彼裙钗哉？实愧则有馀，悔又无益之大无可如何之日也！

看官你道此书从何而来？说起根由虽近荒唐，细按则深有趣味。待在下将此梦公之于世，以供阅者一哂。按那石上书云：当年地陷东南，女娲氏炼石补天，娲皇氏在荒渺山无稽崖炼成顽石。
"#;

    let (encoded, _, _) = encoding_rs::GBK.encode(novel_content);
    std::fs::write(&txt_path, encoded).unwrap();

    // 1. 测试首行缩进 2 字 vs 无缩进（纯文本在默认精排模式下也应响应缩进设置）
    let mut style_indent_2 = Style::pw4();
    style_indent_2.cjk_indent = 2;
    style_indent_2.cjk_gap = 0.0;

    let mut style_indent_none = Style::pw4();
    style_indent_none.cjk_indent = 0;
    style_indent_none.cjk_gap = 0.0;

    let cache_dir1 = temp.join("c1");
    let mut book_indent_2 =
        Book::open(&txt_path, &style_indent_2, Some(&cache_dir1)).expect("open indent 2");
    let mut chrome1 = crate::page::Chrome::load(&style_indent_2).expect("chrome");
    let page_indent_2 = book_indent_2
        .render_page(0, &mut chrome1)
        .expect("render indent 2");

    let cache_dir2 = temp.join("c2");
    let mut book_indent_none =
        Book::open(&txt_path, &style_indent_none, Some(&cache_dir2)).expect("open indent none");
    let mut chrome2 = crate::page::Chrome::load(&style_indent_none).expect("chrome");
    let page_indent_none = book_indent_none
        .render_page(0, &mut chrome2)
        .expect("render indent none");

    // 验证排版产生不同像素数据
    assert_ne!(
        page_indent_2.data, page_indent_none.data,
        "cjk_indent=2 and cjk_indent=0 must produce different layouts"
    );

    save_page_png(&page_indent_2, &shots_dir.join("indent-2chars.png"));
    save_page_png(&page_indent_none, &shots_dir.join("indent-none.png"));
    save_side_by_side_png(
        &[&page_indent_2, &page_indent_none],
        &shots_dir.join("compare-indent.png"),
    );

    // 2. 测试段间距：无 (0.0) vs 小 (0.3) vs 大 (0.6)
    let mut style_gap_none = Style::pw4();
    style_gap_none.cjk_indent = 2;
    style_gap_none.cjk_gap = 0.0;

    let mut style_gap_small = Style::pw4();
    style_gap_small.cjk_indent = 2;
    style_gap_small.cjk_gap = 0.3;

    let mut style_gap_large = Style::pw4();
    style_gap_large.cjk_indent = 2;
    style_gap_large.cjk_gap = 0.6;

    let cache_dir3 = temp.join("c3");
    let mut book_gap_none =
        Book::open(&txt_path, &style_gap_none, Some(&cache_dir3)).expect("open gap none");
    let mut chrome3 = crate::page::Chrome::load(&style_gap_none).expect("chrome");
    let page_gap_none = book_gap_none
        .render_page(0, &mut chrome3)
        .expect("render gap none");

    let cache_dir4 = temp.join("c4");
    let mut book_gap_small =
        Book::open(&txt_path, &style_gap_small, Some(&cache_dir4)).expect("open gap small");
    let mut chrome4 = crate::page::Chrome::load(&style_gap_small).expect("chrome");
    let page_gap_small = book_gap_small
        .render_page(0, &mut chrome4)
        .expect("render gap small");

    let cache_dir5 = temp.join("c5");
    let mut book_gap_large =
        Book::open(&txt_path, &style_gap_large, Some(&cache_dir5)).expect("open gap large");
    let mut chrome5 = crate::page::Chrome::load(&style_gap_large).expect("chrome");
    let page_gap_large = book_gap_large
        .render_page(0, &mut chrome5)
        .expect("render gap large");

    assert_ne!(
        page_gap_none.data, page_gap_small.data,
        "gap 0.0 and 0.3 must produce different layouts"
    );
    assert_ne!(
        page_gap_small.data, page_gap_large.data,
        "gap 0.3 and 0.6 must produce different layouts"
    );

    save_page_png(&page_gap_none, &shots_dir.join("gap-none.png"));
    save_page_png(&page_gap_small, &shots_dir.join("gap-small.png"));
    save_page_png(&page_gap_large, &shots_dir.join("gap-large.png"));
    save_side_by_side_png(
        &[&page_gap_none, &page_gap_small, &page_gap_large],
        &shots_dir.join("compare-gap.png"),
    );

    // 3. EPUB 测试：关闭精排（统一模式）时强制接管书内缩进并产生不同排版
    let epub_path = cache_path.join(BOOK);
    if epub_path.is_file() {
        let mut epub_style_i2 = style_indent_2.clone();
        epub_style_i2.fine_typesetting = false;
        let mut epub_style_i0 = style_indent_none.clone();
        epub_style_i0.fine_typesetting = false;
        let mut book_epub_i2 =
            Book::open(&epub_path, &epub_style_i2, None).expect("open epub indent 2");
        let mut book_epub_i0 =
            Book::open(&epub_path, &epub_style_i0, None).expect("open epub indent 0");
        let page_epub_i2 = book_epub_i2
            .render_page(5, &mut chrome1)
            .expect("render epub page 5 indent 2");
        let page_epub_i0 = book_epub_i0
            .render_page(5, &mut chrome2)
            .expect("render epub page 5 indent 0");
        assert_ne!(
            page_epub_i2.data, page_epub_i0.data,
            "EPUB rendering must vary with cjk_indent in uniform (non-fine) mode"
        );
        save_page_png(&page_epub_i2, &shots_dir.join("epub-indent-2chars.png"));
        save_page_png(&page_epub_i0, &shots_dir.join("epub-indent-none.png"));
        save_side_by_side_png(
            &[&page_epub_i2, &page_epub_i0],
            &shots_dir.join("compare-epub-indent.png"),
        );
    }

    let _ = std::fs::remove_dir_all(&temp);
}

fn create_test_epub(path: &Path, title: &str, body_html: &str) {
    use std::io::Write;
    let mut zip = zip::write::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", opts).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    zip.start_file("META-INF/container.xml", opts).unwrap();
    zip.write_all(b"<?xml version=\"1.0\"?><container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>").unwrap();
    zip.start_file("chapter.xhtml", opts).unwrap();
    let full_html = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh">
<head>
  <title>{title}</title>
</head>
<body>
{body_html}
</body>
</html>"#
    );
    zip.write_all(full_html.as_bytes()).unwrap();
    zip.start_file("content.opf", opts).unwrap();
    let opf_xml = format!(
        r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">test</dc:identifier><dc:title>{title}</dc:title><dc:language>zh</dc:language></metadata><manifest><item id="ch1" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch1"/></spine></package>"#
    );
    zip.write_all(opf_xml.as_bytes()).unwrap();
    zip.finish().unwrap();
}

#[test]
fn test_cjk_indent_class_style_and_u3000_spaces() {
    let _serial = serial();
    let Some(cache_path) = cache() else {
        println!("skipping test_cjk_indent_class_style_and_u3000_spaces: cache not found");
        return;
    };
    use_data_dir(&cache_path);

    let temp = std::env::temp_dir().join(format!("cjk_indent_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();

    // 1. 测试用书 A：书内自带带 class 的 text-indent 样式
    let epub_class_path = temp.join("test_class_indent.epub");
    let class_html = r#"<style>
.para { text-indent: 2em; }
</style>
<p class="para">这是一本测试书籍。段落使用 class 属性定义了首行缩进 2em。当用户在排版面板选择「无缩进」时，必须成功覆盖该书内自带样式。</p>
<p class="para">这是第二段测试正文。同样带有 class 类名样式。</p>"#;
    create_test_epub(&epub_class_path, "Class Indent Test", class_html);

    // 2. 测试用书 B：段首硬编码全角空格（U+3000）
    let epub_u3000_path = temp.join("test_u3000_indent.epub");
    let u3000_html = r#"<p>　　这是一本测试书籍。段首硬编码了两个全角空格（U+3000）。当用户在排版面板选择「无缩进」时，全角空格必须被有效抵消。</p>
<p>　　这是第二段测试正文。同样在段首包含两个全角空格。</p>"#;
    create_test_epub(&epub_u3000_path, "U3000 Indent Test", u3000_html);

    // 强制覆盖书内 class 缩进与全角空格属于统一排版模式（关闭精排）；
    // 开启精排时保留书内 class 样式与全角空格排版意图。
    let mut style_indent_2 = Style::pw4();
    style_indent_2.fine_typesetting = false;
    style_indent_2.cjk_indent = 2;
    style_indent_2.cjk_gap = 0.0;

    let mut style_indent_none = Style::pw4();
    style_indent_none.fine_typesetting = false;
    style_indent_none.cjk_indent = 0;
    style_indent_none.cjk_gap = 0.0;

    let mut chrome_2 = Chrome::load(&style_indent_2).expect("chrome i2");
    let mut chrome_none = Chrome::load(&style_indent_none).expect("chrome none");

    // 验证测试书 A (class-based text-indent):
    let mut book_class =
        Book::open(&epub_class_path, &style_indent_2, None).expect("open book class");
    let page_class_i2 = book_class
        .render_page(0, &mut chrome_2)
        .expect("render class i2");

    // 动态 set_layout 切为无缩进
    book_class
        .set_layout(&style_indent_none)
        .expect("set_layout to none");
    let page_class_none = book_class
        .render_page(0, &mut chrome_none)
        .expect("render class none");

    assert_ne!(
        page_class_i2.data, page_class_none.data,
        "Class-based text-indent must change when switching between 2 chars and none"
    );

    // 验证测试书 B (U+3000 full-width spaces):
    let mut book_u3000 =
        Book::open(&epub_u3000_path, &style_indent_2, None).expect("open book u3000");
    let page_u3000_i2 = book_u3000
        .render_page(0, &mut chrome_2)
        .expect("render u3000 i2");

    book_u3000
        .set_layout(&style_indent_none)
        .expect("set_layout to none");
    let page_u3000_none = book_u3000
        .render_page(0, &mut chrome_none)
        .expect("render u3000 none");

    assert_ne!(
        page_u3000_i2.data, page_u3000_none.data,
        "U+3000 hardcoded indent must change when switching between 2 chars and none"
    );

    let margin = style_indent_none.margin_px();
    let first_x = |book: &mut Book| book.words(0).first().map(|w| w.rect.x0);
    assert_eq!(
        first_x(&mut book_class),
        Some(margin),
        "class-based text-indent must not survive the none setting"
    );
    assert_eq!(
        first_x(&mut book_u3000),
        Some(margin),
        "leading U+3000 must not survive the none setting"
    );

    let shots_dir = std::env::var_os("ARISTOTLE_TEST_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.join("shots"));
    std::fs::create_dir_all(&shots_dir).unwrap();
    save_page_png(&page_class_i2, &shots_dir.join("indent-class-2chars.png"));
    save_page_png(&page_class_none, &shots_dir.join("indent-class-none.png"));
    save_page_png(&page_u3000_i2, &shots_dir.join("indent-u3000-2chars.png"));
    save_page_png(&page_u3000_none, &shots_dir.join("indent-u3000-none.png"));
    save_side_by_side_png(
        &[&page_class_i2, &page_class_none],
        &shots_dir.join("compare-indent-class.png"),
    );
    save_side_by_side_png(
        &[&page_u3000_i2, &page_u3000_none],
        &shots_dir.join("compare-indent-u3000.png"),
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn a_page_size_change_relays_out_the_book_at_the_new_size() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let Some(mut book) = open_book(&cache, &style) else {
        return;
    };

    // The portrait page the book opens with.
    let mut chrome = Chrome::load(&style).expect("chrome fonts must load");
    let page = book
        .render_location_with(0, &mut chrome, None)
        .expect("render the portrait page");
    assert_eq!(page.width, style.width, "the portrait page keeps its width");
    assert_eq!(
        page.height, style.height,
        "the portrait page keeps its height"
    );

    // The same panel turned sideways. The book is not reopened: the layout
    // path has to change the engine's viewport too, or the page comes back
    // with the old geometry.
    let mut landscape = style.clone();
    landscape.width = style.height;
    landscape.height = style.width;
    book.set_layout(&landscape)
        .expect("set the landscape layout");

    let mut chrome = Chrome::load(&landscape).expect("chrome fonts must load");
    let page = book
        .render_location_with(0, &mut chrome, None)
        .expect("render the landscape page");
    assert_eq!(
        page.width, landscape.width,
        "the landscape page is as wide as the new layout says"
    );
    assert_eq!(
        page.height, landscape.height,
        "the landscape page is as tall as the new layout says"
    );
    assert!(
        page.width > page.height,
        "the landscape page is wider than it is tall"
    );

    // Back to portrait: the book is still usable at the old size.
    book.set_layout(&style)
        .expect("set the portrait layout back");
    let mut chrome = Chrome::load(&style).expect("chrome fonts must load");
    let page = book
        .render_location_with(0, &mut chrome, None)
        .expect("render the portrait page again");
    assert_eq!(page.width, style.width);
    assert_eq!(page.height, style.height);
}

#[test]
fn a_rejected_page_size_leaves_the_book_alone() {
    let _serial = serial();
    let Some(cache) = cache() else {
        return;
    };
    let style = Style::pw4();
    let Some(mut book) = open_book(&cache, &style) else {
        return;
    };

    // A screen shorter than the footer margin: the body height underflows
    // and is not a page size. The book must keep its old geometry.
    let mut broken = style.clone();
    broken.height = 10;
    assert!(
        book.set_layout(&broken).is_err(),
        "an underflowed body height must be rejected"
    );

    let mut chrome = Chrome::load(&style).expect("chrome fonts must load");
    let page = book
        .render_location_with(0, &mut chrome, None)
        .expect("the book still renders at the old size");
    assert_eq!(page.width, style.width);
    assert_eq!(page.height, style.height);
}
