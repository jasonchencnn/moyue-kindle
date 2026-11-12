// SPDX-License-Identifier: AGPL-3.0-or-later
//! One-line text editor used for the Readest endpoints.
//!
//! It reuses the on-screen keyboard and input box from [`crate::keyboard`];
//! saving returns the trimmed text to the caller.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::keyboard::{
    action as kb_action, build_input_box_secret, build_keyboard, to_simplified_str, InputMode,
    KeyboardState,
};
use crate::theme::{home_margin, Styles, CARD, INK, PAPER};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const SAVE: ActionId = 2;
}

/// What the editor did with an action.
#[derive(Clone, PartialEq, Eq)]
pub enum ReadestEditOutcome {
    None,
    Changed,
    Cancel,
    Save(String),
}

impl std::fmt::Debug for ReadestEditOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::Changed => write!(f, "Changed"),
            Self::Cancel => write!(f, "Cancel"),
            Self::Save(_) => write!(f, "Save([REDACTED])"),
        }
    }
}

/// State of the endpoint editor.
pub struct ReadestEditState {
    pub title: String,
    pub text: String,
    pub keyboard: KeyboardState,
    /// The value is anon-key-like: never shown or logged.
    pub secret: bool,
    pub error: Option<String>,
}

impl std::fmt::Debug for ReadestEditState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadestEditState")
            .field("title", &self.title)
            .field(
                "text",
                &if self.secret {
                    "[REDACTED]"
                } else {
                    &self.text
                },
            )
            .field("secret", &self.secret)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl ReadestEditState {
    pub fn new(title: String, initial: &str, secret: bool) -> Self {
        let mut keyboard = KeyboardState::new();
        keyboard.set_mode(InputMode::English);
        keyboard.committed = initial.to_string();
        Self {
            title,
            text: initial.to_string(),
            keyboard,
            secret,
            error: None,
        }
    }

    fn sync_committed(&mut self) {
        if !self.keyboard.pinyin.is_empty() {
            if self.keyboard.mode == InputMode::Chinese {
                self.keyboard
                    .committed
                    .push_str(&to_simplified_str(&self.keyboard.pinyin));
            } else {
                self.keyboard.committed.push_str(&self.keyboard.pinyin);
            }
            self.keyboard.pinyin.clear();
            self.keyboard.all_candidates.clear();
            self.keyboard.cand_page = 0;
        }
        self.text = self.keyboard.committed.clone();
    }

    pub fn handle_action(&mut self, id: ActionId) -> ReadestEditOutcome {
        match id {
            action::BACK => ReadestEditOutcome::Cancel,
            action::SAVE => {
                self.sync_committed();
                ReadestEditOutcome::Save(self.text.trim().to_string())
            }
            kb_action::CONFIRM => {
                self.sync_committed();
                ReadestEditOutcome::Save(self.text.trim().to_string())
            }
            kb_action::CAND_PREV => {
                self.keyboard.prev_cand_page();
                ReadestEditOutcome::Changed
            }
            kb_action::CAND_NEXT => {
                self.keyboard.next_cand_page();
                ReadestEditOutcome::Changed
            }
            id if (kb_action::CAND_BASE..kb_action::CAND_BASE + 10).contains(&id) => {
                if self
                    .keyboard
                    .pick_candidate((id - kb_action::CAND_BASE) as usize)
                {
                    self.sync_committed();
                    ReadestEditOutcome::Changed
                } else {
                    ReadestEditOutcome::None
                }
            }
            kb_action::MODE_ZH => {
                self.keyboard.set_mode(InputMode::Chinese);
                self.sync_committed();
                ReadestEditOutcome::Changed
            }
            kb_action::MODE_EN => {
                self.keyboard.set_mode(InputMode::English);
                self.sync_committed();
                ReadestEditOutcome::Changed
            }
            kb_action::TOGGLE_PAGE => {
                self.keyboard.toggle_page();
                ReadestEditOutcome::Changed
            }
            kb_action::SHIFT => {
                self.keyboard.press_shift();
                ReadestEditOutcome::Changed
            }
            kb_action::SPACE => {
                self.keyboard.press_space();
                self.sync_committed();
                ReadestEditOutcome::Changed
            }
            kb_action::DELETE => {
                self.keyboard.press_delete();
                self.sync_committed();
                ReadestEditOutcome::Changed
            }
            id if (kb_action::KEY_BASE..kb_action::KEY_BASE + 0x1000).contains(&id) => {
                if let Some(ch) = char::from_u32(id - kb_action::KEY_BASE) {
                    self.keyboard.press_key(ch);
                    self.sync_committed();
                    ReadestEditOutcome::Changed
                } else {
                    ReadestEditOutcome::None
                }
            }
            _ => ReadestEditOutcome::None,
        }
    }
}

fn button(t: &mut Tree, s: &Styles, text: &str, id: ActionId, filled: bool) -> NodeId {
    let label = t.label(
        text,
        s.font_base().bold().gray(if filled { PAPER } else { INK }),
    );
    let center = t.align(0.5, 0.5, label);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(80),
        })
        .bg(if filled { INK } else { CARD })
        .border(if filled { 0 } else { 2 }, INK, 20)
        .action(id)
        .feedback(Feedback::Invert),
        [center],
    )
}

/// Build the endpoint editor.
pub fn build_readest_edit_screen(
    t: &mut Tree,
    s: &Styles,
    state: &ReadestEditState,
    status: &StatusLine,
) -> NodeId {
    let bar = status_line(t, s, status);
    let status_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(&state.title, s.font_lg().bold().line_height(1.2));
    let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    let header = t.fixed(None, Some(104), Some(header));
    let header_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

    let mut items = Vec::new();
    if let Some(err) = &state.error {
        let label = t.label(err, s.font_sm().bold().gray(INK));
        items.push(
            t.add(
                Node::new(Kind::Padding(Insets::vh(16, 24)))
                    .bg(CARD)
                    .border(2, INK, 16),
                [label],
            ),
        );
        items.push(t.gap(0, 16));
    }
    items.push(build_input_box_secret(
        t,
        s,
        &state.keyboard.committed,
        &state.keyboard.pinyin,
        "",
        state.secret,
    ));
    items.push(t.gap(0, 24));
    items.push(button(t, s, tr(Msg::QuotesSave), action::SAVE, true));
    items.push(t.gap(0, 12));
    items.push(button(t, s, tr(Msg::CommonBack), action::BACK, false));

    let column = t.add(Node::column(0).flex(1), items);
    let pad = t.pad(Insets::new(16, home_margin(), 16, home_margin()), column);
    let keyboard = build_keyboard(t, s, &state.keyboard, tr(Msg::QuotesSave));
    t.add(Node::column(0), [status_pad, header_pad, pad, keyboard])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_returns_the_trimmed_text() {
        let mut state = ReadestEditState::new("API".into(), "  ", false);
        for ch in "https://example.com/api".chars() {
            state.handle_action(kb_action::KEY_BASE + ch as u32);
        }
        assert_eq!(
            state.handle_action(action::SAVE),
            ReadestEditOutcome::Save("https://example.com/api".into())
        );
    }

    #[test]
    fn debug_never_shows_a_secret_value() {
        let state = ReadestEditState::new("Anon".into(), "super-secret-key", true);
        let debug = format!("{state:?}");
        assert!(!debug.contains("super-secret-key"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn cancel_is_not_a_save() {
        let mut state = ReadestEditState::new("API".into(), "https://x", false);
        assert_eq!(
            state.handle_action(action::BACK),
            ReadestEditOutcome::Cancel
        );
    }
}
