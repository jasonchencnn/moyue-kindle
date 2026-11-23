// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! The vertical engine behind [`aristotle_render::book::Book`]: opening a
//! TXT or an EPUB with `Style::writing_mode = VerticalRtl` must paginate,
//! name its chapters, keep its place as a fraction and render a page.

use std::path::PathBuf;

use aristotle_render::book::{Book, Engine, Position};
use aristotle_render::page::Chrome;
use aristotle_render::style::{Style, Underline, UnderlineScope, WritingMode};
use aristotle_render::vertical;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::var_os("ARISTOTLE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = dir.join("vertical-tests");
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir.join(name)
}

fn vertical_style() -> Style {
    let mut style = Style::pw4();
    style.writing_mode = WritingMode::VerticalRtl;
    style
}

/// Two chapters, each long enough to fill more than one page of the PW4.
fn sample_text() -> String {
    let mut text = String::from("第一章 甄士隱夢幻識通靈\n");
    for _ in 0..6 {
        text.push_str(
            "此開卷第一回也。作者自云：因曾歷過一番夢幻之後，故將真事隱去，而借通靈之說，撰此《石頭記》一書也。\n",
        );
        text.push_str(
            "我堂堂鬚眉，誠不若彼裙釵哉！實愧則有餘，悔又無益之大無可如何之日也。乾隆五十六年辛亥（1791）程甲本。\n",
        );
    }
    text.push_str("第二章 賈夫人仙逝揚州城\n");
    for _ in 0..6 {
        text.push_str("詩云：一局輸贏料不真，香銷茶盡尚逡巡。欲知目下興衰兆，須問旁觀冷眼人。\n");
    }
    text
}

#[test]
fn a_vertical_book_paginates_names_chapters_and_keeps_its_place() {
    let style = vertical_style();
    if !vertical::has_font(&style) {
        eprintln!("no font in the data directory; skipping the vertical book test");
        return;
    }
    let path = scratch("vertical-book.txt");
    std::fs::write(&path, sample_text()).expect("the book file");

    let mut book = Book::open(&path, &style, None).expect("the book opens");
    assert_eq!(book.engine(), Engine::Vertical);
    assert_eq!(book.title(), Some("vertical-book"));
    assert!(book.len() > 60, "characters: {}", book.len());

    // The table of contents comes from the TXT chapter headings.
    let toc = book.toc_items();
    assert_eq!(toc.len(), 2);
    assert_eq!(toc[0].title, "第一章 甄士隱夢幻識通靈");
    assert_eq!(toc[1].title, "第二章 賈夫人仙逝揚州城");
    assert!(toc[0].page.is_none(), "pages are found lazily");

    // Pagination is a chain of page layouts.
    let pages = book.paginate();
    assert!(pages > 1, "the sample needs more than one page: {pages}");
    assert_eq!(book.known_page_count(), Some(pages));
    assert_eq!(book.locate(0), Some(0));
    let second = book.next_page(0).expect("a second page");
    assert!(second > 0);
    assert_eq!(book.previous_page(second), Some(0));
    assert_eq!(book.page_start(second), Some(second));
    assert_eq!(book.index_of(second), Some(1));
    assert!(book.chapter_title(0).is_some());

    // A position survives the switch to the horizontal engine as a share of
    // the book, and comes back to the same page.
    let position = book.position_at(second);
    assert!(matches!(position, Position::Fraction(_)));
    let back = book.location_of(&position).expect("the fraction is placed");
    assert!(
        back.abs_diff(second) <= 1,
        "the fraction lands on the same page: {back} vs {second}"
    );

    // The table of contents pages are found without laying out the book.
    assert_eq!(book.toc_page(0), Some(0));
    assert!(book.toc_page(1).is_some());
    let chapter_two = book.chapter_page(0, true).expect("the next chapter");
    assert!(chapter_two > 0);
    assert_eq!(book.page_start(chapter_two), Some(chapter_two));
    assert_eq!(book.toc_current(0), Some(0));

    // The underline settings are render options: changing them needs no new
    // layout.
    book.set_underline(Underline::Solid);
    book.set_underline_scope(UnderlineScope::FullLine);

    // Plato's chrome opens its font relative to the data directory.
    if let Some(dir) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
        std::env::set_current_dir(dir).expect("the data directory");
    }
    let mut chrome = Chrome::load(&style).expect("the chrome font");
    let page = book
        .render_location(0, &mut chrome)
        .expect("the first page renders");
    assert_eq!(page.width, style.width);
    assert_eq!(page.height, style.height);
    let dark = page.data.iter().filter(|&&pixel| pixel < 0x80).count();
    assert!(dark > 200, "the page has text ({dark} dark pixels)");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_progress_fraction_is_the_saved_position() {
    let style = vertical_style();
    if !vertical::has_font(&style) {
        eprintln!("no font in the data directory; skipping the vertical book test");
        return;
    }
    let path = scratch("vertical-fraction.txt");
    std::fs::write(&path, sample_text()).expect("the book file");

    let mut book = Book::open(&path, &style, None).expect("the book opens");
    assert_eq!(book.engine(), Engine::Vertical);
    let pages = book.paginate();
    assert!(pages > 1, "the sample needs more than one page: {pages}");
    let len = book.len();
    assert!(len > 1);

    // The footer's fraction and the saved position are the same measure: a
    // share of the characters, not of the pages, so a typography change
    // cannot move the place they name (VERT-101 M2).
    for location in [0, 1, len / 3, len / 2, len - 1] {
        let fraction = book.fraction(location);
        assert!((0.0..=1.0).contains(&fraction), "{fraction}");
        let saved = match book.position_at(location) {
            Position::Fraction(f) => f,
            other => panic!("a vertical book saves a fraction, not {other:?}"),
        };
        assert!(
            (fraction - saved).abs() < f32::EPSILON,
            "location {location}: footer {fraction}, saved {saved}"
        );
    }
    // The half-way character is half-way through the book, and the fraction
    // names that character again.
    let middle = len / 2;
    assert!((book.fraction(middle) - 0.5).abs() < 0.01);
    let back = book
        .location_of(&Position::Fraction(book.fraction(middle)))
        .expect("the fraction is placed");
    assert!(
        back.abs_diff(middle) <= 1,
        "the fraction comes back to {back}, not {middle}"
    );

    let _ = std::fs::remove_file(&path);
}
