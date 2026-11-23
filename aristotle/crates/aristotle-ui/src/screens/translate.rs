// SPDX-License-Identifier: AGPL-3.0-or-later
//! The translation sheet: the text that was sent, the translation, and the
//! service it came from.
//!
//! The sheet is built here, not in the reader app, so the host tests can
//! lay it out on every panel and in every language and see that nothing
//! overlaps or leaves the screen (see `tests/ui_geometry.rs`).

use crate::geom::MIN_TARGET;
use crate::icons::Icon;
use crate::screens::icon_button;
use crate::text::{Align, Paragraph};
use crate::theme::Styles;
use crate::tree::{pages, Node, NodeId, Tree};

use super::sheet::{self, Button, Place};

/// The actions of the translation sheet.
pub mod action {
    use crate::tree::ActionId;
    /// The page before.
    pub const PREV: ActionId = 23_001;
    /// The page after.
    pub const NEXT: ActionId = 23_002;
}

/// What the translation sheet shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslateView<'a> {
    /// The title over the text.
    pub title: &'a str,
    /// The label over the text that was sent.
    pub original_label: &'a str,
    /// The text that was sent.
    pub original: &'a str,
    /// The translation, when it came.
    pub translation: Option<&'a str>,
    /// The service the translation came from.
    pub origin: Option<&'a str>,
    /// What to show while the service works, or why it did not answer.
    pub status: Option<&'a str>,
    /// The buttons at the foot.
    pub buttons: Vec<Button>,
}

/// The nodes of the translation sheet.
#[derive(Clone, Copy)]
pub struct TranslateUi {
    /// The whole layer: the sheet and the room around it.
    pub root: NodeId,
    /// The sheet itself.
    pub sheet: NodeId,
    /// The paged body: the original and the translation.
    pub pages: NodeId,
    /// The page label in the pager.
    pub pager: NodeId,
    /// The pager's arrows.
    pub arrows: (NodeId, NodeId),
}

/// Builds the translation sheet.
///
/// `available` is the width the content has (the panel, less the margins),
/// and `height` the height of the sheet in device pixels.
pub fn build(
    t: &mut Tree,
    st: &Styles,
    view: &TranslateView<'_>,
    available: i32,
    place: Place,
    height: i32,
) -> TranslateUi {
    // The title, and the service the translation came from beside it.
    let mut para = Paragraph::plain(view.title.to_string(), st.title(), Align::Left);
    para.style.max_lines = Some(1);
    let title = t.text(para);
    let title = t.add(Node::column(0).flex(1), [title]);
    let mut head = vec![title];
    if let Some(origin) = view.origin {
        head.push(t.label(origin, st.small()));
    }
    let head = t.add(Node::row(10), head);
    let head = t.fixed(None, Some(MIN_TARGET), Some(head));

    // The body: what was sent, what came back, and how it went.
    let mut paras = Vec::new();
    let original = format!("{} · {}", view.original_label, view.original);
    let mut para = Paragraph::plain(original, st.small(), Align::Left);
    para.style.max_lines = Some(3);
    paras.push(t.text(para));
    if let Some(text) = view.translation {
        let para = Paragraph::plain(text.to_string(), st.item_regular(), Align::Left);
        paras.push(t.text(para));
    }
    if let Some(status) = view.status {
        paras.push(t.label(status, st.small()));
    }
    let body = t.add(pages(8), paras);
    let pages_node = body;
    let body = t.add(Node::column(0).flex(1), [body]);

    // The foot: the buttons, and the pager of the body on a row of its
    // own.
    let prev = icon_button(t, Icon::ChevronLeft, 40, action::PREV);
    let pager = t.label("", st.small());
    let next = icon_button(t, Icon::ChevronRight, 40, action::NEXT);
    let foot = sheet::foot(t, st, &view.buttons, available, vec![prev, pager, next]);

    let content = sheet::content(t, vec![head, body, foot]);
    let content = t.add(Node::column(0).flex(1), [content]);
    let layer = sheet::layer(t, content, place, Some(height));
    TranslateUi {
        root: layer.root,
        sheet: layer.sheet,
        pages: pages_node,
        pager,
        arrows: (prev, next),
    }
}
