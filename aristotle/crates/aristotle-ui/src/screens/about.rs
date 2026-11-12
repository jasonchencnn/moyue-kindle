// SPDX-License-Identifier: AGPL-3.0-or-later
//! What this program is, and what the device it runs on is.
//!
//! The facts come from the device: the version of the program, the
//! reader, the firmware and the memory. They are read when the screen is
//! opened, not before, so the home screen does not wait for them.

use super::icon_button;
use super::library::{status_line, StatusLine};
use crate::geom::{mm, Insets};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{margin, IconSize, Space, Styles, INK_2};
use crate::tree::{Cross, Node, NodeId, Tree};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
}

/// What the about screen shows.
pub struct About<'a> {
    pub status: StatusLine<'a>,
    /// The facts: the software, the reader, the firmware, the memory.
    pub facts: &'a [(&'static str, String)],
}

/// Handles to the nodes of the about screen.
pub struct AboutUi {
    /// The title of the screen.
    pub title: NodeId,
    /// The loading area in the middle of the status line, hidden.
    pub loading: LoadingNodes,
}

impl AboutUi {
    /// Builds the about screen.
    pub fn build(_text: &mut TextEngine, s: &Styles, about: &About) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &about.status);

        let back = icon_button(&mut t, Icon::ArrowLeft, IconSize::STANDARD, action::BACK);
        let title = t.label(tr(Msg::AboutTitle), s.display());
        let g = t.gap(Space::XS, 1);
        let header = t.add(Node::row(0), [back, g, title]);
        // The back arrow's 9 mm target overhangs the margin.
        let header = t.pad(Insets::new(0, 0, 0, -26), header);

        let mut lines = Vec::new();
        for (label, value) in about.facts {
            let l = t.label(label, s.small());
            let l = t.fixed(Some(mm(25.0)), None, Some(l));
            let mut p = Paragraph::plain(value.as_str(), s.ui(), Align::Left);
            p.style.max_lines = Some(2);
            let val = t.text(p);
            let val = t.add(Node::column(0).flex(1), [val]);
            lines.push(t.add(Node::row(Space::S).cross(Cross::Start), [l, val]));
        }
        let lines = t.add(Node::column(Space::S), lines);
        let info = t.icon(Icon::Info, IconSize::STANDARD, INK_2);
        let block = t.add(Node::row(Space::M).cross(Cross::Start), [info, lines]);
        let block = t.pad(Insets::vh(Space::L, 0), block);

        let col = t.add(Node::column(0), [bar.root, header, block]);
        let col = t.pad(Insets::new(0, margin(), mm(1.5), margin()), col);
        let col = t.add(Node::column(0).flex(1), [col]);
        t.set_root(col);
        (
            t,
            AboutUi {
                title,
                loading: bar.loading,
            },
        )
    }
}
