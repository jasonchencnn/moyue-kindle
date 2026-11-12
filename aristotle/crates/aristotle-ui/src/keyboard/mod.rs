// SPDX-License-Identifier: AGPL-3.0-or-later
//! Screen keyboard and Chinese Pinyin IME component.
//!
//! Provides a full QWERTY keyboard, pinyin candidate selection, input box,
//! and numeric/symbol pages following the pure ink design guidelines.

pub mod opencc_data;
pub mod pinyin;
pub mod pinyin_data;
pub mod state;

pub use opencc_data::{to_simplified_char, to_simplified_str};
pub use state::{InputMode, KeyboardPage, KeyboardState, Shift};

use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::text::{
    layout_paragraph, Align, FamilyId, NoHyphenation, Paragraph, TextEngine, TextStyle,
};
use crate::theme::{Styles, INK, INK_2, INK_3};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;

    pub const CAND_PREV: ActionId = 500;
    pub const CAND_NEXT: ActionId = 501;
    pub const CAND_BASE: ActionId = 510; // 510..517 for candidates 0..7
    pub const MODE_ZH: ActionId = 520;
    pub const MODE_EN: ActionId = 521;
    pub const TOGGLE_PAGE: ActionId = 522;
    pub const SPACE: ActionId = 523;
    pub const DELETE: ActionId = 524;
    pub const CONFIRM: ActionId = 525;
    pub const SHIFT: ActionId = 526;
    pub const KEY_BASE: ActionId = 600; // 600 + char codepoint
}

/// Single-line input box widget displaying committed text, underlined uncommitted pinyin, and cursor.
#[derive(Clone, Debug)]
pub struct InputBoxWidget {
    pub committed: String,
    pub pinyin: String,
    pub placeholder: String,
    pub secret: bool,
}

impl Widget for InputBoxWidget {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        Size::new(c.max.w, 88)
    }

    fn paint(&self, rect: Rect, cx: &mut PaintCx) {
        let p = &mut cx.painter;
        let radius = 16.0;

        // White background with 2px black rounded border.
        p.fill_round_rect(rect, radius, 255);
        p.stroke_round_rect(rect, radius, 2.0, 0);

        let style = TextStyle::new(FamilyId(0), 36.0).gray(INK);
        let placeholder_style = TextStyle::new(FamilyId(0), 36.0).gray(INK_3);

        let pad_x = 24.0;
        let mut curr_x = rect.x as f32 + pad_x;
        let baseline_y = rect.y as f32 + (rect.h as f32 - 44.0) / 2.0;

        if self.committed.is_empty() && self.pinyin.is_empty() {
            if !self.placeholder.is_empty() {
                let para = Paragraph::plain(&self.placeholder, placeholder_style, Align::Left);
                let layout =
                    layout_paragraph(cx.text, &para, rect.w as f32 - pad_x * 2.0, &NoHyphenation);
                layout.draw(cx.text, p, curr_x, baseline_y);
            }
            // Cursor
            p.fill_rect(Rect::new(curr_x as i32, baseline_y as i32, 3, 44), 0);
            return;
        }

        // Draw committed text (masked with dots if secret/password)
        if !self.committed.is_empty() {
            let display_text = if self.secret {
                "•".repeat(self.committed.chars().count())
            } else {
                self.committed.clone()
            };
            let para = Paragraph::plain(&display_text, style.clone(), Align::Left);
            let layout = layout_paragraph(cx.text, &para, rect.w as f32, &NoHyphenation);
            layout.draw(cx.text, p, curr_x, baseline_y);
            curr_x += layout.width;
        }

        // Draw uncommitted pinyin with underline
        if !self.pinyin.is_empty() {
            curr_x += 4.0;
            let para = Paragraph::plain(&self.pinyin, style, Align::Left);
            let layout = layout_paragraph(cx.text, &para, rect.w as f32, &NoHyphenation);
            layout.draw(cx.text, p, curr_x, baseline_y);

            // 2px underline with 6px offset from text baseline
            let underline_y = baseline_y + 40.0;
            p.hline(curr_x as i32, underline_y as i32, layout.width as i32, 2, 0);
            curr_x += layout.width;
        }

        // Cursor bar (3px wide, 44px high)
        curr_x += 4.0;
        p.fill_rect(Rect::new(curr_x as i32, baseline_y as i32, 3, 44), 0);
    }
}

/// Builds an input box node displaying committed text, pinyin, and cursor.
pub fn build_input_box(
    t: &mut Tree,
    s: &Styles,
    committed: &str,
    pinyin: &str,
    placeholder: &str,
) -> NodeId {
    build_input_box_secret(t, s, committed, pinyin, placeholder, false)
}

/// Builds an input box node displaying committed text (optionally masked as secret), pinyin, and cursor.
pub fn build_input_box_secret(
    t: &mut Tree,
    _s: &Styles,
    committed: &str,
    pinyin: &str,
    placeholder: &str,
    secret: bool,
) -> NodeId {
    t.add(
        Node::new(Kind::Custom(Box::new(InputBoxWidget {
            committed: committed.to_string(),
            pinyin: pinyin.to_string(),
            placeholder: placeholder.to_string(),
            secret,
        }))),
        [],
    )
}

/// Builds the candidate bar displaying up to 7 candidates and pagination arrows.
pub fn build_candidate_bar(t: &mut Tree, s: &Styles, state: &KeyboardState) -> NodeId {
    if state.all_candidates.is_empty() {
        let spacer = t.spacer(1);
        return t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(s.density.px(88)),
            })
            .in_pixels(),
            [spacer],
        );
    }

    let prev_active = state.has_prev_cand_page();
    let next_active = state.has_next_cand_page();

    // Prev button
    let prev_icon = t.icon(Icon::ChevronLeft, 28, if prev_active { INK } else { INK_3 });
    let mut prev_node = Node::new(Kind::Fixed {
        w: Some(s.density.px(64)),
        h: Some(s.density.px(64)),
    })
    .in_pixels();
    if prev_active {
        prev_node = prev_node
            .action(action::CAND_PREV)
            .feedback(Feedback::Invert);
    }
    let prev_align = t.align(0.5, 0.5, prev_icon);
    let prev_btn = t.add(prev_node, [prev_align]);

    // Candidate buttons
    let mut cand_nodes = Vec::new();
    let current_cands = state.current_candidates();

    for (i, &ch) in current_cands.iter().enumerate() {
        let label = t.label(
            &ch.to_string(),
            TextStyle::new(FamilyId(0), 40.0)
                .at_density(s.density)
                .gray(INK),
        );
        let pad = t.pad(Insets::vh(0, 14), label);
        let btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(s.density.px(64)),
            })
            .in_pixels()
            .action(action::CAND_BASE + (i as ActionId))
            .feedback(Feedback::Invert),
            [pad],
        );
        cand_nodes.push(btn);
    }

    if next_active {
        let more_label = t.label(
            "…",
            TextStyle::new(FamilyId(0), 36.0)
                .at_density(s.density)
                .gray(INK_2),
        );
        cand_nodes.push(t.pad(Insets::vh(0, 8), more_label));
    }

    let cands_row = t.add(
        Node::row(s.density.px(6)).cross(Cross::Center).flex(1),
        cand_nodes,
    );

    // Next button
    let next_icon = t.icon(
        Icon::ChevronRight,
        28,
        if next_active { INK } else { INK_3 },
    );
    let mut next_node = Node::new(Kind::Fixed {
        w: Some(s.density.px(64)),
        h: Some(s.density.px(64)),
    })
    .in_pixels();
    if next_active {
        next_node = next_node
            .action(action::CAND_NEXT)
            .feedback(Feedback::Invert);
    }
    let next_align = t.align(0.5, 0.5, next_icon);
    let next_btn = t.add(next_node, [next_align]);

    let bar = t.add(
        Node::row(s.density.px(8)).cross(Cross::Center),
        [prev_btn, cands_row, next_btn],
    );

    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(88)),
        })
        .in_pixels(),
        [bar],
    )
}

/// Builds a single keyboard key button with rounded corners, fine black border, and A2 touch inversion.
fn build_key(
    t: &mut Tree,
    s: &Styles,
    label_str: &str,
    ch: char,
    w: i32,
    h: i32,
    font_size: f32,
) -> NodeId {
    let label = t.label(
        label_str,
        TextStyle::new(FamilyId(0), font_size)
            .at_density(s.density)
            .gray(INK),
    );
    let align = t.align(0.5, 0.5, label);

    t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(w)),
            h: Some(s.density.px(h)),
        })
        .in_pixels()
        .bg(255)
        .border(1, 0, 16)
        .feedback(Feedback::Invert)
        .action(action::KEY_BASE + (ch as ActionId)),
        [align],
    )
}

/// Builds the shift key: "Aa" when off, inverted "Aa" for one letter, inverted "AA" for caps lock.
fn build_shift_key(t: &mut Tree, s: &Styles, shift: Shift) -> NodeId {
    let on = shift != Shift::Off;
    let label = t.label(
        if shift == Shift::Lock { "AA" } else { "Aa" },
        TextStyle::new(FamilyId(0), 34.0)
            .at_density(s.density)
            .bold()
            .gray(if on { 255 } else { INK }),
    );
    let align = t.align(0.5, 0.5, label);
    t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(94)),
            h: Some(s.density.px(136)),
        })
        .in_pixels()
        .bg(if on { 0 } else { 255 })
        .border(1, 0, 16)
        .feedback(Feedback::Invert)
        .action(action::SHIFT),
        [align],
    )
}

/// Builds the letter page (QWERTY layout + bottom controls).
fn build_letter_page(
    t: &mut Tree,
    s: &Styles,
    state: &KeyboardState,
    action_label: &str,
) -> NodeId {
    let row1 = ['q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'];
    let row2 = ['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'];
    let row3 = ['z', 'x', 'c', 'v', 'b', 'n', 'm'];

    let english = state.mode == InputMode::English;
    let upper = english && state.shift != Shift::Off;
    let key_label = |c: char| {
        if upper {
            c.to_ascii_uppercase().to_string()
        } else {
            c.to_string()
        }
    };
    let r1_nodes: Vec<NodeId> = row1
        .iter()
        .map(|&c| build_key(t, s, &key_label(c), c, 94, 136, 40.0))
        .collect();
    let r1 = t.add(Node::row(s.density.px(8)).cross(Cross::Center), r1_nodes);

    let r2_nodes: Vec<NodeId> = row2
        .iter()
        .map(|&c| build_key(t, s, &key_label(c), c, 94, 136, 40.0))
        .collect();
    let r2 = t.add(Node::row(s.density.px(8)).cross(Cross::Center), r2_nodes);

    let mut r3_nodes: Vec<NodeId> = Vec::new();
    if english {
        r3_nodes.push(build_shift_key(t, s, state.shift));
    }
    r3_nodes.extend(
        row3.iter()
            .map(|&c| build_key(t, s, &key_label(c), c, 94, 136, 40.0)),
    );
    let r3 = t.add(Node::row(s.density.px(8)).cross(Cross::Center), r3_nodes);

    let rows_col = t.add(
        Node::column(s.density.px(14)).cross(Cross::Center),
        [r1, r2, r3],
    );

    // Bottom row:
    // [中|英] [123] [空格] [Backspace] [Action]
    let zh_active = state.mode == InputMode::Chinese;

    // Segmented button: ZH
    let zh_text = t.label(
        tr(Msg::ImeChinese),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(if zh_active { 255 } else { INK }),
    );
    let zh_align = t.align(0.5, 0.5, zh_text);
    let zh_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(76)),
            h: Some(s.density.px(96)),
        })
        .in_pixels()
        .bg(if zh_active { 0 } else { 255 })
        .border(0, 0, 14)
        .action(action::MODE_ZH)
        .feedback(Feedback::Invert),
        [zh_align],
    );

    // Segmented button: EN
    let en_text = t.label(
        tr(Msg::ImeEnglish),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(if !zh_active { 255 } else { INK }),
    );
    let en_align = t.align(0.5, 0.5, en_text);
    let en_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(76)),
            h: Some(s.density.px(96)),
        })
        .in_pixels()
        .bg(if !zh_active { 0 } else { 255 })
        .border(0, 0, 14)
        .action(action::MODE_EN)
        .feedback(Feedback::Invert),
        [en_align],
    );

    let mode_capsule = t.add(
        Node::row(s.density.px(6)).cross(Cross::Center),
        [zh_btn, en_btn],
    );
    let mode_pad = t.pad(Insets::all(6), mode_capsule);
    let mode_seg = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .bg(255)
        .border(1, 0, 20),
        [mode_pad],
    );

    // 123 button
    let num_label = t.label(
        "123",
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK),
    );
    let num_align = t.align(0.5, 0.5, num_label);
    let num_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(140)),
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .bg(255)
        .border(1, 0, 20)
        .action(action::TOGGLE_PAGE)
        .feedback(Feedback::Invert),
        [num_align],
    );

    // Space button
    let space_label = t.label(
        tr(Msg::ImeSpace),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK),
    );
    let space_align = t.align(0.5, 0.5, space_label);
    let space_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .flex(1)
        .bg(255)
        .border(1, 0, 20)
        .action(action::SPACE)
        .feedback(Feedback::Invert),
        [space_align],
    );

    // Delete (Backspace) button
    let del_icon = t.icon(Icon::Backspace, 44, INK);
    let del_align = t.align(0.5, 0.5, del_icon);
    let del_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(140)),
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .bg(255)
        .border(1, 0, 20)
        .action(action::DELETE)
        .feedback(Feedback::Invert),
        [del_align],
    );

    // Confirm / Action button (Search)
    let action_txt = t.label(
        action_label,
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .bold()
            .gray(255),
    );
    let action_align = t.align(0.5, 0.5, action_txt);
    let action_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(180)),
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .bg(0)
        .border(0, 0, 20)
        .action(action::CONFIRM)
        .feedback(Feedback::Invert),
        [action_align],
    );

    let bottom_row = t.add(
        Node::row(s.density.px(12)).cross(Cross::Center),
        [mode_seg, num_btn, space_btn, del_btn, action_btn],
    );

    let bottom_row = t.pad(Insets::new(s.density.px(14), 0, 0, 0), bottom_row);

    t.add(Node::column(0), [rows_col, bottom_row])
}

/// Builds the numeric/symbol page.
fn build_symbol_page(t: &mut Tree, s: &Styles, action_label: &str) -> NodeId {
    let row1 = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'];
    let row2 = ['-', '/', ':', ';', '(', ')', '¥', '$', '&', '@'];
    // The full stop the login form needs: no email address and no server URL
    // can be typed without it (device, QA-103).
    let row3 = ['!', '?', '\'', '"', '#', '%', '*', '+', '=', '.'];

    let r1_nodes: Vec<NodeId> = row1
        .iter()
        .map(|&c| build_key(t, s, &c.to_string(), c, 94, 136, 36.0))
        .collect();
    let r1 = t.add(Node::row(s.density.px(8)).cross(Cross::Center), r1_nodes);

    let r2_nodes: Vec<NodeId> = row2
        .iter()
        .map(|&c| build_key(t, s, &c.to_string(), c, 94, 136, 36.0))
        .collect();
    let r2 = t.add(Node::row(s.density.px(8)).cross(Cross::Center), r2_nodes);

    let r3_nodes: Vec<NodeId> = row3
        .iter()
        .map(|&c| build_key(t, s, &c.to_string(), c, 94, 136, 36.0))
        .collect();
    let r3 = t.add(Node::row(s.density.px(8)).cross(Cross::Center), r3_nodes);

    let rows_col = t.add(
        Node::column(s.density.px(14)).cross(Cross::Center),
        [r1, r2, r3],
    );

    // Bottom row: [ABC] [空格] [Backspace] [Action]
    let abc_label = t.label(
        "ABC",
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK),
    );
    let abc_align = t.align(0.5, 0.5, abc_label);
    let abc_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(200)),
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .bg(255)
        .border(1, 0, 20)
        .action(action::TOGGLE_PAGE)
        .feedback(Feedback::Invert),
        [abc_align],
    );

    let space_label = t.label(
        tr(Msg::ImeSpace),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK),
    );
    let space_align = t.align(0.5, 0.5, space_label);
    let space_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .flex(1)
        .bg(255)
        .border(1, 0, 20)
        .action(action::SPACE)
        .feedback(Feedback::Invert),
        [space_align],
    );

    let del_icon = t.icon(Icon::Backspace, 44, INK);
    let del_align = t.align(0.5, 0.5, del_icon);
    let del_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(140)),
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .bg(255)
        .border(1, 0, 20)
        .action(action::DELETE)
        .feedback(Feedback::Invert),
        [del_align],
    );

    let action_txt = t.label(
        action_label,
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .bold()
            .gray(255),
    );
    let action_align = t.align(0.5, 0.5, action_txt);
    let action_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(180)),
            h: Some(s.density.px(108)),
        })
        .in_pixels()
        .bg(0)
        .border(0, 0, 20)
        .action(action::CONFIRM)
        .feedback(Feedback::Invert),
        [action_align],
    );

    let bottom_row = t.add(
        Node::row(s.density.px(12)).cross(Cross::Center),
        [abc_btn, space_btn, del_btn, action_btn],
    );

    let bottom_row = t.pad(Insets::new(s.density.px(14), 0, 0, 0), bottom_row);

    t.add(Node::column(0), [rows_col, bottom_row])
}

/// Builds the complete keyboard component including candidate bar and keys.
pub fn build_keyboard(
    t: &mut Tree,
    s: &Styles,
    state: &KeyboardState,
    action_label: &str,
) -> NodeId {
    let cand_bar = build_candidate_bar(t, s, state);

    let keys = match state.page {
        KeyboardPage::Letters => build_letter_page(t, s, state, action_label),
        KeyboardPage::Symbols => build_symbol_page(t, s, action_label),
    };

    let col = t.add(Node::column(0), [cand_bar, keys]);

    let pad = t.pad(
        Insets::new(
            s.density.px(16),
            s.density.px(20),
            s.density.px(20),
            s.density.px(20),
        ),
        col,
    );

    // Outer panel: Gray-14 (#F2F2F2 / 238 in 16-level grayscale), rounded top corners (radius 32)
    t.add(
        Node::new(Kind::Fixed { w: None, h: None })
            .bg(238)
            .border(0, 0, 32),
        [pad],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Screen;
    use crate::text::Fonts;
    use crate::tree::NodeId;
    use crate::ui::Ui;

    #[test]
    fn keyboard_keys_are_all_at_least_64px() {
        let mut t = Tree::new();
        let s = Styles::new(FamilyId(0));
        let state = KeyboardState::new();

        let kb = build_keyboard(&mut t, &s, &state, "搜索");
        t.set_root(kb);

        let profile = Screen::new(1264, 1680, 300).unwrap();
        let text = TextEngine::new(Fonts::embedded());
        let mut ui = Ui::with_screen(t, text, profile);
        ui.layout();

        // Check every interactive button in the tree
        for i in 0..ui.tree.len() {
            let node = ui.tree.node(NodeId(i as u32));
            if let Some(act) = node.action {
                let rect = node.rect;
                assert!(
                    rect.w >= 64,
                    "Action {act} (node {i}) width {} < 64 px",
                    rect.w
                );
                assert!(
                    rect.h >= 64,
                    "Action {act} (node {i}) height {} < 64 px",
                    rect.h
                );
            }
        }
    }

    #[test]
    fn symbol_page_keys_are_all_at_least_64px() {
        let mut t = Tree::new();
        let s = Styles::new(FamilyId(0));
        let mut state = KeyboardState::new();
        state.toggle_page();
        assert_eq!(state.page, KeyboardPage::Symbols);

        let kb = build_keyboard(&mut t, &s, &state, "搜索");
        t.set_root(kb);

        let profile = Screen::new(1264, 1680, 300).unwrap();
        let text = TextEngine::new(Fonts::embedded());
        let mut ui = Ui::with_screen(t, text, profile);
        ui.layout();

        for i in 0..ui.tree.len() {
            let node = ui.tree.node(NodeId(i as u32));
            if let Some(act) = node.action {
                let rect = node.rect;
                assert!(
                    rect.w >= 64,
                    "Action {act} (node {i}) width {} < 64 px",
                    rect.w
                );
                assert!(
                    rect.h >= 64,
                    "Action {act} (node {i}) height {} < 64 px",
                    rect.h
                );
            }
        }
    }

    #[test]
    fn symbol_page_can_type_an_email_address() {
        // The login form asks for an email and the sync settings for a URL:
        // both need the full stop, which nothing else on the keyboard has.
        let mut t = Tree::new();
        let s = Styles::new(FamilyId(0));
        let mut state = KeyboardState::new();
        state.toggle_page();
        let kb = build_keyboard(&mut t, &s, &state, "登录");
        t.set_root(kb);
        let profile = Screen::new(1264, 1680, 300).unwrap();
        let text = TextEngine::new(Fonts::embedded());
        let mut ui = Ui::with_screen(t, text, profile);
        ui.layout();
        let dot = action::KEY_BASE + '.' as u32;
        let at = action::KEY_BASE + '@' as u32;
        for action in [dot, at] {
            let found = (0..ui.tree.len())
                .map(|i| NodeId(i as u32))
                .any(|n| ui.tree.node(n).action == Some(action));
            assert!(found, "no key for action {action} on the symbol page");
        }
    }
}
