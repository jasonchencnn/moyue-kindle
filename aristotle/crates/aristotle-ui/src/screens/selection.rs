// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sheet of actions under a selection: what the reader can do with the
//! words they picked.
//!
//! The sheet is a title and the buttons. The buttons are packed into as
//! many rows as they need, so the narrow panels keep every button.

use crate::geom::MIN_TARGET;
use crate::text::{Align, Paragraph};
use crate::theme::Styles;
use crate::tree::{Node, Tree};

use super::sheet::{self, Button, Place, Sheet};

/// The sheet of actions under a selection.
///
/// `available` is the width the buttons have (the panel, less the
/// margins).
pub fn build(
    t: &mut Tree,
    st: &Styles,
    title: &str,
    buttons: &[Button],
    available: i32,
    place: Place,
) -> Sheet {
    let mut para = Paragraph::plain(title.to_string(), st.title(), Align::Left);
    para.style.max_lines = Some(1);
    let title = t.text(para);
    let title = t.add(Node::column(0).flex(1), [title]);
    // The title keeps a line of its own: a flexible child of a column
    // without a fixed height would grow the sheet over the whole page.
    let head = t.add(Node::row(10), [title]);
    let head = t.fixed(None, Some(MIN_TARGET), Some(head));
    let buttons = sheet::buttons(t, st, buttons, available);
    let content = sheet::content(t, vec![head, buttons]);
    sheet::layer(t, content, place, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Density, Rect};
    use crate::text::{FamilyId, Fonts, TextEngine};

    /// The sheet of actions is a few lines tall, not a white board over
    /// the whole page.
    #[test]
    fn the_sheet_takes_the_room_its_buttons_need() {
        let mut tree = Tree::new();
        let mut text = TextEngine::new(Fonts::embedded());
        let st = Styles::new(FamilyId(0)).with_density(Density::new(167).unwrap());
        let available = sheet::available_width(&st, 600);
        let items = vec![Button {
            label: "Highlight".into(),
            action: 1,
            width: 120,
        }];
        let s = build(
            &mut tree,
            &st,
            "Selection",
            &items,
            available,
            Place::Bottom,
        );
        tree.set_root(s.root);
        tree.layout(Rect::new(0, 0, 600, 800), &mut text);
        let rect = tree.rect(s.sheet);
        assert!(rect.h > 0, "the sheet has no height: {rect:?}");
        // A few lines, not a white board over the whole page.
        assert!(rect.h < 800 / 2, "the sheet fills the screen: {rect:?}");
    }
}
