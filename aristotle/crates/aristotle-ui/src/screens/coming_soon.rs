// SPDX-License-Identifier: AGPL-3.0-or-later
//! The page of a feature that is not built yet.
//!
//! The prototype links to the reading calendar, the notes and the
//! quotations. None of them is in the app, so the page that opens for them
//! says what is coming and offers the way back. It looks like the rest of
//! the "mine" screen: the same header, the same cards.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::TextEngine;
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID};
use crate::tree::{Cross, Feedback, Kind, Node, NodeId, Tree};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// The way back to the screen the page was opened from.
    pub const BACK: ActionId = 1;
}

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 112;
/// The size of the back button, in reference pixels.
const BACK_H: i32 = 72;

/// What the page shows.
pub struct ComingSoon<'a> {
    pub status: StatusLine<'a>,
    /// The name of the feature, as the entry showed it.
    pub title: &'a str,
}

/// Handles to the nodes of the page.
pub struct ComingSoonUi {
    /// The title of the page.
    pub title: NodeId,
    /// The loading area in the middle of the status line, hidden.
    pub loading: LoadingNodes,
}

impl ComingSoonUi {
    /// Builds the page.
    pub fn build(_text: &mut TextEngine, s: &Styles, soon: &ComingSoon) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &soon.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

        let glyph = t.icon(Icon::ChevronLeft, 44, INK);
        let back = t.add(
            Node::new(Kind::MinSize(Size::new(BACK_H, BACK_H)))
                .action(action::BACK)
                .feedback(Feedback::Invert),
            [glyph],
        );
        let title = t.label(
            soon.title,
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
        let header = t.fixed(None, Some(HEADER_H), Some(header));

        let text = t.label(
            tr(Msg::ComingSoonBody),
            s.font_base().gray(INK_MID).line_height(1.4),
        );
        let card = t.add(
            Node::new(Kind::Padding(Insets::new(32, 32, 32, 32)))
                .bg(CARD)
                .border(0, CARD, 32),
            [text],
        );

        let gap = t.gap(0, 24);
        let col = t.add(Node::column(0), [header, gap, card]);
        let col = t.pad(Insets::new(24, home_margin(), 32, home_margin()), col);
        let col = t.add(Node::column(0).flex(1), [col]);
        let root = t.add(Node::column(0), [status, col]);
        t.set_root(root);
        (
            t,
            ComingSoonUi {
                title,
                loading: bar.loading,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Fonts, TextEngine};
    use crate::theme::Styles;

    #[test]
    fn the_page_has_a_way_back() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let soon = ComingSoon {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            title: "Reading calendar",
        };
        let (tree, _view) = ComingSoonUi::build(&mut text, &s, &soon);
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::BACK));
    }

    /// Whether the subtree holds a node with this action.
    fn has_action(tree: &Tree, id: NodeId, action: crate::tree::ActionId) -> bool {
        if tree.node(id).action == Some(action) {
            return true;
        }
        tree.node(id)
            .children
            .iter()
            .any(|c| has_action(tree, *c, action))
    }
}
