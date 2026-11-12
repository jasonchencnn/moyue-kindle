// SPDX-License-Identifier: AGPL-3.0-or-later
//! Screen for Readest account login: email, password, error display, and on-screen keyboard.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::keyboard::{
    action as kb_action, build_input_box, build_input_box_secret, build_keyboard,
    to_simplified_str, InputMode, KeyboardState,
};
use crate::text::{layout_paragraph, Align, FamilyId, NoHyphenation, Paragraph, TextStyle};
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_3, PAPER};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const FOCUS_EMAIL: ActionId = 10;
    pub const FOCUS_PASSWORD: ActionId = 11;
    pub const SUBMIT: ActionId = 12;
    /// The low-key link to the server settings, for signing in against a
    /// self-hosted instance or a mock server.
    pub const SETTINGS: ActionId = 13;
    pub const TOGGLE_SHOW_PASSWORD: ActionId = 14;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadestLoginField {
    Email,
    Password,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ReadestLoginOutcome {
    None,
    Changed,
    Cancel,
    OpenSettings,
    Submit { email: String, password: String },
}

impl std::fmt::Debug for ReadestLoginOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::Changed => write!(f, "Changed"),
            Self::Cancel => write!(f, "Cancel"),
            Self::OpenSettings => write!(f, "OpenSettings"),
            Self::Submit { email, .. } => f
                .debug_struct("Submit")
                .field("email", email)
                .field("password", &"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct ReadestLoginState {
    pub email: String,
    pub password: String,
    pub active_field: ReadestLoginField,
    pub keyboard: KeyboardState,
    pub error_message: Option<String>,
    pub is_logging_in: bool,
    /// Shows the password in plain text instead of dots.
    pub show_password: bool,
}

impl std::fmt::Debug for ReadestLoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadestLoginState")
            .field("email", &self.email)
            .field("password", &"[REDACTED]")
            .field("active_field", &self.active_field)
            .field("keyboard", &self.keyboard)
            .field("error_message", &self.error_message)
            .field("is_logging_in", &self.is_logging_in)
            .field("show_password", &self.show_password)
            .finish()
    }
}

impl Default for ReadestLoginState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadestLoginState {
    pub fn new() -> Self {
        let mut keyboard = KeyboardState::new();
        // Default to English mode for email/password input
        keyboard.set_mode(InputMode::English);
        Self {
            email: String::new(),
            password: String::new(),
            active_field: ReadestLoginField::Email,
            keyboard,
            error_message: None,
            is_logging_in: false,
            show_password: false,
        }
    }

    pub fn with_email(email: &str) -> Self {
        let mut s = Self::new();
        s.email = email.to_string();
        s.keyboard.committed = email.to_string();
        s
    }

    pub fn switch_field(&mut self, field: ReadestLoginField) {
        if self.active_field == field {
            return;
        }
        self.sync_committed_from_keyboard();
        self.active_field = field;
        let new_text = match field {
            ReadestLoginField::Email => self.email.clone(),
            ReadestLoginField::Password => self.password.clone(),
        };
        self.keyboard.committed = new_text;
        self.keyboard.pinyin.clear();
        self.keyboard.all_candidates.clear();
        self.keyboard.cand_page = 0;
    }

    pub fn sync_committed_from_keyboard(&mut self) {
        if !self.keyboard.pinyin.is_empty() {
            if self.keyboard.mode == InputMode::Chinese {
                let converted = to_simplified_str(&self.keyboard.pinyin);
                self.keyboard.committed.push_str(&converted);
            } else {
                self.keyboard.committed.push_str(&self.keyboard.pinyin);
            }
            self.keyboard.pinyin.clear();
            self.keyboard.all_candidates.clear();
            self.keyboard.cand_page = 0;
        }
        match self.active_field {
            ReadestLoginField::Email => self.email = self.keyboard.committed.clone(),
            ReadestLoginField::Password => self.password = self.keyboard.committed.clone(),
        }
    }

    pub fn handle_action(&mut self, id: ActionId) -> ReadestLoginOutcome {
        match id {
            action::BACK => ReadestLoginOutcome::Cancel,
            action::SETTINGS => ReadestLoginOutcome::OpenSettings,
            action::TOGGLE_SHOW_PASSWORD => {
                self.show_password = !self.show_password;
                ReadestLoginOutcome::Changed
            }
            action::FOCUS_EMAIL => {
                self.switch_field(ReadestLoginField::Email);
                ReadestLoginOutcome::Changed
            }
            action::FOCUS_PASSWORD => {
                self.switch_field(ReadestLoginField::Password);
                ReadestLoginOutcome::Changed
            }
            action::SUBMIT | kb_action::CONFIRM => {
                self.sync_committed_from_keyboard();
                let email = self.email.trim().to_string();
                let password = self.password.clone();
                if email.is_empty() {
                    self.error_message = Some(tr(Msg::ReadestEmailPlaceholder).to_string());
                    ReadestLoginOutcome::Changed
                } else if password.is_empty() {
                    self.error_message = Some(tr(Msg::ReadestPasswordPlaceholder).to_string());
                    ReadestLoginOutcome::Changed
                } else {
                    self.error_message = None;
                    self.is_logging_in = true;
                    ReadestLoginOutcome::Submit { email, password }
                }
            }
            // Keyboard actions
            kb_action::CAND_PREV => {
                self.keyboard.prev_cand_page();
                ReadestLoginOutcome::Changed
            }
            kb_action::CAND_NEXT => {
                self.keyboard.next_cand_page();
                ReadestLoginOutcome::Changed
            }
            id if (kb_action::CAND_BASE..kb_action::CAND_BASE + 10).contains(&id) => {
                let idx = (id - kb_action::CAND_BASE) as usize;
                if self.keyboard.pick_candidate(idx) {
                    self.sync_committed_from_keyboard();
                    ReadestLoginOutcome::Changed
                } else {
                    ReadestLoginOutcome::None
                }
            }
            kb_action::MODE_ZH => {
                self.keyboard.set_mode(InputMode::Chinese);
                self.sync_committed_from_keyboard();
                ReadestLoginOutcome::Changed
            }
            kb_action::MODE_EN => {
                self.keyboard.set_mode(InputMode::English);
                self.sync_committed_from_keyboard();
                ReadestLoginOutcome::Changed
            }
            kb_action::TOGGLE_PAGE => {
                self.keyboard.toggle_page();
                ReadestLoginOutcome::Changed
            }
            kb_action::SHIFT => {
                self.keyboard.press_shift();
                ReadestLoginOutcome::Changed
            }
            kb_action::SPACE => {
                self.keyboard.press_space();
                self.sync_committed_from_keyboard();
                ReadestLoginOutcome::Changed
            }
            kb_action::DELETE => {
                self.keyboard.press_delete();
                self.sync_committed_from_keyboard();
                ReadestLoginOutcome::Changed
            }
            id if (kb_action::KEY_BASE..kb_action::KEY_BASE + 0x1000).contains(&id) => {
                if let Some(ch) = char::from_u32(id - kb_action::KEY_BASE) {
                    self.keyboard.press_key(ch);
                    self.sync_committed_from_keyboard();
                    ReadestLoginOutcome::Changed
                } else {
                    ReadestLoginOutcome::None
                }
            }
            _ => ReadestLoginOutcome::None,
        }
    }
}

/// Inactive input box widget displaying text (optionally masked as secret) without active cursor.
#[derive(Clone, Debug)]
struct InactiveInputBoxWidget {
    text: String,
    placeholder: String,
    secret: bool,
}

impl Widget for InactiveInputBoxWidget {
    fn measure(&mut self, c: Constraints, _text: &mut crate::text::TextEngine) -> Size {
        Size::new(c.max.w, 88)
    }

    fn paint(&self, rect: Rect, cx: &mut PaintCx) {
        let p = &mut cx.painter;
        let radius = 16.0;

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
                let layout =
                    layout_paragraph(cx.text, &para, rect.w as f32 - pad_x * 2.0, &NoHyphenation);
                layout.draw(cx.text, p, curr_x, baseline_y);
            }
        } else {
            let display_text = if self.secret {
                "•".repeat(self.text.chars().count())
            } else {
                self.text.clone()
            };
            let para = Paragraph::plain(&display_text, style, Align::Left);
            let layout =
                layout_paragraph(cx.text, &para, rect.w as f32 - pad_x * 2.0, &NoHyphenation);
            layout.draw(cx.text, p, curr_x, baseline_y);
        }
    }
}

/// Builds the Readest login screen tree.
pub fn build_readest_login_screen(
    t: &mut Tree,
    s: &Styles,
    state: &ReadestLoginState,
    status: &StatusLine,
) -> NodeId {
    let bar = status_line(t, s, status);
    let status_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

    // Header: Back button + Title "登录 Readest"
    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(
        tr(Msg::ReadestLoginTitle),
        s.font_lg().bold().line_height(1.2),
    );
    let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    let header = t.fixed(None, Some(104), Some(header));
    let header_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

    let mut form_items = Vec::new();

    // Error message banner if present
    if let Some(err) = &state.error_message {
        let err_icon = t.icon(Icon::SquareInfo, 36, INK);
        let err_label = t.label(err, s.font_sm().bold().gray(INK));
        let banner_content = t.add(Node::row(16).cross(Cross::Center), [err_icon, err_label]);
        let banner = t.add(
            Node::new(Kind::Padding(Insets::vh(16, 24)))
                .bg(CARD)
                .border(2, INK, 16),
            [banner_content],
        );
        form_items.push(banner);
        form_items.push(t.gap(0, 16));
    }

    // Email row
    let email_label = t.label(tr(Msg::ReadestEmail), s.font_sm().bold());
    let email_box = if state.active_field == ReadestLoginField::Email {
        build_input_box(
            t,
            s,
            &state.keyboard.committed,
            &state.keyboard.pinyin,
            tr(Msg::ReadestEmailPlaceholder),
        )
    } else {
        t.add(
            Node::new(Kind::Custom(Box::new(InactiveInputBoxWidget {
                text: state.email.clone(),
                placeholder: tr(Msg::ReadestEmailPlaceholder).to_string(),
                secret: false,
            }))),
            [],
        )
    };
    let email_touch = t.add(
        Node::new(Kind::MinSize(Size::new(0, 88)))
            .action(action::FOCUS_EMAIL)
            .feedback(Feedback::None),
        [email_box],
    );
    form_items.push(email_label);
    form_items.push(t.gap(0, 8));
    form_items.push(email_touch);
    form_items.push(t.gap(0, 20));

    // Password row
    let password_label = t.label(tr(Msg::ReadestPassword), s.font_sm().bold());
    let password_box = if state.active_field == ReadestLoginField::Password {
        build_input_box_secret(
            t,
            s,
            &state.keyboard.committed,
            &state.keyboard.pinyin,
            tr(Msg::ReadestPasswordPlaceholder),
            !state.show_password,
        )
    } else {
        t.add(
            Node::new(Kind::Custom(Box::new(InactiveInputBoxWidget {
                text: state.password.clone(),
                placeholder: tr(Msg::ReadestPasswordPlaceholder).to_string(),
                secret: !state.show_password,
            }))),
            [],
        )
    };
    let password_touch = t.add(
        Node::new(Kind::MinSize(Size::new(0, 88)))
            .flex(1)
            .action(action::FOCUS_PASSWORD)
            .feedback(Feedback::None),
        [password_box],
    );
    let eye_icon = t.icon(
        if state.show_password {
            Icon::EyeOff
        } else {
            Icon::Eye
        },
        44,
        INK_2,
    );
    let eye_center = t.align(0.5, 0.5, eye_icon);
    let eye_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(88)),
            h: Some(s.density.px(88)),
        })
        .in_pixels()
        .action(action::TOGGLE_SHOW_PASSWORD)
        .feedback(Feedback::Invert),
        [eye_center],
    );
    let password_touch = t.add(
        Node::row(s.density.px(8)).cross(Cross::Center),
        [password_touch, eye_btn],
    );
    form_items.push(password_label);
    form_items.push(t.gap(0, 8));
    form_items.push(password_touch);
    form_items.push(t.gap(0, 24));

    // Submit button ("登录" / "Sign in")
    let btn_text = if state.is_logging_in {
        tr(Msg::ReadestLoggingIn)
    } else {
        tr(Msg::ReadestLoginBtn)
    };
    let btn_label = t.label(btn_text, s.font_base().bold().gray(PAPER));
    let btn_center = t.align(0.5, 0.5, btn_label);
    let submit_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(88),
        })
        .bg(INK)
        .border(0, INK, 24)
        .action(action::SUBMIT)
        .feedback(Feedback::Invert),
        [btn_center],
    );
    form_items.push(submit_btn);
    form_items.push(t.gap(0, 12));

    // Secondary entry: configure the server before the first sign-in.
    let settings_label = t.label(tr(Msg::ReadestServerSettings), s.font_link().gray(INK_2));
    let settings_center = t.align(0.5, 0.5, settings_label);
    let settings_link = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(88),
        })
        .action(action::SETTINGS)
        .feedback(Feedback::Invert),
        [settings_center],
    );
    form_items.push(settings_link);

    let form_col = t.add(Node::column(0).flex(1), form_items);
    let form_pad = t.pad(Insets::new(16, home_margin(), 16, home_margin()), form_col);

    // Keyboard at bottom
    let kb_node = build_keyboard(t, s, &state.keyboard, tr(Msg::ReadestLoginBtn));

    t.add(Node::column(0), [status_pad, header_pad, form_pad, kb_node])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_password() {
        let mut state = ReadestLoginState::new();
        state.email = "reader@example.com".into();
        state.password = "hunter2-secret".into();

        let debug = format!("{state:?}");
        assert!(
            !debug.contains("hunter2-secret"),
            "password leaked into Debug: {debug}"
        );
        assert!(debug.contains("[REDACTED]"));

        let outcome = ReadestLoginOutcome::Submit {
            email: "reader@example.com".into(),
            password: "hunter2-secret".into(),
        };
        let debug = format!("{outcome:?}");
        assert!(
            !debug.contains("hunter2-secret"),
            "password leaked into Debug: {debug}"
        );
        assert!(debug.contains("[REDACTED]"));
    }

    fn find_text(t: &Tree, id: NodeId, text: &str) -> bool {
        match &t.node(id).kind {
            Kind::Text { para, .. } => para.spans.iter().any(|span| span.text == text),
            _ => t.node(id).children.iter().any(|&c| find_text(t, c, text)),
        }
    }

    #[test]
    fn show_password_toggles() {
        let mut state = ReadestLoginState::new();
        assert!(!state.show_password);
        state.handle_action(action::TOGGLE_SHOW_PASSWORD);
        assert!(state.show_password);
        state.handle_action(action::TOGGLE_SHOW_PASSWORD);
        assert!(!state.show_password);
    }

    #[test]
    fn login_has_one_server_settings_entry() {
        let mut tree = Tree::new();
        let styles = Styles::new(FamilyId(0));
        let status = StatusLine {
            clock: "12:00",
            battery: None,
            charging: false,
        };
        build_readest_login_screen(&mut tree, &styles, &ReadestLoginState::new(), &status);

        let settings: Vec<NodeId> = (0..tree.len())
            .map(|i| NodeId(i as u32))
            .filter(|&id| tree.node(id).action == Some(action::SETTINGS))
            .collect();
        assert_eq!(settings.len(), 1, "one server settings entry");
        assert!(find_text(
            &tree,
            settings[0],
            tr(Msg::ReadestServerSettings)
        ));
        assert_eq!(
            ReadestLoginState::new().handle_action(action::SETTINGS),
            ReadestLoginOutcome::OpenSettings
        );
    }

    #[test]
    fn empty_fields_do_not_say_the_password_is_wrong() {
        let mut state = ReadestLoginState::new();
        state.email = "reader@example.com".into();
        state.keyboard.committed = state.email.clone();
        assert_eq!(
            state.handle_action(action::SUBMIT),
            ReadestLoginOutcome::Changed
        );
        assert_eq!(
            state.error_message.as_deref(),
            Some(tr(Msg::ReadestPasswordPlaceholder))
        );

        state.email.clear();
        state.password = "secret".into();
        state.keyboard.committed = String::new();
        state.active_field = ReadestLoginField::Email;
        assert_eq!(
            state.handle_action(action::SUBMIT),
            ReadestLoginOutcome::Changed
        );
        assert_eq!(
            state.error_message.as_deref(),
            Some(tr(Msg::ReadestEmailPlaceholder))
        );
    }
}
