// SPDX-License-Identifier: AGPL-3.0-or-later
//! Quotation entry screen: add a user quote with category, author, and source.
//!
//! Reuses the on-screen keyboard and pinyin input method from `crate::keyboard`.

use super::home::quotes::QuoteCategory;
use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::keyboard::{
    action as kb_action, build_input_box, build_keyboard, InputMode, KeyboardState,
};
use crate::text::{Align, FamilyId, NoHyphenation, Paragraph, TextStyle};
use crate::theme::{home_margin, Styles, INK, INK_2, INK_3, PAPER};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    pub const CANCEL: ActionId = 1500;
    pub const SAVE: ActionId = 1501;
    pub const CATEGORY_READING: ActionId = 1502;
    pub const CATEGORY_MOOD: ActionId = 1503;
    pub const FOCUS_QUOTE: ActionId = 1510;
    pub const FOCUS_AUTHOR: ActionId = 1511;
    pub const FOCUS_SOURCE: ActionId = 1512;
}

/// Which field is currently focused for text entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QuoteField {
    #[default]
    Text,
    Author,
    Source,
}

/// The state of the quote entry screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteAddState {
    pub active_field: QuoteField,
    pub text: String,
    pub author: String,
    pub source: String,
    pub category: QuoteCategory,
    pub keyboard: KeyboardState,
}

impl QuoteAddState {
    /// Creates a fresh quote entry state with the given default category.
    pub fn new(category: QuoteCategory) -> Self {
        Self {
            active_field: QuoteField::Text,
            text: String::new(),
            author: String::new(),
            source: String::new(),
            category,
            keyboard: KeyboardState::new(),
        }
    }

    /// Pre-populates the entry state with existing values.
    pub fn with_values(category: QuoteCategory, text: &str, author: &str, source: &str) -> Self {
        let mut s = Self::new(category);
        s.text = text.to_string();
        s.author = author.to_string();
        s.source = source.to_string();
        s.keyboard.committed = text.to_string();
        s
    }

    /// Sets the active focused field, committing any uncommitted pinyin and
    /// synchronizing keyboard state.
    pub fn switch_field(&mut self, field: QuoteField) {
        if self.active_field == field {
            return;
        }
        self.sync_committed_from_keyboard();
        self.active_field = field;
        let new_text = match field {
            QuoteField::Text => self.text.clone(),
            QuoteField::Author => self.author.clone(),
            QuoteField::Source => self.source.clone(),
        };
        self.keyboard.committed = new_text;
        self.keyboard.pinyin.clear();
        self.keyboard.all_candidates.clear();
        self.keyboard.cand_page = 0;
    }

    /// Commits any uncommitted pinyin and copies `keyboard.committed` to the active field.
    pub fn sync_committed_from_keyboard(&mut self) {
        if !self.keyboard.pinyin.is_empty() {
            if !self.keyboard.all_candidates.is_empty() {
                self.keyboard
                    .committed
                    .push(self.keyboard.all_candidates[0]);
            } else {
                self.keyboard.committed.push_str(&self.keyboard.pinyin);
            }
            self.keyboard.pinyin.clear();
            self.keyboard.all_candidates.clear();
            self.keyboard.cand_page = 0;
        }
        match self.active_field {
            QuoteField::Text => self.text = self.keyboard.committed.clone(),
            QuoteField::Author => self.author = self.keyboard.committed.clone(),
            QuoteField::Source => self.source = self.keyboard.committed.clone(),
        }
    }

    /// Handles a UI action event and returns the outcome.
    pub fn handle_action(&mut self, id: ActionId) -> QuoteAddOutcome {
        match id {
            action::CANCEL => QuoteAddOutcome::Cancel,
            action::SAVE => self.try_save(),
            action::CATEGORY_READING => {
                if self.category != QuoteCategory::Reading {
                    self.category = QuoteCategory::Reading;
                    QuoteAddOutcome::Changed
                } else {
                    QuoteAddOutcome::None
                }
            }
            action::CATEGORY_MOOD => {
                if self.category != QuoteCategory::Mood {
                    self.category = QuoteCategory::Mood;
                    QuoteAddOutcome::Changed
                } else {
                    QuoteAddOutcome::None
                }
            }
            action::FOCUS_QUOTE => {
                self.switch_field(QuoteField::Text);
                QuoteAddOutcome::Changed
            }
            action::FOCUS_AUTHOR => {
                self.switch_field(QuoteField::Author);
                QuoteAddOutcome::Changed
            }
            action::FOCUS_SOURCE => {
                self.switch_field(QuoteField::Source);
                QuoteAddOutcome::Changed
            }
            // Keyboard candidate pagination
            kb_action::CAND_PREV => {
                self.keyboard.prev_cand_page();
                QuoteAddOutcome::Changed
            }
            kb_action::CAND_NEXT => {
                self.keyboard.next_cand_page();
                QuoteAddOutcome::Changed
            }
            // Keyboard candidate selection
            id if (kb_action::CAND_BASE..kb_action::CAND_BASE + 10).contains(&id) => {
                let idx = (id - kb_action::CAND_BASE) as usize;
                if self.keyboard.pick_candidate(idx) {
                    self.sync_committed_from_keyboard();
                    QuoteAddOutcome::Changed
                } else {
                    QuoteAddOutcome::None
                }
            }
            // Keyboard mode toggles
            kb_action::MODE_ZH => {
                self.keyboard.set_mode(InputMode::Chinese);
                self.sync_committed_from_keyboard();
                QuoteAddOutcome::Changed
            }
            kb_action::MODE_EN => {
                self.keyboard.set_mode(InputMode::English);
                self.sync_committed_from_keyboard();
                QuoteAddOutcome::Changed
            }
            kb_action::TOGGLE_PAGE => {
                self.keyboard.toggle_page();
                QuoteAddOutcome::Changed
            }
            kb_action::SHIFT => {
                self.keyboard.press_shift();
                QuoteAddOutcome::Changed
            }
            kb_action::SPACE => {
                self.keyboard.press_space();
                self.sync_committed_from_keyboard();
                QuoteAddOutcome::Changed
            }
            kb_action::DELETE => {
                self.keyboard.press_delete();
                self.sync_committed_from_keyboard();
                QuoteAddOutcome::Changed
            }
            // Key character press
            id if (kb_action::KEY_BASE..kb_action::KEY_BASE + 0x1000).contains(&id) => {
                if let Some(ch) = char::from_u32(id - kb_action::KEY_BASE) {
                    self.keyboard.press_key(ch);
                    self.sync_committed_from_keyboard();
                    QuoteAddOutcome::Changed
                } else {
                    QuoteAddOutcome::None
                }
            }
            // Confirm action on keyboard
            kb_action::CONFIRM => self.try_save(),
            _ => QuoteAddOutcome::None,
        }
    }

    fn try_save(&mut self) -> QuoteAddOutcome {
        self.sync_committed_from_keyboard();
        let trimmed_text = self.text.trim();
        if trimmed_text.is_empty() {
            // Quotation is required; do not save empty quotation
            QuoteAddOutcome::None
        } else {
            QuoteAddOutcome::Save {
                text: trimmed_text.to_string(),
                author: self.author.trim().to_string(),
                source: self.source.trim().to_string(),
                category: self.category,
            }
        }
    }
}

/// The outcome of processing an action event on the quote entry screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuoteAddOutcome {
    None,
    Changed,
    Cancel,
    Save {
        text: String,
        author: String,
        source: String,
        category: QuoteCategory,
    },
}

/// Inactive input box widget displaying committed text or placeholder without active cursor.
#[derive(Clone, Debug)]
struct InactiveInputBoxWidget {
    text: String,
    placeholder: String,
}

impl Widget for InactiveInputBoxWidget {
    fn measure(&mut self, c: Constraints, _text: &mut crate::text::TextEngine) -> Size {
        Size::new(c.max.w, 88)
    }

    fn paint(&self, rect: Rect, cx: &mut PaintCx) {
        let p = &mut cx.painter;
        let radius = 16.0;

        // White background with 1px black rounded border
        p.fill_round_rect(rect, radius, 255);
        p.stroke_round_rect(rect, radius, 1.0, 0);

        let style = TextStyle::new(FamilyId(0), 36.0).gray(INK);
        let placeholder_style = TextStyle::new(FamilyId(0), 36.0).gray(INK_3);

        let pad_x = 24.0;
        let curr_x = rect.x as f32 + pad_x;
        let baseline_y = rect.y as f32 + (rect.h as f32 - 44.0) / 2.0;

        if self.text.is_empty() {
            if !self.placeholder.is_empty() {
                let para = Paragraph::plain(&self.placeholder, placeholder_style, Align::Left);
                let layout = crate::text::layout_paragraph(
                    cx.text,
                    &para,
                    rect.w as f32 - pad_x * 2.0,
                    &NoHyphenation,
                );
                layout.draw(cx.text, p, curr_x, baseline_y);
            }
        } else {
            let para = Paragraph::plain(&self.text, style, Align::Left);
            let layout = crate::text::layout_paragraph(
                cx.text,
                &para,
                rect.w as f32 - pad_x * 2.0,
                &NoHyphenation,
            );
            layout.draw(cx.text, p, curr_x, baseline_y);
        }
    }
}

/// UI handle for the quote entry screen.
pub struct QuoteAddUi {
    pub state: QuoteAddState,
}

impl QuoteAddUi {
    /// Builds the quote entry screen tree.
    pub fn build(s: &Styles, state: &QuoteAddState, status: &StatusLine) -> (Tree, Self) {
        let mut t = Tree::new();
        let (root, ui) = build_quote_add_screen(&mut t, s, state, status);
        t.set_root(root);
        (t, ui)
    }
}

/// Builds the quote entry screen tree and returns root NodeId and handle.
pub fn build_quote_add_screen(
    t: &mut Tree,
    s: &Styles,
    state: &QuoteAddState,
    status: &StatusLine,
) -> (NodeId, QuoteAddUi) {
    // 1. Status bar
    let bar = status_line(t, s, status);
    let status_bar = t.pad(Insets::vh(0, home_margin()), bar.root);
    let status_fixed = t.fixed(None, Some(88), Some(status_bar));

    // 2. Header bar: [‹ 返回] [添加句子] [保存]
    let back_icon = t.icon(Icon::ChevronLeft, 44, INK);
    let back_pad = t.pad(Insets::all(14), back_icon);
    let back_btn = t.add(
        Node::row(0)
            .cross(Cross::Center)
            .action(action::CANCEL)
            .feedback(Feedback::Invert),
        [back_pad],
    );

    let title_label = t.label(
        tr(Msg::QuotesAddQuoteTitle),
        s.font_lg().bold().line_height(1.2).tracking(4.0),
    );

    let save_text = t.label(
        tr(Msg::QuotesSave),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .bold()
            .gray(PAPER),
    );
    let save_pad = t.pad(Insets::vh(16, 32), save_text);
    let can_save = !state.text.trim().is_empty() || !state.keyboard.committed.trim().is_empty();
    let save_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(72)),
        })
        .in_pixels()
        .bg(if can_save { INK } else { INK_3 })
        .border(0, INK, 20)
        .action(action::SAVE)
        .feedback(Feedback::Invert),
        [save_pad],
    );

    let header_left = t.add(Node::row(20).cross(Cross::Center), [back_btn, title_label]);
    let spacer = t.spacer(1);
    let header_row = t.add(
        Node::row(16).cross(Cross::Center),
        [header_left, spacer, save_btn],
    );
    let header_bar = t.pad(Insets::vh(0, home_margin()), header_row);
    let header_fixed = t.fixed(None, Some(112), Some(header_bar));

    // 3. Middle Form area
    let mut form_nodes = Vec::new();

    // 3.1 Category Selector: [分类]  [ 读书 ]  [ 心情 ]
    let cat_label = t.label(
        tr(Msg::QuotesCategoryLabel),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK_2),
    );

    let reading_active = state.category == QuoteCategory::Reading;
    let reading_text = t.label(
        tr(Msg::QuotesCategoryReading),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(if reading_active { PAPER } else { INK }),
    );
    let reading_pad = t.pad(Insets::vh(16, 28), reading_text);
    let reading_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(68)),
        })
        .in_pixels()
        .bg(if reading_active { INK } else { PAPER })
        .border(if reading_active { 0 } else { 2 }, INK, 18)
        .action(action::CATEGORY_READING)
        .feedback(Feedback::Invert),
        [reading_pad],
    );

    let mood_active = state.category == QuoteCategory::Mood;
    let mood_text = t.label(
        tr(Msg::QuotesCategoryMood),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(if mood_active { PAPER } else { INK }),
    );
    let mood_pad = t.pad(Insets::vh(16, 28), mood_text);
    let mood_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(68)),
        })
        .in_pixels()
        .bg(if mood_active { INK } else { PAPER })
        .border(if mood_active { 0 } else { 2 }, INK, 18)
        .action(action::CATEGORY_MOOD)
        .feedback(Feedback::Invert),
        [mood_pad],
    );

    let cat_row = t.add(
        Node::row(s.density.px(16)).cross(Cross::Center),
        [cat_label, reading_btn, mood_btn],
    );
    form_nodes.push(cat_row);

    // 3.2 Field: 句子（必填）
    let q_label = t.label(
        tr(Msg::QuotesFieldQuote),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .bold()
            .gray(INK),
    );
    let q_input = if state.active_field == QuoteField::Text {
        build_input_box(
            t,
            s,
            &state.keyboard.committed,
            &state.keyboard.pinyin,
            tr(Msg::QuotesFieldQuotePlaceholder),
        )
    } else {
        t.add(
            Node::new(Kind::Custom(Box::new(InactiveInputBoxWidget {
                text: state.text.clone(),
                placeholder: tr(Msg::QuotesFieldQuotePlaceholder).to_string(),
            }))),
            [],
        )
    };
    let q_box = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(88)),
        })
        .in_pixels()
        .action(action::FOCUS_QUOTE)
        .feedback(Feedback::Invert),
        [q_input],
    );
    let q_group = t.add(Node::column(s.density.px(6)), [q_label, q_box]);
    form_nodes.push(t.pad(Insets::new(s.density.px(8), 0, 0, 0), q_group));

    // 3.3 Field: 作者（可选）
    let a_label = t.label(
        tr(Msg::QuotesFieldAuthor),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK_2),
    );
    let a_input = if state.active_field == QuoteField::Author {
        build_input_box(
            t,
            s,
            &state.keyboard.committed,
            &state.keyboard.pinyin,
            tr(Msg::QuotesFieldAuthorPlaceholder),
        )
    } else {
        t.add(
            Node::new(Kind::Custom(Box::new(InactiveInputBoxWidget {
                text: state.author.clone(),
                placeholder: tr(Msg::QuotesFieldAuthorPlaceholder).to_string(),
            }))),
            [],
        )
    };
    let a_box = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(88)),
        })
        .in_pixels()
        .action(action::FOCUS_AUTHOR)
        .feedback(Feedback::Invert),
        [a_input],
    );
    let a_group = t.add(Node::column(s.density.px(6)), [a_label, a_box]);
    form_nodes.push(t.pad(Insets::new(s.density.px(8), 0, 0, 0), a_group));

    // 3.4 Field: 出处（可选）
    let src_label = t.label(
        tr(Msg::QuotesFieldSource),
        TextStyle::new(FamilyId(0), 32.0)
            .at_density(s.density)
            .gray(INK_2),
    );
    let src_input = if state.active_field == QuoteField::Source {
        build_input_box(
            t,
            s,
            &state.keyboard.committed,
            &state.keyboard.pinyin,
            tr(Msg::QuotesFieldSourcePlaceholder),
        )
    } else {
        t.add(
            Node::new(Kind::Custom(Box::new(InactiveInputBoxWidget {
                text: state.source.clone(),
                placeholder: tr(Msg::QuotesFieldSourcePlaceholder).to_string(),
            }))),
            [],
        )
    };
    let src_box = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(s.density.px(88)),
        })
        .in_pixels()
        .action(action::FOCUS_SOURCE)
        .feedback(Feedback::Invert),
        [src_input],
    );
    let src_group = t.add(Node::column(s.density.px(6)), [src_label, src_box]);
    form_nodes.push(t.pad(Insets::new(s.density.px(8), 0, 0, 0), src_group));

    let form_col = t.add(Node::column(s.density.px(10)), form_nodes);
    let form_area = t.pad(
        Insets::new(
            s.density.px(12),
            home_margin(),
            s.density.px(12),
            home_margin(),
        ),
        form_col,
    );

    let top_col = t.add(
        Node::column(0).flex(1),
        [status_fixed, header_fixed, form_area],
    );

    // 4. Keyboard at bottom
    let kb_node = build_keyboard(t, s, &state.keyboard, tr(Msg::QuotesSave));

    let root = t.add(Node::column(0), [top_col, kb_node]);
    t.set_root(root);

    (
        root,
        QuoteAddUi {
            state: state.clone(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_initialization_defaults_to_text_field() {
        let state = QuoteAddState::new(QuoteCategory::Reading);
        assert_eq!(state.active_field, QuoteField::Text);
        assert_eq!(state.category, QuoteCategory::Reading);
        assert!(state.text.is_empty());
        assert!(state.author.is_empty());
        assert!(state.source.is_empty());
    }

    #[test]
    fn switching_fields_syncs_text_and_keyboard() {
        let mut state = QuoteAddState::new(QuoteCategory::Reading);
        state.keyboard.committed = "这是句子".into();
        state.switch_field(QuoteField::Author);

        assert_eq!(state.text, "这是句子");
        assert_eq!(state.active_field, QuoteField::Author);
        assert!(state.keyboard.committed.is_empty());

        state.keyboard.committed = "作者名".into();
        state.switch_field(QuoteField::Source);
        assert_eq!(state.author, "作者名");
        assert_eq!(state.active_field, QuoteField::Source);

        state.keyboard.committed = "《出处书》".into();
        state.switch_field(QuoteField::Text);
        assert_eq!(state.source, "《出处书》");
        assert_eq!(state.active_field, QuoteField::Text);
        assert_eq!(state.keyboard.committed, "这是句子");
    }

    #[test]
    fn try_save_validates_required_quote_text() {
        let mut state = QuoteAddState::new(QuoteCategory::Reading);
        // Empty text cannot save
        assert_eq!(state.handle_action(action::SAVE), QuoteAddOutcome::None);

        state.keyboard.committed = "  ".into();
        assert_eq!(state.handle_action(action::SAVE), QuoteAddOutcome::None);

        // With quotation text, save succeeds
        state.keyboard.committed = "纸上得来终觉浅".into();
        state.author = "陆游".into();
        state.source = "《冬夜读书示子聿》".into();
        let outcome = state.handle_action(action::SAVE);
        assert_eq!(
            outcome,
            QuoteAddOutcome::Save {
                text: "纸上得来终觉浅".into(),
                author: "陆游".into(),
                source: "《冬夜读书示子聿》".into(),
                category: QuoteCategory::Reading,
            }
        );
    }

    #[test]
    fn category_switching() {
        let mut state = QuoteAddState::new(QuoteCategory::Reading);
        assert_eq!(
            state.handle_action(action::CATEGORY_MOOD),
            QuoteAddOutcome::Changed
        );
        assert_eq!(state.category, QuoteCategory::Mood);

        assert_eq!(
            state.handle_action(action::CATEGORY_READING),
            QuoteAddOutcome::Changed
        );
        assert_eq!(state.category, QuoteCategory::Reading);
    }

    #[test]
    fn cancel_returns_cancel_outcome() {
        let mut state = QuoteAddState::new(QuoteCategory::Mood);
        assert_eq!(state.handle_action(action::CANCEL), QuoteAddOutcome::Cancel);
    }
}
