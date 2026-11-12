// SPDX-License-Identifier: AGPL-3.0-or-later
//! "My highlights and notes": the library-wide list of highlights and
//! notes, newest first, with a pager.
//!
//! The layout follows the quotation library and the library lists of
//! `docs/ui/prototype-v2/reference/quotelibrary.png`: the same status bar,
//! the same 112 px header with the way back, rounded cards on the panel
//! gray and the pager at the foot. There is no bottom navigation.
//!
//! Every card shows the highlighted text in at most three lines, the note
//! when there is one, the book's title and the saved time. A card whose
//! book has left the library says so and does not jump anywhere.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use aristotle_i18n::{tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// Return to the "mine" screen.
    pub const BACK: ActionId = 1;
    /// The page before.
    pub const PREV_PAGE: ActionId = 2;
    /// The page after.
    pub const NEXT_PAGE: ActionId = 3;

    /// Tap a card to open its book at the highlight (its index in the whole
    /// list).
    pub const CARD_BASE: ActionId = 20;
}

/// The height of the status bar, in reference pixels.
const STATUS_H: i32 = 88;

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 112;

/// The content's top and bottom padding, in reference pixels.
const CONTENT_TOP: i32 = 24;
const CONTENT_BOTTOM: i32 = 28;

/// The height of the pager, in reference pixels.
const PAGER_H: i32 = 104;

/// The gap between two cards, in reference pixels.
const CARD_GAP: i32 = 20;

/// One row of the list.
pub struct Mark<'a> {
    /// The highlighted text, or the note's text.
    pub text: &'a str,
    /// The note attached to the highlight, when there is one.
    pub note: Option<&'a str>,
    /// The book's title.
    pub book: &'a str,
    /// The saved time, "YYYY-MM-DD HH:MM".
    pub time: &'a str,
    /// The book left the library: the row says so and cannot jump.
    pub missing: bool,
}

/// What the screen shows.
pub struct MarksPage<'a> {
    pub status: StatusLine<'a>,
    /// The page on the screen, from 1 to the count `build` lays out.
    pub page: usize,
    /// Every row of the list, newest first. Each page holds as many cards as
    /// their measured height leaves room for.
    pub entries: Vec<Mark<'a>>,
}

/// Handles to the nodes of the screen.
pub struct MarksUi {
    /// The loading area in the middle of the status line.
    pub loading: LoadingNodes,
    /// The paginated list of cards.
    pub list: NodeId,
    /// The page label in the pager.
    pub pager_label: NodeId,
    /// The page before.
    pub prev: NodeId,
    /// The page after.
    pub next: NodeId,
}

impl MarksUi {
    /// Builds the screen.
    pub fn build(_text: &mut TextEngine, s: &Styles, page: &MarksPage) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &page.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let status_fixed = t.fixed(None, Some(STATUS_H), Some(status));

        let header_row = header(&mut t, s);
        let header_fixed = t.fixed(None, Some(HEADER_H), Some(header_row));
        let header = t.pad(
            Insets::new(0, home_margin(), 0, home_margin()),
            header_fixed,
        );

        let list = if page.entries.is_empty() {
            empty_state(&mut t, s)
        } else {
            // `Pages` measures every card and fills a page until the next
            // one would not fit, so short rows do not waste the page and a
            // page of tall rows still leaves the pager its 104 px.
            let cards: Vec<NodeId> = page
                .entries
                .iter()
                .enumerate()
                .map(|(i, entry)| card(&mut t, s, entry, i as ActionId))
                .collect();
            let list = t.add(pages(CARD_GAP).flex(1), cards);
            t.set_page(list, page.page.saturating_sub(1));
            list
        };
        let (pager, pager_label, prev, next) = pager(&mut t, s, page.page);

        let content = t.add(Node::column(0).flex(1), [list, pager]);
        let content = t.pad(
            Insets::new(CONTENT_TOP, home_margin(), CONTENT_BOTTOM, home_margin()),
            content,
        );
        let content = t.add(Node::column(0).flex(1), [content]);

        let root = t.add(Node::column(0).bg(PAPER), [status_fixed, header, content]);
        t.set_root(root);
        (
            t,
            MarksUi {
                loading: bar.loading,
                list,
                pager_label,
                prev,
                next,
            },
        )
    }

    /// Updates the pager after layout: the count comes from the measured
    /// cards, which is known only once the list has been laid out.
    pub fn sync(&self, t: &mut Tree) {
        let (page, count) = t.page(self.list);
        t.set_text(
            self.pager_label,
            &tf!(
                Msg::LibraryPagerPageOf,
                page = page + 1,
                count = count.max(1)
            ),
        );
        t.set_icon_gray(self.prev, if page == 0 { RULE } else { INK });
        t.set_icon_gray(self.next, if page + 1 >= count { RULE } else { INK });
    }
}

/// The way back and the name of the screen.
fn header(t: &mut Tree, s: &Styles) -> NodeId {
    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let mut title_para = Paragraph::plain(
        tr(Msg::MineEntryMarks),
        s.font_lg().bold().line_height(1.2).tracking(4.0),
        Align::Left,
    );
    // The title is one line on every language: Polish "Moje wyróżnienia i
    // notatki" is wider than the header at 56 px and must end in an ellipsis
    // rather than wrap out of the fixed 112 px band.
    title_para.style.max_lines = Some(1);
    let title = t.text(title_para);
    let title = t.add(Node::column(0).flex(1), [title]);
    t.add(Node::row(20).cross(Cross::Center), [back, title])
}

/// One rounded card: the text, the note, and the book with its time.
///
/// `index` is the row's position in the whole list; its action id therefore
/// stays valid across pages.
fn card(t: &mut Tree, s: &Styles, entry: &Mark, index: ActionId) -> NodeId {
    let mut text = Paragraph::plain(
        entry.text,
        s.font_sm().gray(INK).line_height(1.45),
        Align::Left,
    );
    text.style.max_lines = Some(3);
    let text = t.text(text);

    let mut parts = vec![text];
    if let Some(note) = entry.note {
        parts.push(t.gap(0, 10));
        let mut line = Paragraph::plain(
            tf!(Msg::MarksNoteLine, note = note),
            s.font_xs().gray(INK_MID).line_height(1.35),
            Align::Left,
        );
        line.style.max_lines = Some(2);
        parts.push(t.text(line));
    }
    parts.push(t.gap(0, 12));

    let mut book = Paragraph::plain(
        entry.book,
        s.font_xs().gray(INK_MID).line_height(1.3),
        Align::Left,
    );
    book.style.max_lines = Some(1);
    let book_text = t.text(book);
    let book = t.add(Node::column(0).flex(1), [book_text]);
    let time = t.label(entry.time, s.font_xs().gray(INK_MID).line_height(1.3));
    let meta = t.add(Node::row(16).cross(Cross::Center), [book, time]);
    parts.push(meta);
    if entry.missing {
        parts.push(t.gap(0, 6));
        parts.push(t.label(
            tr(Msg::MarksMissing),
            s.font_xs().gray(INK_MID).line_height(1.3),
        ));
    }

    let content = t.add(Node::column(0), parts);
    let pad = t.pad(Insets::vh(24, 28), content);
    let mut node = Node::new(Kind::MinSize(Size::new(0, 196)))
        .bg(CARD)
        .border(0, CARD, 24);
    if !entry.missing {
        node = node
            .action(action::CARD_BASE + index)
            .feedback(Feedback::Invert);
    }
    t.add(node, [pad])
}

/// What a library without highlights and notes shows.
fn empty_state(t: &mut Tree, s: &Styles) -> NodeId {
    let icon = t.icon(Icon::Marks, 64, INK_MID);
    let title = t.text(Paragraph::plain(
        tr(Msg::MarksEmptyTitle),
        s.font_base().bold().gray(INK).line_height(1.3),
        Align::Center,
    ));
    let hint = t.text(Paragraph::plain(
        tr(Msg::MarksEmptyHint),
        s.font_xs().gray(INK_MID).line_height(1.35),
        Align::Center,
    ));
    let above = t.spacer(1);
    let below = t.spacer(1);
    let col = t.add(
        Node::column(16).cross(Cross::Center).flex(1),
        [above, icon, title, hint, below],
    );
    let col = t.pad(Insets::vh(48, 32), col);
    t.add(
        Node::new(Kind::Padding(Insets::new(0, 0, 0, 0)))
            .bg(CARD)
            .border(0, CARD, 24)
            .flex(1),
        [col],
    )
}

/// The pager: the way back and forward, the page, and the hint. The count is
/// written by [`MarksUi::sync`] once the cards have been measured.
fn pager(t: &mut Tree, s: &Styles, page: usize) -> (NodeId, NodeId, NodeId, NodeId) {
    let can_prev = page > 1;
    let prev_icon = t.icon(Icon::ChevronLeft, 44, if can_prev { INK } else { RULE });
    let prev_pad = t.pad(Insets::all(22), prev_icon);
    let prev = t.add(
        Node::row(0)
            .cross(Cross::Center)
            .action(action::PREV_PAGE)
            .feedback(Feedback::Invert),
        [prev_pad],
    );

    let page_of = tf!(Msg::LibraryPagerPageOf, page = page, count = 1);
    let page_label = t.text(Paragraph::plain(
        page_of,
        s.font_sm().gray(INK).line_height(1.3),
        Align::Center,
    ));
    let hint = t.text(Paragraph::plain(
        tr(Msg::LibraryPagerHintScroll),
        s.font_xs().gray(INK_MID).line_height(1.3),
        Align::Center,
    ));
    let center_col = t.add(Node::column(0).cross(Cross::Center), [page_label, hint]);
    let center = t.add(Node::column(0).cross(Cross::Center).flex(1), [center_col]);

    let next_icon = t.icon(Icon::ChevronRight, 44, RULE);
    let next_pad = t.pad(Insets::all(22), next_icon);
    let next = t.add(
        Node::row(0)
            .cross(Cross::Center)
            .action(action::NEXT_PAGE)
            .feedback(Feedback::Invert),
        [next_pad],
    );

    let row = t.add(Node::row(0).cross(Cross::Center), [prev, center, next]);
    let fixed = t.fixed(None, Some(PAGER_H), Some(row));
    (fixed, page_label, prev, next)
}

/// A sample of the screen for the preview and the tests: two rows, one of
/// them from a book that left the library.
pub fn sample(status: StatusLine<'static>) -> MarksPage<'static> {
    MarksPage {
        status,
        page: 1,
        entries: vec![
            Mark {
                text: "纸上得来终觉浅，绝知此事要躬行。",
                note: Some("读书要与实践结合。"),
                book: "冬夜读书示子聿",
                time: "2026-11-08 21:38",
                missing: false,
            },
            Mark {
                text: "读书有三到，谓心到、眼到、口到。",
                note: None,
                book: "训学斋规",
                time: "2026-10-28 22:07",
                missing: true,
            },
        ],
    }
}

/// The worst page the screen can be handed: every row at the three-line
/// highlight and two-line note limit, and a missing book, which adds the
/// tallest line of all. Four rows are more than one page holds, so the
/// pager test also covers a caller that passes more rows than fit.
pub fn worst_case(status: StatusLine<'static>) -> MarksPage<'static> {
    const TEXT: &str =
        "纸上得来终觉浅，绝知此事要躬行。古人学问无遗力，少壮工夫老始成。问渠那得清如许？为有源头活水来。";
    const NOTE: &str =
        "读书要与实践结合，不能只停留在字面上；每读到一处，都要想一想它在自己的生活里是什么样子。";
    MarksPage {
        status,
        page: 1,
        entries: vec![
            Mark {
                text: TEXT,
                note: Some(NOTE),
                book: "训学斋规",
                time: "2026-11-07 20:15",
                missing: true,
            },
            Mark {
                text: TEXT,
                note: Some(NOTE),
                book: "冬夜读书示子聿",
                time: "2026-11-08 21:38",
                missing: false,
            },
            Mark {
                text: TEXT,
                note: Some(NOTE),
                book: "观书有感",
                time: "2026-11-06 09:42",
                missing: false,
            },
            Mark {
                text: TEXT,
                note: Some(NOTE),
                book: "冬夜读书示子聿",
                time: "2026-11-05 22:07",
                missing: false,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Rect, SCREEN_HEIGHT, SCREEN_WIDTH};
    use crate::text::{FamilyId, Fonts};
    use crate::ui::Ui;

    fn build(page: MarksPage) -> Tree {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(FamilyId(0));
        MarksUi::build(&mut text, &s, &page).0
    }

    fn laid_out(page: MarksPage) -> (Ui, MarksUi) {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(FamilyId(0));
        let (tree, view) = MarksUi::build(&mut text, &s, &page);
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        view.sync(&mut ui.tree);
        ui.layout();
        (ui, view)
    }

    fn has_action(tree: &Tree, action: ActionId) -> bool {
        action_node(tree, action).is_some()
    }

    fn action_node(tree: &Tree, action: ActionId) -> Option<NodeId> {
        (0..tree.len())
            .map(|i| NodeId(i as u32))
            .find(|&id| tree.node(id).action == Some(action))
    }

    fn action_rect(tree: &Tree, action: ActionId) -> Option<Rect> {
        action_node(tree, action).map(|id| tree.rect(id))
    }

    fn status() -> StatusLine<'static> {
        StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        }
    }

    #[test]
    fn the_list_builds_with_a_way_back_and_a_pager() {
        let tree = build(sample(status()));
        assert!(has_action(&tree, action::BACK));
        assert!(has_action(&tree, action::PREV_PAGE));
        assert!(has_action(&tree, action::NEXT_PAGE));
        assert!(has_action(&tree, action::CARD_BASE));
        // The sample's second row is from a deleted book.
        assert!(!has_action(&tree, action::CARD_BASE + 1));
    }

    #[test]
    fn a_deleted_book_has_no_card_action() {
        let page = MarksPage {
            status: status(),
            page: 1,
            entries: vec![
                Mark {
                    text: "still here",
                    note: None,
                    book: "Kept",
                    time: "2026-11-09 10:00",
                    missing: false,
                },
                Mark {
                    text: "gone",
                    note: None,
                    book: "Deleted",
                    time: "2026-11-08 10:00",
                    missing: true,
                },
            ],
        };
        let tree = build(page);
        assert!(has_action(&tree, action::CARD_BASE));
        assert!(!has_action(&tree, action::CARD_BASE + 1));
    }

    #[test]
    fn an_empty_library_builds_the_empty_state() {
        let page = MarksPage {
            status: status(),
            page: 1,
            entries: Vec::new(),
        };
        let tree = build(page);
        assert!(has_action(&tree, action::BACK));
        assert!(!has_action(&tree, action::CARD_BASE));
    }

    /// The card actions whose rect is on the page (off-page cards have no
    /// rect).
    fn visible_cards(tree: &Tree) -> Vec<(ActionId, Rect)> {
        (0..64)
            .map(|i| {
                let action = action::CARD_BASE + i;
                (action, action_rect(tree, action))
            })
            .filter_map(|(action, rect)| rect.map(|r| (action, r)).filter(|(_, r)| !r.is_empty()))
            .collect()
    }

    #[test]
    fn a_full_page_of_worst_case_cards_keeps_the_pager_on_screen() {
        // Four rows, each three lines of highlight and two lines of note.
        // The measured height leaves room for two of them; the pager must
        // keep its 104 px and the last card must not run under it.
        assert_eq!(worst_case(status()).entries.len(), 4);
        let (ui, view) = laid_out(worst_case(status()));
        let tree = &ui.tree;
        let (_, count) = tree.page(view.list);
        assert_eq!(count, 2, "two worst-case cards per page");

        let pager = action_rect(tree, action::NEXT_PAGE).expect("pager");
        assert!(pager.h > 0, "{pager:?}");
        assert!(pager.bottom() <= SCREEN_HEIGHT, "{pager:?}");

        let cards = visible_cards(tree);
        assert_eq!(cards.len(), 2, "the page shows two cards: {cards:?}");
        let last = cards.last().expect("last card").1;
        assert!(last.h > 0, "{last:?}");
        assert!(last.bottom() <= pager.y, "{last:?} above {pager:?}");
    }

    #[test]
    fn short_cards_fill_the_page_instead_of_two_per_page() {
        // One-line highlights with no note: their measured height is about
        // half the worst case, so five fit where two tall cards do.
        let page = MarksPage {
            status: status(),
            page: 1,
            entries: (0..8)
                .map(|_| Mark {
                    text: "短句",
                    note: None,
                    book: "冬夜读书示子聿",
                    time: "2026-11-08 21:38",
                    missing: false,
                })
                .collect(),
        };
        let (ui, view) = laid_out(page);
        let tree = &ui.tree;
        let (_, count) = tree.page(view.list);
        assert_eq!(count, 2, "eight short cards take two pages, not four");
        let cards = visible_cards(tree);
        assert!(
            cards.len() >= 4,
            "short cards must fill the page: {cards:?}"
        );
        let pager = action_rect(tree, action::NEXT_PAGE).expect("pager");
        let last = cards.last().expect("last card").1;
        assert!(last.bottom() <= pager.y, "{last:?} above {pager:?}");
    }

    #[test]
    fn a_long_polish_title_stays_on_one_line() {
        aristotle_i18n::with_language("pl", || {
            let title_text = tr(Msg::MineEntryMarks);
            let (ui, _) = laid_out(sample(status()));
            let id = (0..ui.tree.len())
                .map(|i| NodeId(i as u32))
                .find(|&id| match &ui.tree.node(id).kind {
                    Kind::Text { para, .. } => para
                        .spans
                        .first()
                        .is_some_and(|span| span.text == title_text),
                    _ => false,
                })
                .expect("header title");
            let Kind::Text { para, .. } = &ui.tree.node(id).kind else {
                unreachable!()
            };
            assert_eq!(para.style.max_lines, Some(1));
            let rect = ui.tree.rect(id);
            assert!(rect.h < HEADER_H, "{rect:?}");
            assert!(rect.right() <= SCREEN_WIDTH, "{rect:?}");
        });
    }
}
