// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reader's table of contents: the book's chapters as an outline
//! that can be folded, and the screen that shows it.
//!
//! Two parts, kept apart on purpose:
//!
//! - The model ([`Contents`]) is pure. It holds the book's entries, the
//!   outline they make, and which groups are open. It knows nothing
//!   about the screen, the book or the reader's position, so it can be
//!   tested on the host.
//! - The screen ([`build`]) draws the rows, the fold markers and the
//!   pager. It decides nothing: the reader app tells it what to show
//!   and what a tap on a row means.
//!
//! The list is folded by default: only the group the reader is in is
//! open, so the contents open at the chapter being read instead of at
//! the first chapter of the book.

use crate::geom::Insets;
use crate::icons::Icon;
use crate::text::{Align, Paragraph};
use crate::theme::{margin, Styles, INK, INK_2, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// The way back to the page.
    pub const CLOSE: ActionId = 22_000;
    /// The page before.
    pub const PREV_PAGE: ActionId = 22_001;
    /// The page after.
    pub const NEXT_PAGE: ActionId = 22_002;
    /// The first chapter row. The rows follow, one id each, so the
    /// chapters of a long book all have their own id.
    ///
    /// The base is the reader's own list base (`overlay::act::ITEM`),
    /// so a script that taps "the first chapter" finds it here too.
    pub const ITEM_BASE: ActionId = 2000;
    /// How many rows the chapter ids cover.
    pub const ITEM_LIMIT: usize = 10_000;
    /// The first fold marker. The markers follow, one id each, in a band
    /// of their own, so a marker never means "a chapter".
    pub const TOGGLE_BASE: ActionId = 12_000;
    /// How many rows the marker ids cover.
    pub const TOGGLE_LIMIT: usize = 10_000;
}

/// What an action of the contents list means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TocAction {
    /// The way back to the page.
    Close,
    /// The page before.
    PrevPage,
    /// The page after.
    NextPage,
    /// The chapter on a row.
    Select(usize),
    /// The fold marker on a row.
    Toggle(usize),
}

/// What an action id of the contents list means, if it is one.
///
/// The ids come in bands that do not overlap: the chapters and the fold
/// markers have their own, and the buttons that turn the pages have
/// theirs. An id that belongs to another screen is [`None`], so the
/// reader app can tell the contents list's actions from the rest
/// without knowing the numbers.
///
/// The band of the chapters is wide (thousands of chapters), so it
/// covers the ids the reader uses while another overlay is up. The
/// reader app therefore decodes only while the contents list is open.
pub fn decode(id: ActionId) -> Option<TocAction> {
    match id {
        action::CLOSE => return Some(TocAction::Close),
        action::PREV_PAGE => return Some(TocAction::PrevPage),
        action::NEXT_PAGE => return Some(TocAction::NextPage),
        _ => {}
    }
    if (action::ITEM_BASE..action::ITEM_BASE + action::ITEM_LIMIT as ActionId).contains(&id) {
        return Some(TocAction::Select((id - action::ITEM_BASE) as usize));
    }
    if (action::TOGGLE_BASE..action::TOGGLE_BASE + action::TOGGLE_LIMIT as ActionId).contains(&id) {
        return Some(TocAction::Toggle((id - action::TOGGLE_BASE) as usize));
    }
    None
}

/// One entry of the book's table of contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The chapter's title.
    pub title: String,
    /// 0 for a top-level chapter, 1 for a chapter under it, and so on.
    pub depth: usize,
    /// The entry's own number in the book's list; the book jumps by it.
    pub index: usize,
    /// The page the chapter starts on, when the engine knows it.
    pub page: Option<usize>,
}

/// The table of contents as an outline that can be folded.
#[derive(Clone, Debug)]
pub struct Contents {
    entries: Vec<Entry>,
    /// The entries under each entry.
    children: Vec<Vec<usize>>,
    /// The top-level entries.
    roots: Vec<usize>,
    /// Whether each entry's children are shown.
    open: Vec<bool>,
    /// The entries on screen, in order.
    rows: Vec<usize>,
    /// The number of the entry the reader is in.
    current: Option<usize>,
}

impl Contents {
    /// The contents of a book: its entries in the book's order, and the
    /// entry the reader is in (by the book's number).
    ///
    /// The outline is the entries' depths: an entry at depth 1 hangs
    /// under the last entry at depth 0, an entry at depth 2 under the
    /// last at depth 1, and so on. A depth that jumps (a book whose
    /// contents start deeper) hangs on the deepest entry open, so no
    /// entry is lost.
    ///
    /// Only the reader's own place is open: the entries above it, so
    /// the chapter is on the screen, and the chapter itself, so its own
    /// children are too. Every other group stays folded.
    pub fn new(entries: Vec<Entry>, current: Option<usize>) -> Self {
        let n = entries.len();
        let mut parent: Vec<Option<usize>> = vec![None; n];
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut roots: Vec<usize> = Vec::new();
        let mut above: Vec<usize> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            let depth = e.depth.min(above.len());
            above.truncate(depth);
            if depth == 0 {
                roots.push(i);
            } else {
                let p = above[depth - 1];
                parent[i] = Some(p);
                children[p].push(i);
            }
            above.push(i);
        }
        let mut open = vec![false; n];
        let mut node = current.and_then(|c| entries.iter().position(|e| e.index == c));
        while let Some(i) = node {
            if !children[i].is_empty() {
                open[i] = true;
            }
            node = parent[i];
        }
        let mut contents = Contents {
            entries,
            children,
            roots,
            open,
            rows: Vec::new(),
            current,
        };
        contents.collect();
        contents
    }

    /// The entries on screen, in order: their places in the book's list.
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    /// The entry at a place in the book's list.
    pub fn entry(&self, at: usize) -> Option<&Entry> {
        self.entries.get(at)
    }

    /// Whether that entry has children to fold.
    pub fn has_children(&self, at: usize) -> bool {
        self.children.get(at).is_some_and(|c| !c.is_empty())
    }

    /// Whether that entry's children are on screen.
    pub fn is_open(&self, at: usize) -> bool {
        self.open.get(at).copied().unwrap_or(false)
    }

    /// The entry on a row.
    pub fn row_entry(&self, row: usize) -> Option<&Entry> {
        self.rows.get(row).and_then(|&at| self.entry(at))
    }

    /// The book's number of the entry on a row: what the book jumps by.
    pub fn row_index(&self, row: usize) -> Option<usize> {
        self.row_entry(row).map(|e| e.index)
    }

    /// The row the reader's chapter is on.
    pub fn current_row(&self) -> Option<usize> {
        let current = self.current?;
        self.rows
            .iter()
            .position(|&at| self.entries[at].index == current)
    }

    /// The book's number of the reader's chapter.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Folds the children of the entry on a row open or shut. Returns
    /// whether anything changed; a row without children never changes.
    pub fn toggle_row(&mut self, row: usize) -> bool {
        let Some(&at) = self.rows.get(row) else {
            return false;
        };
        if !self.has_children(at) {
            return false;
        }
        self.open[at] = !self.open[at];
        self.collect();
        true
    }

    /// How many entries the book has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the book has no contents at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The rows on screen: every entry, and the children of the open
    /// ones, in the order they are read.
    fn collect(&mut self) {
        self.rows.clear();
        let mut stack: Vec<usize> = self.roots.iter().rev().copied().collect();
        while let Some(at) = stack.pop() {
            self.rows.push(at);
            if !self.open[at] {
                continue;
            }
            for &child in self.children[at].iter().rev() {
                stack.push(child);
            }
        }
    }
}

/// The nodes of the contents screen.
#[derive(Clone, Copy)]
pub struct TocUi {
    /// The whole screen.
    pub root: NodeId,
    /// The paged list of chapters.
    pub pages: NodeId,
    /// The page label in the pager.
    pub pager: NodeId,
    /// The "previous page" button.
    pub prev: NodeId,
    /// The "next page" button.
    pub next: NodeId,
    /// The arrow on the "previous page" button.
    pub prev_icon: NodeId,
    /// The word on the "previous page" button.
    pub prev_label: NodeId,
    /// The arrow on the "next page" button.
    pub next_icon: NodeId,
    /// The word on the "next page" button.
    pub next_label: NodeId,
}

/// Builds the contents screen over `t`: the way back and the title, the
/// chapters, and the pager at the foot.
pub fn build(t: &mut Tree, st: &Styles, contents: &Contents, title: &str, empty: &str) -> TocUi {
    let m = margin();
    let back_icon = t.icon(Icon::ArrowLeft, 44, INK);
    let back_label = t.label(tr(Msg::CommonBack), st.ui());
    let g = t.gap(12, 1);
    let back = t.add(Node::row(0), [back_icon, g, back_label]);
    let back = t.pad(Insets::vh(0, 16), back);
    let back = t.button(Node::plain(), back, action::CLOSE);
    let mut tl = Paragraph::plain(title, st.title(), Align::Center);
    tl.style.max_lines = Some(1);
    let tl = t.text(tl);
    let tl = t.add(Node::column(0).flex(1), [tl]);
    let pad = t.gap(160, 1);
    let bar = t.add(Node::row(0), [back, tl, pad]);
    let bar = t.fixed(None, Some(120), Some(bar));
    let bar = t.pad(Insets::vh(0, m - 16), bar);
    let line = t.rule(2, INK);

    let mut items = Vec::new();
    if contents.rows().is_empty() {
        let l = t.label(empty, st.ui().gray(INK_2));
        let l = t.pad(Insets::vh(40, 0), l);
        items.push(l);
    }
    for (row, &at) in contents.rows().iter().enumerate() {
        let Some(entry) = contents.entry(at) else {
            continue;
        };
        let style = if Some(entry.index) == contents.current() {
            st.ui_bold()
        } else {
            st.ui()
        };
        let mut p = Paragraph::plain(entry.title.clone(), style, Align::Left);
        p.style.max_lines = Some(2);
        let text = t.text(p);
        let mut col = vec![text];
        if let Some(page) = entry.page {
            let mut mp = Paragraph::plain((page + 1).to_string(), st.small(), Align::Left);
            mp.style.max_lines = Some(1);
            col.push(t.text(mp));
        }
        let col = t.add(Node::column(6), col);
        let col = t.pad(Insets::new(18, 0, 18, 36 * entry.depth.min(4) as i32), col);
        // The chapter takes the room the fold marker leaves, so the
        // marker always has its own space at the right of the row.
        let col = t.add(Node::column(0).flex(1), [col]);
        let hair = t.rule(1, RULE);
        let mut cells = vec![col];
        if contents.has_children(at) {
            // The fold marker, at the right of the row: it is its own
            // button, so folding never jumps to the chapter by mistake.
            let icon = if contents.is_open(at) {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            let mark = t.icon(icon, 32, INK);
            let mark = t.button(Node::plain(), mark, action::TOGGLE_BASE + row as ActionId);
            cells.push(mark);
        }
        let body = t.add(Node::row(8).cross(Cross::Center), cells);
        let body = t.add(Node::column(0), [body, hair]);
        let body = t.add(
            Node::new(Kind::Padding(Insets::vh(0, m)))
                .action(action::ITEM_BASE + row as ActionId)
                .feedback(Feedback::Outline),
            [body],
        );
        items.push(body);
    }
    let list = t.add(pages(0).flex(1), items);
    let pages_node = list;

    // The pager: the page before, the page, the page after. The buttons
    // say which way they go and grey out at the ends of the list.
    let prev_icon = t.icon(Icon::ChevronLeft, 36, INK);
    let prev_label = t.label(tr(Msg::ReaderTocPrevPage), st.ui());
    let prev_body = t.add(Node::row(6).cross(Cross::Center), [prev_icon, prev_label]);
    let prev = t.button(Node::plain(), prev_body, action::PREV_PAGE);
    let pager = t.label("", st.small());
    let pager_c = t.align(0.5, 0.5, pager);
    let pager_c = t.fixed(None, Some(80), Some(pager_c));
    let pager_c = t.add(Node::column(0).flex(1), [pager_c]);
    let next_label = t.label(tr(Msg::ReaderTocNextPage), st.ui());
    let next_icon = t.icon(Icon::ChevronRight, 36, INK);
    let next_body = t.add(Node::row(6).cross(Cross::Center), [next_label, next_icon]);
    let next = t.button(Node::plain(), next_body, action::NEXT_PAGE);
    let foot = t.add(Node::row(8).cross(Cross::Center), [prev, pager_c, next]);
    let foot = t.pad(Insets::new(0, m - 16, 24, m - 16), foot);

    let root = t.add(Node::column(0).bg(255), [bar, line, list, foot]);
    TocUi {
        root,
        pages: pages_node,
        pager,
        prev,
        next,
        prev_icon,
        prev_label,
        next_icon,
        next_label,
    }
}

impl TocUi {
    /// Labels the pager and greys out the buttons that have nowhere to
    /// go: the page before on the first page, the page after on the
    /// last one.
    pub fn sync(&self, t: &mut Tree) {
        let (page, count) = t.page(self.pages);
        let label = if count > 1 {
            format!("{} / {}", page + 1, count)
        } else {
            String::new()
        };
        t.set_text(self.pager, &label);
        let first = page == 0;
        let last = page + 1 >= count;
        t.set_icon_gray(self.prev_icon, if first { RULE } else { INK });
        t.set_text_gray(self.prev_label, if first { INK_2 } else { INK });
        t.set_icon_gray(self.next_icon, if last { RULE } else { INK });
        t.set_text_gray(self.next_label, if last { INK_2 } else { INK });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, depth: usize, index: usize) -> Entry {
        Entry {
            title: title.into(),
            depth,
            index,
            page: Some(index * 10),
        }
    }

    /// A book with three chapters, the second one holding two sections.
    fn book() -> Vec<Entry> {
        vec![
            entry("One", 0, 0),
            entry("Two", 0, 1),
            entry("Two.1", 1, 2),
            entry("Two.2", 1, 3),
            entry("Three", 0, 4),
        ]
    }

    fn titles(contents: &Contents) -> Vec<&str> {
        contents
            .rows()
            .iter()
            .map(|&at| contents.entry(at).unwrap().title.as_str())
            .collect()
    }

    #[test]
    fn a_book_without_subtitles_stays_as_it_is() {
        let contents = Contents::new(
            vec![entry("One", 0, 0), entry("Two", 0, 1), entry("Three", 0, 2)],
            Some(1),
        );
        assert_eq!(titles(&contents), vec!["One", "Two", "Three"]);
        assert!(!contents.has_children(0));
    }

    #[test]
    fn only_the_group_of_the_current_chapter_is_open() {
        // The reader is in "Two.2": its group is open, the others folded.
        let contents = Contents::new(book(), Some(3));
        assert_eq!(
            titles(&contents),
            vec!["One", "Two", "Two.1", "Two.2", "Three"]
        );
        assert!(contents.is_open(1));
        assert!(!contents.is_open(0));
        assert!(!contents.is_open(4));
    }

    #[test]
    fn a_current_chapter_with_no_children_leaves_the_groups_folded() {
        let contents = Contents::new(book(), Some(0));
        assert_eq!(titles(&contents), vec!["One", "Two", "Three"]);
        assert!(!contents.has_children(0));
        assert!(!contents.is_open(1));
    }

    #[test]
    fn the_current_group_opens_its_own_children() {
        // The reader is in the chapter "Two" itself, not in one of its
        // sections: the group opens, so the sections show.
        let contents = Contents::new(book(), Some(1));
        assert_eq!(
            titles(&contents),
            vec!["One", "Two", "Two.1", "Two.2", "Three"]
        );
        assert!(contents.is_open(1));
    }

    #[test]
    fn a_book_without_a_current_chapter_is_all_folded() {
        let contents = Contents::new(book(), None);
        assert_eq!(titles(&contents), vec!["One", "Two", "Three"]);
    }

    #[test]
    fn an_empty_book_has_no_rows() {
        let contents = Contents::new(Vec::new(), None);
        assert!(contents.is_empty());
        assert_eq!(contents.len(), 0);
        assert!(contents.rows().is_empty());
        assert!(contents.current_row().is_none());
    }

    #[test]
    fn folding_a_group_hides_its_children_and_keeps_the_rest() {
        let mut contents = Contents::new(book(), Some(3));
        assert!(contents.toggle_row(1));
        assert_eq!(titles(&contents), vec!["One", "Two", "Three"]);
        // The chapter's own row is still there, and it still jumps to the
        // same place in the book.
        assert_eq!(contents.row_index(1), Some(1));
        // Opening it again brings the sections back.
        assert!(contents.toggle_row(1));
        assert_eq!(
            titles(&contents),
            vec!["One", "Two", "Two.1", "Two.2", "Three"]
        );
    }

    #[test]
    fn a_row_without_children_does_not_fold() {
        let mut contents = Contents::new(book(), Some(3));
        assert!(!contents.toggle_row(0));
        assert_eq!(
            titles(&contents),
            vec!["One", "Two", "Two.1", "Two.2", "Three"]
        );
    }

    #[test]
    fn the_rows_keep_the_places_in_the_book() {
        let contents = Contents::new(book(), Some(3));
        let indices: Vec<usize> = contents
            .rows()
            .iter()
            .map(|&at| contents.entry(at).unwrap().index)
            .collect();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
        assert_eq!(contents.row_index(2), Some(2));
        assert_eq!(contents.current_row(), Some(3));
    }

    #[test]
    fn a_deeper_book_folds_the_whole_chain_of_the_current_chapter() {
        let entries = vec![
            entry("One", 0, 0),
            entry("One.1", 1, 1),
            entry("One.1.1", 2, 2),
            entry("Two", 0, 3),
        ];
        let contents = Contents::new(entries, Some(2));
        // The chapter and every group above it are open.
        assert_eq!(titles(&contents), vec!["One", "One.1", "One.1.1", "Two"]);
        assert!(contents.is_open(0));
        assert!(contents.is_open(1));
        assert!(!contents.is_open(3));
    }

    #[test]
    fn a_depth_that_jumps_keeps_every_entry() {
        // A book whose contents start at depth 1: the entries hang on
        // the deepest one open, so none is lost.
        let entries = vec![entry("One", 1, 0), entry("Two", 2, 1), entry("Three", 0, 2)];
        let contents = Contents::new(entries, Some(1));
        assert_eq!(titles(&contents), vec!["One", "Two", "Three"]);
        assert!(contents.is_open(0));
    }

    // --- The action ids ------------------------------------------------

    #[test]
    fn the_ids_of_the_chapters_decode_back_to_their_rows() {
        // A short directory, a middle one and a long one.
        for row in 0u32..=20 {
            assert_eq!(
                decode(action::ITEM_BASE + row),
                Some(TocAction::Select(row as usize)),
                "row {row}"
            );
        }
        for row in 21u32..=99 {
            assert_eq!(
                decode(action::ITEM_BASE + row),
                Some(TocAction::Select(row as usize))
            );
        }
        for row in 100u32..=500 {
            assert_eq!(
                decode(action::ITEM_BASE + row),
                Some(TocAction::Select(row as usize))
            );
        }
    }

    #[test]
    fn the_ids_of_the_markers_decode_back_to_their_rows() {
        for row in 0u32..=20 {
            assert_eq!(
                decode(action::TOGGLE_BASE + row),
                Some(TocAction::Toggle(row as usize)),
                "row {row}"
            );
        }
        for row in 21u32..=99 {
            assert_eq!(
                decode(action::TOGGLE_BASE + row),
                Some(TocAction::Toggle(row as usize))
            );
        }
        for row in 100u32..=500 {
            assert_eq!(
                decode(action::TOGGLE_BASE + row),
                Some(TocAction::Toggle(row as usize))
            );
        }
    }

    #[test]
    fn the_buttons_of_the_pager_decode_to_themselves() {
        assert_eq!(decode(action::CLOSE), Some(TocAction::Close));
        assert_eq!(decode(action::PREV_PAGE), Some(TocAction::PrevPage));
        assert_eq!(decode(action::NEXT_PAGE), Some(TocAction::NextPage));
    }

    #[test]
    fn no_id_means_two_things() {
        // The chapter ids and the marker ids are in bands of their own:
        // an id of one is never the other.
        for row in 0u32..=500 {
            let chapter = action::ITEM_BASE + row;
            let marker = action::TOGGLE_BASE + row;
            assert_eq!(decode(chapter), Some(TocAction::Select(row as usize)));
            assert_eq!(decode(marker), Some(TocAction::Toggle(row as usize)));
            assert_ne!(decode(chapter), Some(TocAction::Toggle(row as usize)));
            assert_ne!(decode(marker), Some(TocAction::Select(row as usize)));
        }
        // The bands do not touch each other either.
        assert!(action::ITEM_BASE + action::ITEM_LIMIT as ActionId <= action::TOGGLE_BASE);
        assert!(action::TOGGLE_BASE + action::TOGGLE_LIMIT as ActionId > action::TOGGLE_BASE);
    }

    #[test]
    fn the_ids_of_other_screens_are_not_ours() {
        // The reading menu's ids (below 100) are not ours.
        for id in 0..100u32 {
            assert_eq!(decode(id), None, "id {id} belongs to the reading menu");
        }
        // The reader's own buttons are above the menu and below our
        // bands, so they are not ours either.
        for id in [1001u32, 1002, 1009, 1100, 1200] {
            assert_eq!(decode(id), None, "id {id} belongs to another screen");
        }
        // The ids the reader uses while another overlay is up (the "go
        // to" sheet, the navigation) fall inside the band of the
        // chapters, so they are decoded only while the contents list is
        // open: the reader app checks the overlay before it decodes.
        assert_eq!(decode(3100), Some(TocAction::Select(1100)));
    }
}
