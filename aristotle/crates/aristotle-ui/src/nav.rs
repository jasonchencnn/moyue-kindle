// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bottom navigation bar: Home, Library, Mine.
//!
//! One bar for the three sections of the app, so the reader always knows
//! where she is. It stands at the foot of the screen on a light gray
//! panel, three equal cells, the current one inverted (white on black).
//!
//! The bar is shared chrome, so its action ids live in a range of their
//! own ([`action::TAB`]), clear of the ids the screens use for their own
//! rows. A screen that shows the bar hands any action it does not know to
//! [`tab_of`].

use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, RULE};
use crate::tree::{ActionId, Cross, Kind, Node, NodeId, Tree};
use aristotle_i18n::{tr, Msg};

/// The sections of the app, in the order the bar shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Home,
    Library,
    Mine,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Home, Tab::Library, Tab::Mine];

    /// The word under the icon.
    pub fn label(self) -> &'static str {
        tr(match self {
            Tab::Home => Msg::NavHome,
            Tab::Library => Msg::NavLibrary,
            Tab::Mine => Msg::NavMine,
        })
    }

    /// The icon above the word.
    pub fn icon(self) -> Icon {
        match self {
            Tab::Home => Icon::Home,
            Tab::Library => Icon::Library,
            Tab::Mine => Icon::User,
        }
    }
}

pub mod action {
    use crate::tree::ActionId;
    /// `TAB + i` selects `Tab::ALL[i]`.
    ///
    /// The bar is shared chrome, so its ids start well above the ones the
    /// screens use for their own rows: no screen id can reach them.
    pub const TAB: ActionId = 100_000;
}

/// The height of the bar, in reference pixels: 24 px above, the 44 px
/// icon, a 10 px gap, the 52 px word and 22 px below.
pub const HEIGHT: i32 = 152;
/// The icon above the word.
const ICON: i32 = 44;
/// The gap between the icon and the word.
const GAP: i32 = 10;
/// The height of the capsule the word stands on.
const CAPSULE_H: i32 = 52;
/// The padding inside the capsule, to the left and to the right.
const CAPSULE_PAD: i32 = 22;
/// The radius of the capsule.
const CAPSULE_R: i32 = 16;

/// The tab an action id selects, if it is one of the bar's.
pub fn tab_of(id: ActionId) -> Option<Tab> {
    let i = id.checked_sub(action::TAB)?;
    Tab::ALL.get(i as usize).copied()
}

/// Handles to the nodes of the bar.
#[derive(Clone, Debug)]
pub struct NavBar {
    /// The bar itself, to be put at the foot of the screen.
    pub root: NodeId,
    /// The cell of each tab, in `Tab::ALL` order.
    pub cells: Vec<NodeId>,
}

/// Builds the bar with `current` inverted.
///
/// The bar stands on a gray panel that reaches the edges of the screen;
/// the cells sit inside the page's margin.
pub fn build(t: &mut Tree, s: &Styles, current: Tab) -> NavBar {
    let mut cells = Vec::new();
    let mut row = Vec::new();
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        let selected = tab == current;
        let icon = t.icon(tab.icon(), ICON, INK);
        let style = if selected {
            s.font_xs().bold()
        } else {
            s.font_xs().gray(INK_MID)
        };
        let label = t.label(tab.label(), style);
        let label = t.pad(Insets::new(0, CAPSULE_PAD, 0, CAPSULE_PAD), label);
        // The current tab's word stands on an inverted capsule, white on
        // black; the others are grey on the panel.
        let capsule = t.add(
            Node::new(Kind::MinSize(Size::new(0, CAPSULE_H)))
                .border(0, RULE, CAPSULE_R)
                .inverted(selected),
            [label],
        );
        let cell = t.add(
            Node::column(GAP).cross(Cross::Center).flex(1),
            [icon, capsule],
        );
        // The cells share the width of the bar, one third each.
        let cell = t.button(Node::plain().flex(1), cell, action::TAB + i as ActionId);
        cells.push(cell);
        row.push(cell);
    }
    // The row takes the rest of the bar, so the cells sit in its middle.
    let row = t.add(Node::row(0).cross(Cross::Center).flex(1), row);
    let row = t.pad(Insets::new(0, home_margin(), 0, home_margin()), row);
    // The panel reaches the edges of the screen; the cells keep the
    // page's margin inside it.
    let bar = t.add(Node::column(0).bg(CARD), [row]);
    let bar = t.pad(Insets::new(24, 0, 22, 0), bar);
    let bar = t.fixed(None, Some(HEIGHT), Some(bar));
    NavBar { root: bar, cells }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{FamilyId, Fonts, TextEngine};

    fn tree() -> (Tree, TextEngine) {
        (Tree::new(), TextEngine::new(Fonts::embedded()))
    }

    #[test]
    fn the_three_tabs_are_the_ones_the_bar_shows() {
        assert_eq!(Tab::ALL.len(), 3);
        assert_eq!(tab_of(action::TAB), Some(Tab::Home));
        assert_eq!(tab_of(action::TAB + 1), Some(Tab::Library));
        assert_eq!(tab_of(action::TAB + 2), Some(Tab::Mine));
        assert_eq!(tab_of(action::TAB + 3), None);
        assert_eq!(tab_of(0), None);
        assert_eq!(tab_of(1), None);
    }

    #[test]
    fn the_bar_has_a_cell_for_every_tab() {
        let (mut t, _text) = tree();
        let s = Styles::new(FamilyId(0));
        let bar = build(&mut t, &s, Tab::Home);
        assert_eq!(bar.cells.len(), Tab::ALL.len());
        for (i, cell) in bar.cells.iter().enumerate() {
            assert_eq!(t.node(*cell).action, Some(action::TAB + i as ActionId));
        }
    }

    #[test]
    fn the_current_tab_is_inverted_and_the_others_are_not() {
        let (mut t, _text) = tree();
        let s = Styles::new(FamilyId(0));
        let bar = build(&mut t, &s, Tab::Mine);
        let inverted: Vec<bool> = bar
            .cells
            .iter()
            .map(|c| {
                let column = t.node(*c).children[0];
                t.node(column).children.iter().any(|n| t.node(*n).inverted)
            })
            .collect();
        assert_eq!(inverted, vec![false, false, true]);
    }
}
