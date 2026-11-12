// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search screen with on-screen keyboard, candidate bar, and in-memory book matching.

use crate::geom::Insets;
use crate::icons::Icon;
use crate::keyboard::{
    action as kb_action, build_input_box, build_keyboard, pinyin, to_simplified_str, InputMode,
    KeyboardState,
};
use crate::screens::library::{status_line, Book, StatusLine};
use crate::screens::{icon_button, text_button};
use crate::text::{FamilyId, TextStyle};
use crate::theme::{margin, IconSize, Styles, INK, INK_2, INK_3};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::{Cover, GrayImage};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 2000;
    pub const CANCEL: ActionId = 2001;
    pub const RESULT_BASE: ActionId = 2100;
}

/// The result of processing an action event on the search screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchOutcome {
    None,
    Back,
    OpenBook(usize),
    Changed,
}

/// Pre-computed search index for local library books.
/// Built once when the search screen is opened, avoiding full-collection
/// recomputation on keystrokes.
#[derive(Clone, Debug, Default)]
pub struct SearchIndex {
    pub books: Vec<IndexedBook>,
}

#[derive(Clone, Debug)]
pub struct IndexedBook {
    pub orig_index: usize,
    pub title_simp: String,
    pub author_simp: String,
    pub title_syllables: Vec<&'static str>,
    pub title_initials: String,
    pub author_syllables: Vec<&'static str>,
    pub author_initials: String,
}

impl SearchIndex {
    /// Builds the search index from a slice of library books.
    pub fn build(books: &[Book]) -> Self {
        let mut indexed = Vec::with_capacity(books.len());
        for (i, b) in books.iter().enumerate() {
            let title_simp = to_simplified_str(&b.title);
            let author_simp = to_simplified_str(&b.author);

            let mut title_syllables = Vec::new();
            let mut title_initials = String::new();
            for c in title_simp.chars() {
                if let Some(syl) = pinyin::char_primary_pinyin(c) {
                    title_syllables.push(syl);
                    title_initials.push(syl.chars().next().unwrap());
                } else if c.is_ascii_alphabetic() {
                    title_initials.push(c.to_ascii_lowercase());
                }
            }

            let mut author_syllables = Vec::new();
            let mut author_initials = String::new();
            for c in author_simp.chars() {
                if let Some(syl) = pinyin::char_primary_pinyin(c) {
                    author_syllables.push(syl);
                    author_initials.push(syl.chars().next().unwrap());
                } else if c.is_ascii_alphabetic() {
                    author_initials.push(c.to_ascii_lowercase());
                }
            }

            indexed.push(IndexedBook {
                orig_index: i,
                title_simp,
                author_simp,
                title_syllables,
                title_initials,
                author_syllables,
                author_initials,
            });
        }
        Self { books: indexed }
    }

    /// Searches the index with a query string, returning original book indices.
    pub fn search(&self, query: &str) -> Vec<usize> {
        let q_norm = to_simplified_str(query);
        let q = q_norm.trim();
        if q.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();
        for b in &self.books {
            if b.title_simp.contains(q)
                || b.author_simp.contains(q)
                || b.title_initials.contains(q)
                || b.author_initials.contains(q)
                || pinyin::match_pinyin_seq(&b.title_syllables, &b.title_initials, q)
                || pinyin::match_pinyin_seq(&b.author_syllables, &b.author_initials, q)
                || pinyin::match_initials(&b.title_simp, q)
                || pinyin::match_initials(&b.author_simp, q)
            {
                matches.push(b.orig_index);
            }
        }
        matches
    }
}

/// Handles to interactive nodes and state of the search screen.
pub struct SearchUi {
    pub keyboard: KeyboardState,
    pub back_btn: NodeId,
    pub cancel_btn: NodeId,
    pub index: SearchIndex,
    pub matched_indices: Vec<usize>,
}

impl SearchUi {
    /// Dispatches an action event on the search screen, updating internal state.
    pub fn handle_action(&mut self, id: ActionId) -> SearchOutcome {
        match id {
            action::BACK => SearchOutcome::Back,
            action::CANCEL => {
                if self.keyboard.pinyin.is_empty() && self.keyboard.committed.is_empty() {
                    SearchOutcome::Back
                } else {
                    self.keyboard.clear();
                    SearchOutcome::Changed
                }
            }
            id if (action::RESULT_BASE..action::RESULT_BASE + 100).contains(&id) => {
                let idx = (id - action::RESULT_BASE) as usize;
                if let Some(&orig_idx) = self.matched_indices.get(idx) {
                    SearchOutcome::OpenBook(orig_idx)
                } else {
                    SearchOutcome::None
                }
            }
            kb_action::SPACE => {
                self.keyboard.press_space();
                SearchOutcome::Changed
            }
            kb_action::DELETE => {
                self.keyboard.press_delete();
                SearchOutcome::Changed
            }
            kb_action::MODE_ZH => {
                self.keyboard.set_mode(InputMode::Chinese);
                SearchOutcome::Changed
            }
            kb_action::MODE_EN => {
                self.keyboard.set_mode(InputMode::English);
                SearchOutcome::Changed
            }
            kb_action::TOGGLE_PAGE => {
                self.keyboard.toggle_page();
                SearchOutcome::Changed
            }
            kb_action::SHIFT => {
                self.keyboard.press_shift();
                SearchOutcome::Changed
            }
            kb_action::CAND_PREV => {
                self.keyboard.prev_cand_page();
                SearchOutcome::Changed
            }
            kb_action::CAND_NEXT => {
                self.keyboard.next_cand_page();
                SearchOutcome::Changed
            }
            id if (kb_action::CAND_BASE..kb_action::CAND_BASE + 10).contains(&id) => {
                let idx = (id - kb_action::CAND_BASE) as usize;
                if self.keyboard.pick_candidate(idx) {
                    SearchOutcome::Changed
                } else {
                    SearchOutcome::None
                }
            }
            id if (kb_action::KEY_BASE..kb_action::KEY_BASE + 0x1000).contains(&id) => {
                if let Some(ch) = char::from_u32(id - kb_action::KEY_BASE) {
                    self.keyboard.press_key(ch);
                    SearchOutcome::Changed
                } else {
                    SearchOutcome::None
                }
            }
            kb_action::CONFIRM => {
                if let Some(&orig_idx) = self.matched_indices.first() {
                    SearchOutcome::OpenBook(orig_idx)
                } else {
                    SearchOutcome::None
                }
            }
            _ => SearchOutcome::None,
        }
    }
}

/// Builds the search screen tree and UI handles.
///
/// `cover` reads the thumbnail of a result: only the handful of books the
/// screen draws ask for one, so searching a thousand books never reads a
/// thousand pictures.
pub fn build_search_screen(
    t: &mut Tree,
    s: &Styles,
    books: &[Book],
    cover: &mut dyn FnMut(usize) -> Option<GrayImage>,
    index: &SearchIndex,
    keyboard_state: &KeyboardState,
    status: &StatusLine,
) -> (NodeId, SearchUi) {
    let bar = status_line(t, s, status);
    let status_bar = t.pad(Insets::vh(0, margin()), bar.root);

    // Search header bar: [Back] [Input Box] [Cancel]
    let back_btn = icon_button(t, Icon::ArrowLeft, IconSize::STANDARD, action::BACK);

    let input_node = build_input_box(
        t,
        s,
        &keyboard_state.committed,
        &keyboard_state.pinyin,
        tr(Msg::ImeSearchPlaceholder),
    );
    let input_wrap = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(88)),
        })
        .flex(1),
        [input_node],
    );

    let cancel_btn = text_button(t, tr(Msg::CommonCancel), s.ui(), action::CANCEL);

    let header_bar = t.add(
        Node::row(s.density.px(16)).cross(Cross::Center),
        [back_btn, input_wrap, cancel_btn],
    );
    let header_bar = t.pad(Insets::vh(0, margin()), header_bar);

    // Results area
    let query = keyboard_state.query();
    let mut matched_indices = Vec::new();
    let mut results_nodes = Vec::new();

    let title_label = t.label(
        tr(Msg::ImeSearchResults),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK_2),
    );
    results_nodes.push(title_label);

    if query.trim().is_empty() {
        let hint = t.label(
            tr(Msg::ImeSearchPlaceholder),
            TextStyle::new(FamilyId(0), 32.0)
                .at_density(s.density)
                .gray(INK_3),
        );
        results_nodes.push(t.pad(Insets::vh(s.density.px(24), 0), hint));
    } else {
        matched_indices = index.search(&query);

        if matched_indices.is_empty() {
            let empty = t.label(
                tr(Msg::ImeNoResults),
                TextStyle::new(FamilyId(0), 32.0)
                    .at_density(s.density)
                    .gray(INK_3),
            );
            results_nodes.push(t.pad(Insets::vh(s.density.px(24), 0), empty));
        } else {
            // Show up to 3 matched books to leave comfortable space above keyboard
            for (match_idx, &orig_idx) in matched_indices.iter().take(3).enumerate() {
                let book = &books[orig_idx];

                let cover = Cover::new(&book.title, &book.author, s.family)
                    .with_mark(book.mark())
                    .with_image(cover(orig_idx));
                let mark_node = t.custom(cover);
                let mark_box = t.fixed(
                    Some(s.density.px(108)),
                    Some(s.density.px(144)),
                    Some(mark_node),
                );

                // Book info
                let book_title = t.label(
                    &book.title,
                    TextStyle::new(FamilyId(0), 40.0)
                        .at_density(s.density)
                        .bold()
                        .gray(INK),
                );
                // The format comes from the library record, the same field
                // the library list draws (FIX-106): search used to print
                // "EPUB" for every book, PDF and CBZ included.
                let sub_str = format!("{} · {}", book.author, book.format);
                let book_sub = t.label(
                    &sub_str,
                    TextStyle::new(FamilyId(0), 32.0)
                        .at_density(s.density)
                        .gray(INK_2),
                );
                let info_col = t.add(
                    Node::column(s.density.px(8)).cross(Cross::Start).flex(1),
                    [book_title, book_sub],
                );

                let card_row = t.add(
                    Node::row(s.density.px(24)).cross(Cross::Center),
                    [mark_box, info_col],
                );

                let card_pad = t.pad(Insets::all(s.density.px(20)), card_row);
                let card_btn = t.add(
                    Node::new(Kind::Fixed { w: None, h: None })
                        .bg(242)
                        .border(0, 0, 24)
                        .action(action::RESULT_BASE + (match_idx as ActionId))
                        .feedback(Feedback::Invert),
                    [card_pad],
                );

                results_nodes.push(t.pad(Insets::new(s.density.px(12), 0, 0, 0), card_btn));
            }
        }
    }

    let results_col = t.add(Node::column(s.density.px(8)), results_nodes);
    let results_area = t.pad(
        Insets::new(s.density.px(20), margin(), 0, margin()),
        results_col,
    );
    let top_half = t.add(
        Node::column(s.density.px(12)).flex(1),
        [status_bar, header_bar, results_area],
    );

    // Keyboard at bottom
    let kb_node = build_keyboard(t, s, keyboard_state, tr(Msg::CommonSearch));

    let root = t.add(Node::column(0), [top_half, kb_node]);
    t.set_root(root);

    (
        root,
        SearchUi {
            keyboard: keyboard_state.clone(),
            back_btn,
            cancel_btn,
            index: index.clone(),
            matched_indices,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_test_books() -> Vec<Book> {
        vec![
            Book::new("紅樓夢", "曹雪芹", 0.35),
            Book::new("西游记", "吴承恩", 0.10),
            Book::new("三国演义", "罗贯中", 0.0),
            Book::new("水浒传", "施耐庵", 0.85),
            Book::new("War and Peace", "Leo Tolstoy", 0.5),
        ]
    }

    /// Every label under `id`, in tree order.
    fn texts(t: &Tree, id: NodeId) -> Vec<String> {
        let mut out = Vec::new();
        if let Kind::Text { para, .. } = &t.node(id).kind {
            out.push(para.text());
        }
        for child in &t.node(id).children {
            out.extend(texts(t, *child));
        }
        out
    }

    fn test_status() -> StatusLine<'static> {
        StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        }
    }

    /// FIX-106: a search result says the book's real format, the same one the
    /// library list shows. It used to say EPUB for every book.
    #[test]
    fn results_name_the_book_format_not_epub() {
        let books = vec![
            Book::new("Skan", "Jan Kowalski", 0.1).with_format("PDF"),
            Book::new("Komiks", "Anna Nowak", 0.2).with_format("CBZ"),
            Book::new("Notatki", "Bolesław Prus", 0.3).with_format("TXT"),
        ];
        let mut t = Tree::new();
        let s = Styles::new(FamilyId(0));
        let index = SearchIndex::build(&books);
        let mut keyboard = KeyboardState::new();
        keyboard.committed = "a".to_string();
        let (root, search) = build_search_screen(
            &mut t,
            &s,
            &books,
            &mut |_| None,
            &index,
            &keyboard,
            &test_status(),
        );
        assert_eq!(search.matched_indices.len(), 3);
        let shown = texts(&t, root);
        assert!(
            shown.contains(&"Jan Kowalski · PDF".to_string()),
            "{shown:?}"
        );
        assert!(shown.contains(&"Anna Nowak · CBZ".to_string()), "{shown:?}");
        assert!(
            shown.contains(&"Bolesław Prus · TXT".to_string()),
            "{shown:?}"
        );
        assert!(!shown.iter().any(|s| s.ends_with("· EPUB")), "{shown:?}");
    }

    /// The screen draws at most three results, so a library of a thousand
    /// books must not read a thousand thumbnails on the way in.
    #[test]
    fn only_the_results_drawn_ask_for_a_cover() {
        let mut t = Tree::new();
        let s = Styles::new(FamilyId(0));
        let books: Vec<Book> = (0..40)
            .map(|i| Book::new(&format!("Red Book {i}"), "An Author", 0.1))
            .collect();
        let index = SearchIndex::build(&books);
        let mut keyboard = KeyboardState::new();
        keyboard.set_mode(InputMode::English);
        for c in "red".chars() {
            keyboard.press_key(c);
        }
        let status = StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        };
        let mut asked = Vec::new();
        let (_, search) = build_search_screen(
            &mut t,
            &s,
            &books,
            &mut |i| {
                asked.push(i);
                None
            },
            &index,
            &keyboard,
            &status,
        );
        assert_eq!(search.matched_indices.len(), 40);
        assert_eq!(asked.len(), 3, "only the drawn results are asked");
    }

    #[test]
    fn search_screen_layout_and_matching() {
        let mut t = Tree::new();
        let s = Styles::new(FamilyId(0));
        let books = sample_test_books();
        let index = SearchIndex::build(&books);
        let state = KeyboardState::new();
        let status = StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        };

        // Initially empty
        let (_, mut search) = build_search_screen(
            &mut t,
            &s,
            &books,
            &mut |i| books[i].cover.clone(),
            &index,
            &state,
            &status,
        );
        assert!(search.matched_indices.is_empty());

        // 1. Uncommitted pinyin "hong" matches "紅樓夢"
        for c in "hong".chars() {
            search.handle_action(kb_action::KEY_BASE + (c as ActionId));
        }
        assert_eq!(search.keyboard.pinyin, "hong");
        let mut t_hong = Tree::new();
        let (_, search_hong) = build_search_screen(
            &mut t_hong,
            &s,
            &books,
            &mut |i| books[i].cover.clone(),
            &index,
            &search.keyboard,
            &status,
        );
        assert_eq!(search_hong.matched_indices.len(), 1);
        assert_eq!(search_hong.matched_indices[0], 0);

        // 2. Clear and type initials "hlm"
        search.keyboard.clear();
        for c in "hlm".chars() {
            search.handle_action(kb_action::KEY_BASE + (c as ActionId));
        }
        assert_eq!(search.keyboard.pinyin, "hlm");
        let mut t_hlm = Tree::new();
        let (_, search_hlm) = build_search_screen(
            &mut t_hlm,
            &s,
            &books,
            &mut |i| books[i].cover.clone(),
            &index,
            &search.keyboard,
            &status,
        );
        assert_eq!(search_hlm.matched_indices.len(), 1);
        assert_eq!(search_hlm.matched_indices[0], 0);

        // 3. Clear and type full pinyin prefix "honglou"
        search.keyboard.clear();
        for c in "honglou".chars() {
            search.handle_action(kb_action::KEY_BASE + (c as ActionId));
        }
        assert_eq!(search.keyboard.pinyin, "honglou");
        let mut t_hl = Tree::new();
        let (_, search_hl) = build_search_screen(
            &mut t_hl,
            &s,
            &books,
            &mut |i| books[i].cover.clone(),
            &index,
            &search.keyboard,
            &status,
        );
        assert_eq!(search_hl.matched_indices.len(), 1);
        assert_eq!(search_hl.matched_indices[0], 0);

        // 4. Select simplified candidate "红" matches traditional "紅樓夢"
        search.keyboard.clear();
        search.keyboard.committed = "红".to_string();
        let mut t_simp = Tree::new();
        let (_, search_simp) = build_search_screen(
            &mut t_simp,
            &s,
            &books,
            &mut |i| books[i].cover.clone(),
            &index,
            &search.keyboard,
            &status,
        );
        assert_eq!(search_simp.matched_indices.len(), 1);
        assert_eq!(search_simp.matched_indices[0], 0);

        // Cancel clears
        assert_eq!(search.handle_action(action::CANCEL), SearchOutcome::Changed);
        assert_eq!(search.keyboard.committed, "");
        assert_eq!(search.keyboard.pinyin, "");

        // Cancel again returns back
        assert_eq!(search.handle_action(action::CANCEL), SearchOutcome::Back);
    }
}
