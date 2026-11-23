// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sheet the reader puts over the page, and the buttons at its foot.
//!
//! A sheet is a rule, the content, and the room around them: it sits at
//! the top or the bottom of the screen, over the page, and the page is
//! still there around it.
//!
//! The buttons at the foot are packed into rows: the narrow panels do not
//! have room for six buttons on one line, and a button that does not fit
//! is not drawn at all. The width of every button is measured by the
//! caller, which owns the text engine (the tree does not).

use crate::geom::Insets;
use crate::text::TextEngine;
use crate::theme::{margin, Styles, INK};
use crate::tree::{ActionId, Cross, Node, NodeId, Tree};

/// The room a button leaves around its label, in reference pixels.
pub const PADDING: i32 = 22;

/// The gap between two buttons, in reference pixels.
pub const GAP: i32 = 18;

/// Where a sheet sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Top,
    Bottom,
}

/// A button of a sheet's foot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    /// What the button says.
    pub label: String,
    /// What the button does.
    pub action: ActionId,
    /// How wide the button is.
    pub width: i32,
}

impl Button {
    /// The button for `label`, as wide as the text engine says.
    pub fn new(text: &mut TextEngine, st: &Styles, label: &str, action: ActionId) -> Button {
        Button {
            label: label.to_string(),
            action,
            width: measure(text, st, label),
        }
    }
}

/// The width of a button with this label: the text, and the room the
/// button leaves around it.
pub fn measure(text: &mut TextEngine, st: &Styles, label: &str) -> i32 {
    let w = text.measure(label, &st.ui_bold()).ceil() as i32;
    w + 2 * st.density.px(PADDING)
}

/// The width the buttons of a sheet have: the panel, less the margins.
pub fn available_width(st: &Styles, panel: i32) -> i32 {
    panel - 2 * st.density.px(margin())
}

/// The buttons of a sheet's foot, in as many rows as they need.
///
/// The rows are stacked in a column: a row of buttons that does not fit
/// the width breaks, and the rows below it follow.
pub fn buttons(t: &mut Tree, st: &Styles, items: &[Button], available: i32) -> NodeId {
    let rows = rows(t, st, items, available);
    t.add(Node::column(6), rows)
}

/// The foot of a sheet: the buttons, and the pager on a row of its own.
///
/// The pager keeps a line of its own: the buttons may need the whole
/// width, and a pager squeezed between them would be pressed by mistake.
pub fn foot(
    t: &mut Tree,
    st: &Styles,
    items: &[Button],
    available: i32,
    pager: Vec<NodeId>,
) -> NodeId {
    let buttons = buttons(t, st, items, available);
    let mut line = vec![t.spacer(1)];
    line.extend(pager);
    let line = t.add(Node::row(4).cross(Cross::Center), line);
    t.add(Node::column(8), [buttons, line])
}

/// The buttons in rows that fit `available`: one node per row.
///
/// The buttons keep their order. A row breaks when the next button would
/// not fit; a button wider than the whole row keeps a row of its own.
pub fn rows(t: &mut Tree, st: &Styles, buttons: &[Button], available: i32) -> Vec<NodeId> {
    let groups = pack(buttons, available, st.density.px(GAP));
    groups
        .into_iter()
        .map(|group| row(t, st, buttons, &group))
        .collect()
}

/// The rows the buttons fall into: the buttons of each row, in order.
fn pack(buttons: &[Button], available: i32, gap: i32) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut row: Vec<usize> = Vec::new();
    let mut used = 0;
    for (i, b) in buttons.iter().enumerate() {
        let need = if row.is_empty() {
            b.width
        } else {
            gap + b.width
        };
        if !row.is_empty() && used + need > available {
            out.push(std::mem::take(&mut row));
            used = 0;
        }
        let need = if row.is_empty() {
            b.width
        } else {
            gap + b.width
        };
        used += need;
        row.push(i);
    }
    if !row.is_empty() {
        out.push(row);
    }
    out
}

/// One row of buttons.
fn row(t: &mut Tree, st: &Styles, buttons: &[Button], group: &[usize]) -> NodeId {
    let mut cells = Vec::new();
    for &i in group {
        let label = t.label(&buttons[i].label, st.ui_bold());
        let label = t.pad(Insets::vh(0, PADDING), label);
        let b = t.button(Node::plain().border(2, INK, 14), label, buttons[i].action);
        cells.push(b);
    }
    t.add(Node::row(GAP), cells)
}

/// The nodes of a sheet.
#[derive(Clone, Copy)]
pub struct Sheet {
    /// The whole layer: the sheet and the room around it.
    pub root: NodeId,
    /// The sheet itself.
    pub sheet: NodeId,
}

/// The layer with the sheet at the top or the bottom of the screen.
///
/// `height` is the height of the sheet in device pixels; `None` lets it
/// take the room its content needs.
pub fn layer(t: &mut Tree, content: NodeId, place: Place, height: Option<i32>) -> Sheet {
    let rule = t.rule(3, INK);
    let children = match place {
        Place::Bottom => [rule, content],
        Place::Top => [content, rule],
    };
    let sheet = t.add(Node::column(0).bg(255), children);
    let sheet = match height {
        Some(h) => t.fixed_pixels(None, Some(h), Some(sheet)),
        None => sheet,
    };
    let fill = t.spacer(1);
    let layer = match place {
        Place::Bottom => [fill, sheet],
        Place::Top => [sheet, fill],
    };
    let root = t.add(Node::column(0), layer);
    Sheet { root, sheet }
}

/// The content of a sheet: the rows, and the margins around them.
pub fn content(t: &mut Tree, rows: Vec<NodeId>) -> NodeId {
    let content = t.add(Node::column(14).cross(Cross::Stretch), rows);
    t.pad(Insets::new(18, margin(), 18, margin()), content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(width: i32) -> Button {
        Button {
            label: "x".into(),
            action: 1,
            width,
        }
    }

    #[test]
    fn the_buttons_keep_their_order() {
        let buttons: Vec<Button> = (0..4).map(|_| button(100)).collect();
        assert_eq!(pack(&buttons, 1000, 10), vec![vec![0, 1, 2, 3]]);
    }

    #[test]
    fn a_row_breaks_when_the_next_button_does_not_fit() {
        let buttons: Vec<Button> = (0..4).map(|_| button(100)).collect();
        // 100 + 10 + 100 = 210 fits, 310 does not.
        assert_eq!(pack(&buttons, 300, 10), vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn a_button_wider_than_the_row_keeps_a_row_of_its_own() {
        let buttons = vec![button(100), button(400), button(100)];
        assert_eq!(pack(&buttons, 300, 10), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn no_buttons_make_no_rows() {
        assert!(pack(&[], 300, 10).is_empty());
    }
}
