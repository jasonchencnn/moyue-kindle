// SPDX-License-Identifier: AGPL-3.0-or-later
//! KO2-ENG-001: a recognized chapter starts on a new page, TXT and EPUB.
//!
//! The books are synthetic: short chapters that would otherwise share a page,
//! so a page number can only be right when the boundary itself moved.

use std::path::{Path, PathBuf};

use aristotle_render::book::Book;
use aristotle_render::page::Chrome;
use aristotle_render::style::Style;

fn data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join(".cache/aristotle/render/device/data");
    dir.join("serif/Literata-Regular.ttf")
        .is_file()
        .then_some(dir)
}

fn use_data_dir() -> Option<PathBuf> {
    let dir = data_dir()?;
    std::env::set_current_dir(&dir).ok()?;
    Some(dir)
}

fn screenshots_dir() -> Option<PathBuf> {
    std::env::var_os("ARISTOTLE_TEST_SHOTS").map(PathBuf::from)
}

fn save_page_png(page: &aristotle_render::page::Page, path: &Path) {
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

fn render_and_save(book: &mut Book, name: &str) {
    let Some(dir) = screenshots_dir() else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let mut chrome = Chrome::load(&Style::pw4()).expect("chrome");
    let page = book.render_page(0, &mut chrome).expect("render first page");
    save_page_png(&page, &dir.join(format!("{name}-page1.png")));
    let page = book
        .render_page(1, &mut chrome)
        .expect("render second page");
    save_page_png(&page, &dir.join(format!("{name}-page2.png")));
}

fn zip_file(path: &Path, entries: &[(&str, &str)]) {
    use std::io::Write;
    let mut zip = zip::write::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn epub(path: &Path, files: &[(&str, &str)], nav: &str, spine: &str) {
    let mut manifest = String::new();
    let mut entries: Vec<(String, String)> = vec![
        ("mimetype".into(), "application/epub+zip".into()),
        (
            "META-INF/container.xml".into(),
            r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.into(),
        ),
    ];
    for (i, (name, body)) in files.iter().enumerate() {
        entries.push(((*name).into(), (*body).into()));
        manifest.push_str(&format!(
            "<item id=\"i{i}\" href=\"{name}\" media-type=\"application/xhtml+xml\"/>"
        ));
    }
    let opf = format!(
        r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">ko2</dc:identifier><dc:title>章节边界</dc:title><dc:language>zh</dc:language></metadata><manifest>{manifest}<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx">{spine}</spine></package>"#
    );
    entries.push(("content.opf".into(), opf));
    entries.push((
        "toc.ncx".into(),
        format!(
            r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head><meta name="dtb:uid" content="ko2"/><meta name="dtb:depth" content="1"/></head><docTitle><text>章节边界</text></docTitle><navMap>{nav}</navMap></ncx>"#
        ),
    ));
    let refs: Vec<(&str, &str)> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_str()))
        .collect();
    zip_file(path, &refs);
}

fn short_chapter(id: &str, title: &str) -> String {
    format!("<h2 id=\"{id}\">{title}</h2><p>短章一句话。</p>")
}

fn toc_pages(book: &mut Book) -> Vec<(String, usize)> {
    book.paginate();
    book.toc_items()
        .into_iter()
        .map(|item| (item.title, item.page.unwrap_or(usize::MAX)))
        .collect()
}

fn page_text(book: &mut Book, page: usize) -> String {
    book.words(page)
        .into_iter()
        .map(|word| word.text)
        .collect::<String>()
}

#[cfg(feature = "crengine")]
fn enable_cache() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join("ko2-chapter-fix-crengine-cache");
        let _ = std::fs::create_dir_all(&dir);
        // The cache is process-wide; another test binary may have initialized
        // it first, which is fine for a warm reopen.
        let _ = aristotle_render::crengine::configure_cache(&dir);
    });
}

#[test]
fn a_short_txt_chapter_still_starts_on_a_new_page() {
    let _ = use_data_dir();
    let temp = std::env::temp_dir().join("ko2_eng_txt_boundary");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let txt = temp.join("short.txt");
    std::fs::write(
        &txt,
        "第一章 起点\n短章一句话。\n第二章 风起\n短章一句话。\n第三章 归途\n短章一句话。\n",
    )
    .unwrap();
    let style = Style::pw4();
    let mut book = Book::open(&txt, &style, Some(&temp.join("cache"))).expect("open TXT");
    let pages = toc_pages(&mut book);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts.len(), 3, "TOC: {pages:?}");
    assert_eq!(
        starts,
        (0..3).collect::<Vec<_>>(),
        "each short chapter must start on its own page: {pages:?}"
    );
    render_and_save(&mut book, "txt");
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn a_short_spine_chapter_still_starts_on_a_new_page() {
    let _ = use_data_dir();
    let temp = std::env::temp_dir().join("ko2_eng_epub_boundary");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let files = [
        (
            "c0.xhtml",
            "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h2>第一章</h2><p>短章一句话。</p></body></html>",
        ),
        (
            "c1.xhtml",
            "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h2>第二章</h2><p>短章一句话。</p></body></html>",
        ),
        (
            "c2.xhtml",
            "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h2>第三章</h2><p>短章一句话。</p></body></html>",
        ),
    ];
    let nav = r#"<navPoint id="n0" playOrder="1"><navLabel><text>第一章</text></navLabel><content src="c0.xhtml"/></navPoint><navPoint id="n1" playOrder="2"><navLabel><text>第二章</text></navLabel><content src="c1.xhtml"/></navPoint><navPoint id="n2" playOrder="3"><navLabel><text>第三章</text></navLabel><content src="c2.xhtml"/></navPoint>"#;
    let spine = r#"<itemref idref="i0"/><itemref idref="i1"/><itemref idref="i2"/>"#;
    let path = temp.join("spine.epub");
    epub(&path, &files, nav, spine);
    let style = Style::pw4();
    let mut book = Book::open(&path, &style, None).expect("open EPUB");
    let pages = toc_pages(&mut book);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts.len(), 3, "TOC: {pages:?}");
    assert_eq!(
        starts,
        (0..3).collect::<Vec<_>>(),
        "each short spine chapter must start on its own page: {pages:?}"
    );
    render_and_save(&mut book, "epub-spine");
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn a_short_single_file_epub_chapter_still_starts_on_a_new_page() {
    let _ = use_data_dir();
    let temp = std::env::temp_dir().join("ko2_eng_epub_single_boundary");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let body = format!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>{}{}{}</body></html>",
        short_chapter("c0", "第一章"),
        short_chapter("c1", "第二章"),
        short_chapter("c2", "第三章")
    );
    let files = [("book.xhtml", body.as_str())];
    let nav = r#"<navPoint id="n0" playOrder="1"><navLabel><text>第一章</text></navLabel><content src="book.xhtml#c0"/></navPoint><navPoint id="n1" playOrder="2"><navLabel><text>第二章</text></navLabel><content src="book.xhtml#c1"/></navPoint><navPoint id="n2" playOrder="3"><navLabel><text>第三章</text></navLabel><content src="book.xhtml#c2"/></navPoint>"#;
    let spine = r#"<itemref idref="i0"/>"#;
    let path = temp.join("single.epub");
    epub(&path, &files, nav, spine);
    let style = Style::pw4();
    let mut book = Book::open(&path, &style, None).expect("open EPUB");
    let pages = toc_pages(&mut book);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts.len(), 3, "TOC: {pages:?}");
    assert_eq!(
        starts,
        (0..3).collect::<Vec<_>>(),
        "each short single-file chapter must start on its own page: {pages:?}"
    );
    render_and_save(&mut book, "epub-single");
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn toc_targets_p_div_and_a_break_their_block_not_every_heading() {
    let _ = use_data_dir();
    let temp = std::env::temp_dir().join("ko2_chapter_fix_targets");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    // The TOC points at a div, a p and an empty a. The h2 inside the first
    // div is a subsection the TOC never names, so it must stay on the
    // chapter's page instead of starting one.
    let body = concat!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>",
        "<div id=\"c0\"><h2>第一章</h2><p>短章一句话。</p><h2>一、人物</h2><p>小节一句话。</p></div>",
        "<p id=\"c1\">第二章</p><p>短章一句话。</p>",
        "<a name=\"c2\"></a><h2>第三章</h2><p>短章一句话。</p>",
        "</body></html>"
    );
    let files = [("book.xhtml", body)];
    let nav = r#"<navPoint id="n0" playOrder="1"><navLabel><text>第一章</text></navLabel><content src="book.xhtml#c0"/></navPoint><navPoint id="n1" playOrder="2"><navLabel><text>第二章</text></navLabel><content src="book.xhtml#c1"/></navPoint><navPoint id="n2" playOrder="3"><navLabel><text>第三章</text></navLabel><content src="book.xhtml#c2"/></navPoint>"#;
    let spine = r#"<itemref idref="i0"/>"#;
    let path = temp.join("targets.epub");
    epub(&path, &files, nav, spine);
    let style = Style::pw4();
    let mut book = Book::open(&path, &style, None).expect("open EPUB");
    let pages = toc_pages(&mut book);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts, vec![0, 1, 2], "TOC anchors must break: {pages:?}");
    assert_eq!(book.len(), 3, "no extra page from the empty anchor");

    let first = page_text(&mut book, 0);
    assert!(first.contains("第一章"), "page 0: {first:?}");
    assert!(
        first.contains("人物"),
        "an untargeted h2 subsection must stay on its chapter page: {first:?}"
    );
    assert!(!first.contains("第二章"), "page 0: {first:?}");
    let second = page_text(&mut book, 1);
    assert!(second.contains("第二章"), "page 1: {second:?}");
    let third = page_text(&mut book, 2);
    assert!(third.contains("第三章"), "page 2: {third:?}");

    render_and_save(&mut book, "epub-anchors");
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn toc_anchor_breaks_survive_restyle_and_warm_reopen() {
    let _ = use_data_dir();
    enable_cache();
    let temp = std::env::temp_dir().join("ko2_chapter_fix_reopen");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let body = concat!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>",
        "<div id=\"c0\"><p>第一章</p><p>短章一句话。</p></div>",
        "<div id=\"c1\"><p>第二章</p><p>短章一句话。</p></div>",
        "</body></html>"
    );
    let files = [("book.xhtml", body)];
    let nav = r#"<navPoint id="n0" playOrder="1"><navLabel><text>第一章</text></navLabel><content src="book.xhtml#c0"/></navPoint><navPoint id="n1" playOrder="2"><navLabel><text>第二章</text></navLabel><content src="book.xhtml#c1"/></navPoint>"#;
    let spine = r#"<itemref idref="i0"/>"#;
    let path = temp.join("reopen.epub");
    epub(&path, &files, nav, spine);
    let style = Style::pw4();

    let mut book = Book::open(&path, &style, None).expect("open EPUB");
    let pages = toc_pages(&mut book);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts, vec![0, 1], "first open: {pages:?}");
    assert_eq!(book.len(), 2, "first open must not add a blank page");
    let css = book
        .crengine_layout()
        .and_then(|layout| layout.user_css.as_deref())
        .expect("the user sheet is set");
    assert!(
        css.contains(r#"[id="_doc_fragment_0_ c0"]"#),
        "the rule must name crengine's prefixed id: {css}"
    );
    assert!(
        !css.contains("body > h1")
            && !css.contains("body > h2")
            && !css.contains("body > h3")
            && !css.contains("div > h2:first-child"),
        "no heading-position wildcard may force chapter pages: {css}"
    );

    drop(book);

    // Reopen with the same style: the engine's parsed book and layout cache
    // must give the same pages without a forced re-layout.
    let mut warm = Book::open(&path, &style, None).expect("warm reopen");
    let pages = toc_pages(&mut warm);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts, vec![0, 1], "warm reopen: {pages:?}");
    assert_eq!(warm.len(), 2, "warm reopen must not add a blank page");
    let first = page_text(&mut warm, 0);
    assert!(first.contains("第一章"), "page 0: {first:?}");
    let second = page_text(&mut warm, 1);
    assert!(second.contains("第二章"), "page 1: {second:?}");

    let mut restyled = style.clone();
    restyled.cjk_indent = 0;
    restyled.cjk_gap = 0.0;
    warm.set_layout(&restyled).expect("restyle");
    let pages = toc_pages(&mut warm);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts, vec![0, 1], "after restyle: {pages:?}");
    assert_eq!(warm.len(), 2, "restyle must not add a blank page");
    drop(warm);

    // The restyled sheet is a different layout; a warm open with it keeps the
    // anchor breaks too.
    let mut restyled_warm = Book::open(&path, &restyled, None).expect("warm reopen after restyle");
    let pages = toc_pages(&mut restyled_warm);
    let starts: Vec<usize> = pages.iter().map(|(_, page)| *page).collect();
    assert_eq!(starts, vec![0, 1], "warm restyled reopen: {pages:?}");
    assert_eq!(
        restyled_warm.len(),
        2,
        "warm restyled reopen must not add a blank page"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn an_empty_txt_opens_to_a_blank_page() {
    let _ = use_data_dir();
    let temp = std::env::temp_dir().join("ko2_eng_empty_txt");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let txt = temp.join("empty.txt");
    std::fs::write(&txt, b"").unwrap();

    let style = Style::pw4();
    let mut book = Book::open(&txt, &style, Some(&temp.join("cache")))
        .expect("empty TXT must open, not fail with 'document has no pages'");
    assert_eq!(book.paginate(), 1, "an empty TXT has one blank page");
    let mut chrome = Chrome::load(&style).expect("chrome");
    book.render_page(0, &mut chrome)
        .expect("render the blank page");
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn legacy_txt_word_positions_resolve_to_the_same_words_after_resplitting() {
    let _ = use_data_dir();
    let dir = std::env::temp_dir().join("ko2_txt_legacy_round_trip");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let txt = dir.join("legacy.txt");
    std::fs::write(&txt, "前言\n引言正文。\n第一章Alpha\n章一正文。\nChapter 2 Beta\n中段Middle\n第二章 归途\n末段Omega\n").unwrap();
    let old = dir.join("converter1.epub");
    let first = "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h2>前言</h2><p>引言正文。</p><p>第一章Alpha</p><p>章一正文。</p><p>Chapter 2 Beta</p><p>中段Middle</p></body></html>";
    let last = "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h2>第二章 归途</h2><p>末段Omega</p></body></html>";
    epub(&old, &[("first.xhtml", first), ("last.xhtml", last)],
        "<navPoint id=\"n0\" playOrder=\"1\"><navLabel><text>前言</text></navLabel><content src=\"first.xhtml\"/></navPoint><navPoint id=\"n1\" playOrder=\"2\"><navLabel><text>归途</text></navLabel><content src=\"last.xhtml\"/></navPoint>",
        "<itemref idref=\"i0\"/><itemref idref=\"i1\"/>");
    let style = Style::pw4();
    let mut legacy = Book::open(&old, &style, Some(&dir.join("old-cache"))).unwrap();
    let mut current = Book::open(&txt, &style, Some(&dir.join("new-cache"))).unwrap();
    for token in ["Alpha", "Beta", "Middle", "Omega"] {
        legacy.paginate();
        let old_word = (0..legacy.len())
            .flat_map(|page| legacy.words(page))
            .find(|word| word.text.contains(token))
            .expect("word in converter-1 fixture");
        let mapped = aristotle_render::txt::map_legacy_xpointer(
            &txt,
            old_word.start_xpointer.as_deref().unwrap(),
        )
        .unwrap();
        let mapped_end = aristotle_render::txt::map_legacy_xpointer(
            &txt,
            old_word.end_xpointer.as_deref().unwrap(),
        )
        .unwrap();
        let location = current
            .location_of(&aristotle_render::book::Position::XPointer {
                xpointer: mapped.clone(),
            })
            .expect("mapped pointer resolves");
        let word = current
            .words(location)
            .into_iter()
            .find(|word| word.text.contains(token))
            .expect("same word on mapped page");
        assert_eq!(
            mapped_end.replace("[1]", ""),
            word.end_xpointer.as_ref().unwrap().replace("[1]", ""),
            "{token} end"
        );
        // crengine omits singleton [1] steps in its canonical paths.
        assert_eq!(
            mapped.replace("[1]", ""),
            word.start_xpointer.unwrap().replace("[1]", ""),
            "{token}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
