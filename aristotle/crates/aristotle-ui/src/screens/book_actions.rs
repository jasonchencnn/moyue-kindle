// SPDX-License-Identifier: AGPL-3.0-or-later
//! The book actions sheet: what opens when a book is held down.
//!
//! The sheet comes up from the bottom of the screen, with no dimming
//! scrim, the way `docs/ui/prototype-v2/mcode/LibraryActions.dc.html`
//! draws it: the header with the cover, the title, the author and the
//! book's facts, then the actions, and the deletion behind a second
//! question.

use crate::geom::Insets;
use crate::icons::Icon;
use crate::text::{Align, Paragraph};
use crate::theme::{Styles, CARD, INK, INK_2, PAPER, RULE};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::{Cover, GrayImage};
use aristotle_i18n::{fmt_duration, tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// Close the sheet, changing nothing.
    pub const CANCEL: ActionId = 500;
    /// Show what the book is and how long it has been read.
    pub const DETAILS: ActionId = 501;
    /// Mark the book as finished, or as not finished.
    pub const TOGGLE_FINISHED: ActionId = 502;
    /// Ask whether the local file may be deleted.
    pub const DELETE_PROMPT: ActionId = 503;
    /// Delete the local file.
    pub const CONFIRM_DELETE: ActionId = 504;
    /// Go back to the menu.
    pub const BACK_TO_MENU: ActionId = 505;
}

/// Which part of the sheet is on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionStep {
    /// The actions themselves.
    Menu,
    /// What the book is and how long it has been read.
    Details,
    /// The question before the local file is deleted.
    ConfirmDelete,
}

/// What the sheet says about the book.
#[derive(Clone, Debug)]
pub struct BookActionInfo {
    /// The book's id in the library index.
    pub id: String,
    pub title: String,
    pub author: String,
    /// "EPUB", "TXT", ...
    pub format: String,
    /// "2.4 MB"
    pub size: String,
    /// The book's path in the library, e.g. "文学/红楼梦.epub".
    pub path: String,
    /// Where the file is on the device.
    pub full_path: String,
    /// How much of the book has been read, from 0 to 1.
    pub progress: f32,
    /// The book is marked as read to the end.
    pub finished: bool,
    /// The book has not been opened yet.
    pub unread: bool,
    /// How long the book has been read, in seconds.
    pub read_secs: u64,
    /// The day the book was first opened.
    pub start_date: Option<String>,
    /// The cover image, when the library shows covers.
    pub cover: Option<GrayImage>,
}

impl BookActionInfo {
    /// The line under the author's name: "EPUB · 2.4 MB · 已读 68% ·
    /// 累计 9 小时 12 分钟".
    pub fn summary(&self) -> String {
        let mut out = format!("{} · {}", self.format, self.size);
        out.push_str(" · ");
        if self.finished {
            out.push_str(tr(Msg::LibraryBookFinished));
        } else if self.unread || self.progress <= 0.0 {
            out.push_str(tr(Msg::LibraryBookUnread));
        } else {
            let pct = (self.progress * 100.0).floor() as i64;
            out.push_str(&tf!(Msg::LibraryReadPercent, percent = pct));
        }
        if self.read_secs >= 60 {
            out.push_str(" · ");
            let dur = fmt_duration((self.read_secs / 60) as u32);
            out.push_str(&tf!(Msg::LibraryTotalTime, duration = &dur));
        }
        out
    }

    /// How long the book has been read, as it reads on the screen.
    pub fn read_time(&self) -> String {
        if self.read_secs >= 60 {
            fmt_duration((self.read_secs / 60) as u32)
        } else {
            tf!(Msg::FormatMinutes, m = 0)
        }
    }
}

/// The sheet's nodes, so the app can fill it and change what it shows.
#[derive(Clone, Copy, Debug)]
pub struct ActionsSheet {
    /// The overlay: put it on top of the screen's tree.
    pub root: NodeId,
    /// The node whose children hold the sheet's content.
    pub content: NodeId,
}

/// Builds the sheet and puts it on top of the tree.
///
/// The overlay covers the whole screen: a transparent shield that closes
/// the sheet when it is tapped, and the sheet itself at the bottom.
pub fn build_book_actions(
    t: &mut Tree,
    s: &Styles,
    info: &BookActionInfo,
    step: ActionStep,
) -> ActionsSheet {
    let content = t.add(Node::column(0).cross(Cross::Stretch), []);
    let padded = t.pad(Insets::new(28, 88, 40, 88), content);
    // The sheet's top corners are rounded; the bottom ones run into the
    // edge of the screen. The tone is the prototype's #F2F2F2.
    let sheet = t.add(Node::column(0).bg(CARD).top_border(0, CARD, 32), [padded]);
    let shield = t.add(
        Node::column(0)
            .flex(1)
            .action(action::CANCEL)
            .feedback(Feedback::None),
        [],
    );
    let root = t.add(Node::column(0), [shield, sheet]);
    let actions = ActionsSheet { root, content };
    set_book_actions(t, s, &actions, info, step);
    actions
}

/// Fills the sheet with the header and the actions of one step.
pub fn set_book_actions(
    t: &mut Tree,
    s: &Styles,
    actions: &ActionsSheet,
    info: &BookActionInfo,
    step: ActionStep,
) {
    let header = header_block(t, s, info, chip_title(step));
    let gap = t.gap(1, 24);
    let body = match step {
        ActionStep::Menu => menu_block(t, s, info),
        ActionStep::Details => details_block(t, s, info),
        ActionStep::ConfirmDelete => confirm_block(t, s, info),
    };
    t.set_children(actions.content, vec![header, gap, body]);
}

/// The name of the sheet's mark: what it is about.
fn chip_title(step: ActionStep) -> &'static str {
    match step {
        ActionStep::Menu | ActionStep::ConfirmDelete => tr(Msg::LibraryActionMenuTitle),
        ActionStep::Details => tr(Msg::LibraryDetailsTitle),
    }
}

/// The cover, the title, the author, the book's facts and its path.
fn header_block(t: &mut Tree, s: &Styles, info: &BookActionInfo, chip: &str) -> NodeId {
    let cover = Cover::new(&info.title, &info.author, s.family).with_image(info.cover.clone());
    let cover = t.custom(cover);
    let cover = t.fixed(Some(150), Some(200), Some(cover));

    let chip_text = t.label(chip, s.ui());
    let chip_pad = t.pad(Insets::new(12, 20, 12, 20), chip_text);
    let chip = t.add(
        Node::column(0)
            .cross(Cross::Start)
            .bg(PAPER)
            .border(2, INK, 16),
        [chip_pad],
    );

    let mut tp = Paragraph::plain(&info.title, s.headline(), Align::Left);
    tp.style.max_lines = Some(2);
    let title = t.text(tp);

    let mut ap = Paragraph::plain(&info.author, s.ui().gray(INK_2), Align::Left);
    ap.style.max_lines = Some(1);
    let author = t.text(ap);

    let summary = t.label(&info.summary(), s.ui().gray(INK_2));

    let mut pp = Paragraph::plain(&info.path, s.ui().gray(INK_2), Align::Left);
    pp.style.max_lines = Some(1);
    let path = t.text(pp);

    let g1 = t.gap(1, 16);
    let g2 = t.gap(1, 8);
    let words = t.add(
        Node::column(0).cross(Cross::Start).flex(1),
        [chip, g1, title, g2, author, g2, summary, g2, path],
    );

    t.add(Node::row(28).cross(Cross::Start), [cover, words])
}

/// The actions: the details, the read mark and the deletion.
fn menu_block(t: &mut Tree, s: &Styles, info: &BookActionInfo) -> NodeId {
    let details = action_row(
        t,
        s,
        Icon::SquareInfo,
        tr(Msg::LibraryActionDetails),
        action::DETAILS,
        false,
    );

    let (mark_icon, mark_label) = if info.finished {
        (Icon::SquareCheck, tr(Msg::LibraryActionMarkUnread))
    } else {
        (Icon::SquareCheck, tr(Msg::LibraryActionMarkFinished))
    };
    let mark = action_row(t, s, mark_icon, mark_label, action::TOGGLE_FINISHED, false);

    let delete = action_row(
        t,
        s,
        Icon::Trash,
        tr(Msg::LibraryActionDelete),
        action::DELETE_PROMPT,
        true,
    );

    let rows = t.add(
        Node::column(0).cross(Cross::Stretch),
        [details, mark, delete],
    );
    let rows = t.pad(Insets::new(8, 24, 8, 24), rows);
    let card = t.add(Node::column(0).bg(PAPER).border(0, PAPER, 24), [rows]);

    let cancel = pill(t, s, tr(Msg::CommonCancel), action::CANCEL, false);
    let gap = t.gap(1, 24);

    t.add(Node::column(0).cross(Cross::Stretch), [card, gap, cancel])
}

/// One action: an icon in a box and its name.
///
/// Only the name of the action that does something irreversible is bold;
/// the prototype shows the rest in the regular weight.
fn action_row(
    t: &mut Tree,
    s: &Styles,
    icon: Icon,
    label: &str,
    action: ActionId,
    bold: bool,
) -> NodeId {
    let glyph = t.icon(icon, 38, INK);
    let box_ = t.add(
        Node::new(Kind::Align { x: 0.5, y: 0.5 })
            .bg(CARD)
            .border(0, INK, 16),
        [glyph],
    );
    let box_ = t.fixed(Some(64), Some(64), Some(box_));

    let style = if bold { s.item() } else { s.item_regular() };
    let text = t.label(label, style);

    let gap = t.gap(24, 1);
    let row = t.add(Node::row(0).cross(Cross::Center).flex(1), [box_, gap, text]);
    let row = t.pad(Insets::new(14, 0, 14, 0), row);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(100),
        })
        .action(action)
        .feedback(Feedback::Invert),
        [row],
    )
}

/// A wide button: white with a black frame, or black with white words.
///
/// The button fills the width it is given. Two buttons side by side each
/// take half of it, so the caller wraps them in a column with `flex`.
fn pill(t: &mut Tree, s: &Styles, label: &str, action: ActionId, dark: bool) -> NodeId {
    let style = if dark {
        s.item().gray(PAPER)
    } else {
        s.item_regular()
    };
    let text = t.label(label, style);
    let text = t.add(Node::new(Kind::Align { x: 0.5, y: 0.5 }), [text]);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(96),
        })
        .bg(if dark { INK } else { PAPER })
        .border(2, INK, 20)
        .action(action)
        .feedback(Feedback::Invert),
        [text],
    )
}

/// What the book is and how long it has been read.
fn details_block(t: &mut Tree, s: &Styles, info: &BookActionInfo) -> NodeId {
    let format = detail_row(t, s, tr(Msg::LibraryDetailsFormat), &info.format);
    let size = detail_row(t, s, tr(Msg::LibraryDetailsSize), &info.size);
    let path = detail_row(t, s, tr(Msg::LibraryDetailsPath), &info.full_path);
    let time = detail_row(t, s, tr(Msg::LibraryDetailsReadingTime), &info.read_time());
    let start = detail_row(
        t,
        s,
        tr(Msg::LibraryDetailsStartDate),
        info.start_date
            .as_deref()
            .unwrap_or(tr(Msg::LibraryDetailsNotStarted)),
    );

    let sep1 = t.rule(2, RULE);
    let sep2 = t.rule(2, RULE);
    let sep3 = t.rule(2, RULE);
    let sep4 = t.rule(2, RULE);

    let rows = t.add(
        Node::column(0).cross(Cross::Stretch),
        [format, sep1, size, sep2, path, sep3, time, sep4, start],
    );
    let rows = t.pad(Insets::new(20, 24, 20, 24), rows);
    let card = t.add(Node::column(0).bg(PAPER).border(0, PAPER, 24), [rows]);

    let back = pill(t, s, tr(Msg::CommonBack), action::BACK_TO_MENU, false);
    let gap = t.gap(1, 24);

    t.add(Node::column(0).cross(Cross::Stretch), [card, gap, back])
}

/// One line of the details: what it is on the left, the value on the right.
fn detail_row(t: &mut Tree, s: &Styles, label: &str, value: &str) -> NodeId {
    let name = t.label(label, s.ui().gray(INK_2));
    let name = t.fixed(Some(280), None, Some(name));

    let mut vp = Paragraph::plain(value, s.ui().bold(), Align::Left);
    vp.style.max_lines = Some(2);
    let value = t.text(vp);
    let value = t.add(Node::column(0).flex(1), [value]);

    let row = t.add(Node::row(12).cross(Cross::Center), [name, value]);
    t.pad(Insets::new(10, 0, 10, 0), row)
}

/// The question before the local file is deleted.
fn confirm_block(t: &mut Tree, s: &Styles, info: &BookActionInfo) -> NodeId {
    let title_text = tf!(Msg::LibraryDeleteConfirmTitle, title = &info.title);
    let mut tp = Paragraph::plain(&title_text, s.title(), Align::Left);
    tp.style.max_lines = Some(2);
    let title = t.text(tp);

    let mut hp = Paragraph::plain(
        tr(Msg::LibraryDeleteConfirmHint),
        s.ui().gray(INK_2),
        Align::Left,
    );
    hp.style.max_lines = Some(3);
    let hint = t.text(hp);

    let g1 = t.gap(1, 16);
    let g2 = t.gap(1, 24);

    let cancel = pill(t, s, tr(Msg::CommonCancel), action::BACK_TO_MENU, false);
    let delete = pill(
        t,
        s,
        tr(Msg::LibraryActionConfirmDelete),
        action::CONFIRM_DELETE,
        true,
    );
    // Two buttons side by side: each takes half of the width.
    let cancel = t.add(Node::column(0).flex(1), [cancel]);
    let delete = t.add(Node::column(0).flex(1), [delete]);
    let gap = t.gap(24, 1);
    let buttons = t.add(Node::row(0).cross(Cross::Center), [cancel, gap, delete]);

    let block = t.add(
        Node::column(0).cross(Cross::Stretch),
        [title, g1, hint, g2, buttons],
    );
    t.pad(Insets::new(8, 0, 8, 0), block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::FamilyId;

    fn info() -> BookActionInfo {
        BookActionInfo {
            id: "abc".into(),
            title: "Lalka".into(),
            author: "Bolesław Prus".into(),
            format: "EPUB".into(),
            size: "2.4 MB".into(),
            path: "Prus/Lalka.epub".into(),
            full_path: "/mnt/us/documents/Prus/Lalka.epub".into(),
            progress: 0.68,
            finished: false,
            unread: false,
            read_secs: 3600,
            start_date: None,
            cover: None,
        }
    }

    /// The actions a step offers, from the top down.
    fn actions_of(t: &Tree, id: NodeId, out: &mut Vec<ActionId>) {
        let n = t.node(id);
        if let Some(a) = n.action {
            out.push(a);
        }
        for &ch in &n.children {
            actions_of(t, ch, out);
        }
    }

    fn sheet(step: ActionStep) -> (Tree, ActionsSheet) {
        let mut t = Tree::new();
        let s = Styles::new(FamilyId(0));
        let actions = build_book_actions(&mut t, &s, &info(), step);
        (t, actions)
    }

    #[test]
    fn the_menu_offers_details_the_read_mark_and_the_deletion() {
        let (t, actions) = sheet(ActionStep::Menu);
        let mut got = Vec::new();
        actions_of(&t, actions.content, &mut got);
        assert_eq!(
            got,
            vec![
                action::DETAILS,
                action::TOGGLE_FINISHED,
                action::DELETE_PROMPT,
                action::CANCEL,
            ]
        );
    }

    #[test]
    fn the_details_and_the_question_offer_a_way_back() {
        let (t, actions) = sheet(ActionStep::Details);
        let mut got = Vec::new();
        actions_of(&t, actions.content, &mut got);
        assert_eq!(got, vec![action::BACK_TO_MENU]);

        let (t, actions) = sheet(ActionStep::ConfirmDelete);
        let mut got = Vec::new();
        actions_of(&t, actions.content, &mut got);
        assert_eq!(got, vec![action::BACK_TO_MENU, action::CONFIRM_DELETE]);
    }

    #[test]
    fn the_summary_says_what_the_book_is_and_how_far_it_was_read() {
        let mut i = info();
        let read = i.summary();
        assert!(read.starts_with("EPUB · 2.4 MB · "), "{read}");
        assert!(read.contains("68%"), "{read}");
        // A finished book and an unread one say so instead of a share.
        i.finished = true;
        let finished = i.summary();
        assert!(finished.starts_with("EPUB · 2.4 MB · "), "{finished}");
        assert!(!finished.contains("68%"), "{finished}");
        i.finished = false;
        i.unread = true;
        i.progress = 0.0;
        let unread = i.summary();
        assert!(unread.starts_with("EPUB · 2.4 MB · "), "{unread}");
        assert!(!unread.contains("68%"), "{unread}");
    }
}
