// SPDX-License-Identifier: AGPL-3.0-or-later
//! Screen for the Nutstore (Jianguoyun) account setup: the WebDAV server
//! address, the email account, the app password and the remote folder, with
//! an on-screen keyboard.
//!
//! The password is the app password from Nutstore's own security settings,
//! never the account password: the WebDAV endpoint only accepts the third
//! party one. The field is masked until the reader asks to see it.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::keyboard::{
    action as kb_action, build_keyboard, to_simplified_str, InputMode, KeyboardState,
};
use crate::text::{layout_paragraph, Align, FamilyId, NoHyphenation, Paragraph, TextStyle};
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_3, INK_MID, PAPER};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use aristotle_i18n::{tr, Msg};

/// Nutstore's own WebDAV endpoint, the one the app password belongs to.
pub const DEFAULT_SERVER_URL: &str = "https://dav.jianguoyun.com/dav/";

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    /// The server address field.
    pub const FOCUS_SERVER_URL: ActionId = 10;
    /// The email account field.
    pub const FOCUS_EMAIL: ActionId = 11;
    /// The app password field.
    pub const FOCUS_PASSWORD: ActionId = 12;
    /// The remote folder field.
    pub const FOCUS_REMOTE_DIR: ActionId = 13;
    /// Test the connection and save the account.
    pub const SUBMIT: ActionId = 14;
    /// Show the app password in plain text.
    pub const TOGGLE_SHOW_PASSWORD: ActionId = 15;
}

/// The four fields of the setup form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JianguoyunLoginField {
    ServerUrl,
    Email,
    Password,
    RemoteDir,
}

impl JianguoyunLoginField {
    /// The fields in the order the form shows them.
    pub const ALL: [JianguoyunLoginField; 4] = [
        JianguoyunLoginField::ServerUrl,
        JianguoyunLoginField::Email,
        JianguoyunLoginField::Password,
        JianguoyunLoginField::RemoteDir,
    ];

    /// The next field, wrapping at the end.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The field before this one, wrapping at the start.
    pub fn previous(self) -> Self {
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// What the setup screen asks the app to do next.
#[derive(Clone, PartialEq, Eq)]
pub enum JianguoyunLoginOutcome {
    None,
    /// Something on the screen changed; redraw it.
    Changed,
    /// Leave the screen without saving.
    Cancel,
    /// Test the connection and save the account.
    Submit {
        server_url: String,
        email: String,
        password: String,
        remote_dir: String,
    },
}

impl std::fmt::Debug for JianguoyunLoginOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::Changed => write!(f, "Changed"),
            Self::Cancel => write!(f, "Cancel"),
            Self::Submit {
                server_url,
                email,
                remote_dir,
                ..
            } => f
                .debug_struct("Submit")
                .field("server_url", server_url)
                .field("email", email)
                .field("password", &"[REDACTED]")
                .field("remote_dir", remote_dir)
                .finish(),
        }
    }
}

/// The setup form's state: the four fields, the keyboard and the flags.
#[derive(Clone)]
pub struct JianguoyunLoginState {
    pub server_url: String,
    pub email: String,
    pub password: String,
    pub remote_dir: String,
    pub active_field: JianguoyunLoginField,
    pub keyboard: KeyboardState,
    last_synced_committed: String,
    pub error_message: Option<String>,
    pub is_saving: bool,
    /// Shows the app password in plain text instead of dots.
    pub show_password: bool,
}

impl std::fmt::Debug for JianguoyunLoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JianguoyunLoginState")
            .field("server_url", &self.server_url)
            .field("email", &self.email)
            .field("password", &"[REDACTED]")
            .field("remote_dir", &self.remote_dir)
            .field("active_field", &self.active_field)
            .field("keyboard", &self.keyboard)
            .field("error_message", &self.error_message)
            .field("is_saving", &self.is_saving)
            .field("show_password", &self.show_password)
            .finish()
    }
}

impl Default for JianguoyunLoginState {
    fn default() -> Self {
        Self::new()
    }
}

impl JianguoyunLoginState {
    pub fn new() -> Self {
        let mut keyboard = KeyboardState::new();
        // The server address, the mail and the password are all typed in
        // Latin letters, so the keyboard starts in English mode with the
        // pre-filled server URL already loaded.
        keyboard.set_mode(InputMode::English);
        keyboard.committed = DEFAULT_SERVER_URL.to_string();
        Self {
            server_url: DEFAULT_SERVER_URL.to_string(),
            email: String::new(),
            password: String::new(),
            remote_dir: String::new(),
            active_field: JianguoyunLoginField::ServerUrl,
            keyboard,
            last_synced_committed: DEFAULT_SERVER_URL.to_string(),
            error_message: None,
            is_saving: false,
            show_password: false,
        }
    }

    /// The text of the field the keyboard is typing into.
    pub fn active_text(&self) -> &str {
        match self.active_field {
            JianguoyunLoginField::ServerUrl => &self.server_url,
            JianguoyunLoginField::Email => &self.email,
            JianguoyunLoginField::Password => &self.password,
            JianguoyunLoginField::RemoteDir => &self.remote_dir,
        }
    }

    /// Moves the caret to `field` and loads its text into the keyboard.
    pub fn switch_field(&mut self, field: JianguoyunLoginField) {
        if self.active_field == field {
            return;
        }
        self.sync_committed_from_keyboard();
        self.active_field = field;
        let current = self.active_text().to_string();
        self.keyboard.committed = current.clone();
        self.last_synced_committed = current;
        self.keyboard.pinyin.clear();
        self.keyboard.all_candidates.clear();
        self.keyboard.cand_page = 0;
    }

    /// Synchronizes the keyboard buffer and the active field.
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
        if self.keyboard.committed != self.last_synced_committed {
            let text = self.keyboard.committed.clone();
            match self.active_field {
                JianguoyunLoginField::ServerUrl => self.server_url = text.clone(),
                JianguoyunLoginField::Email => self.email = text.clone(),
                JianguoyunLoginField::Password => self.password = text.clone(),
                JianguoyunLoginField::RemoteDir => self.remote_dir = text.clone(),
            }
            self.last_synced_committed = text;
        } else {
            let field_val = self.active_text().to_string();
            self.keyboard.committed = field_val.clone();
            self.last_synced_committed = field_val;
        }
    }

    /// The problem with the form, if it has one: the first field that is
    /// empty or wrong, in the order the form shows them.
    pub fn validate(&self) -> Option<Msg> {
        let url = self.server_url.trim();
        if url.is_empty() {
            return Some(Msg::JianguoyunErrServerUrl);
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) || url.contains('@') {
            return Some(Msg::JianguoyunErrServerUrl);
        }
        let dir = self.remote_dir.trim();
        if dir.contains('\\') || dir.split('/').any(|s| s == "..") {
            return Some(Msg::JianguoyunErrServerUrl);
        }
        if self.email.trim().is_empty() {
            return Some(Msg::JianguoyunErrEmail);
        }
        if self.password.is_empty() {
            return Some(Msg::JianguoyunErrPassword);
        }
        None
    }

    /// Applies an action to the form.
    pub fn handle_action(&mut self, id: ActionId) -> JianguoyunLoginOutcome {
        match id {
            action::BACK => JianguoyunLoginOutcome::Cancel,
            action::TOGGLE_SHOW_PASSWORD => {
                self.show_password = !self.show_password;
                JianguoyunLoginOutcome::Changed
            }
            action::FOCUS_SERVER_URL => {
                self.switch_field(JianguoyunLoginField::ServerUrl);
                JianguoyunLoginOutcome::Changed
            }
            action::FOCUS_EMAIL => {
                self.switch_field(JianguoyunLoginField::Email);
                JianguoyunLoginOutcome::Changed
            }
            action::FOCUS_PASSWORD => {
                self.switch_field(JianguoyunLoginField::Password);
                JianguoyunLoginOutcome::Changed
            }
            action::FOCUS_REMOTE_DIR => {
                self.switch_field(JianguoyunLoginField::RemoteDir);
                JianguoyunLoginOutcome::Changed
            }
            action::SUBMIT | kb_action::CONFIRM => {
                self.sync_committed_from_keyboard();
                match self.validate() {
                    Some(problem) => {
                        self.error_message = Some(tr(problem).to_string());
                        JianguoyunLoginOutcome::Changed
                    }
                    None => {
                        self.error_message = None;
                        self.is_saving = true;
                        JianguoyunLoginOutcome::Submit {
                            server_url: self.server_url.trim().to_string(),
                            email: self.email.trim().to_string(),
                            password: self.password.clone(),
                            remote_dir: self.remote_dir.trim().to_string(),
                        }
                    }
                }
            }
            kb_action::CAND_PREV => {
                self.keyboard.prev_cand_page();
                JianguoyunLoginOutcome::Changed
            }
            kb_action::CAND_NEXT => {
                self.keyboard.next_cand_page();
                JianguoyunLoginOutcome::Changed
            }
            id if (kb_action::CAND_BASE..kb_action::CAND_BASE + 10).contains(&id) => {
                let idx = (id - kb_action::CAND_BASE) as usize;
                if self.keyboard.pick_candidate(idx) {
                    self.sync_committed_from_keyboard();
                    JianguoyunLoginOutcome::Changed
                } else {
                    JianguoyunLoginOutcome::None
                }
            }
            kb_action::MODE_ZH => {
                self.keyboard.set_mode(InputMode::Chinese);
                self.sync_committed_from_keyboard();
                JianguoyunLoginOutcome::Changed
            }
            kb_action::MODE_EN => {
                self.keyboard.set_mode(InputMode::English);
                self.sync_committed_from_keyboard();
                JianguoyunLoginOutcome::Changed
            }
            kb_action::TOGGLE_PAGE => {
                self.keyboard.toggle_page();
                JianguoyunLoginOutcome::Changed
            }
            kb_action::SHIFT => {
                self.keyboard.press_shift();
                JianguoyunLoginOutcome::Changed
            }
            kb_action::SPACE => {
                self.keyboard.press_space();
                self.sync_committed_from_keyboard();
                JianguoyunLoginOutcome::Changed
            }
            kb_action::DELETE => {
                self.keyboard.press_delete();
                self.sync_committed_from_keyboard();
                JianguoyunLoginOutcome::Changed
            }
            id if (kb_action::KEY_BASE..kb_action::KEY_BASE + 0x1000).contains(&id) => {
                if let Some(ch) = char::from_u32(id - kb_action::KEY_BASE) {
                    self.keyboard.press_key(ch);
                    self.sync_committed_from_keyboard();
                    JianguoyunLoginOutcome::Changed
                } else {
                    JianguoyunLoginOutcome::None
                }
            }
            _ => JianguoyunLoginOutcome::None,
        }
    }
}

/// Density-scaled single-line input box widget so 2-column rows on 600x800 and
/// 758x1024 panels never wrap onto neighboring labels.
#[derive(Clone, Debug)]
struct LoginInputBoxWidget {
    text: String,
    pinyin: String,
    placeholder: String,
    secret: bool,
    focused: bool,
    height_px: i32,
    font_px: f32,
    pad_x: f32,
    radius: f32,
}

impl Widget for LoginInputBoxWidget {
    fn measure(&mut self, c: Constraints, _text: &mut crate::text::TextEngine) -> Size {
        Size::new(c.max.w, self.height_px)
    }

    fn paint(&self, rect: Rect, cx: &mut PaintCx) {
        let p = &mut cx.painter;
        p.fill_round_rect(rect, self.radius, 255);
        p.stroke_round_rect(rect, self.radius, if self.focused { 2.0 } else { 1.0 }, 0);

        let style = TextStyle::new(FamilyId(0), self.font_px).gray(INK);
        let placeholder_style = TextStyle::new(FamilyId(0), self.font_px).gray(INK_3);
        let line_h = (self.font_px * 1.25).round();
        let curr_x = rect.x as f32 + self.pad_x;
        let baseline_y = rect.y as f32 + ((rect.h as f32 - line_h) / 2.0).max(2.0);
        let avail_w = (rect.w as f32 - self.pad_x * 2.0).max(8.0);

        if self.text.is_empty() && self.pinyin.is_empty() {
            if !self.placeholder.is_empty() {
                let mut para = Paragraph::plain(&self.placeholder, placeholder_style, Align::Left);
                para.style.max_lines = Some(1);
                let layout = layout_paragraph(cx.text, &para, avail_w, &NoHyphenation);
                layout.draw(cx.text, p, curr_x, baseline_y);
            }
            if self.focused {
                p.fill_rect(
                    Rect::new(curr_x as i32, baseline_y as i32, 2, line_h as i32),
                    0,
                );
            }
            return;
        }

        let mut shown = if self.secret {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        };
        if self.focused && !self.pinyin.is_empty() {
            if !shown.is_empty() {
                shown.push(' ');
            }
            shown.push_str(&self.pinyin);
        }
        let mut para = Paragraph::plain(&shown, style, Align::Left);
        para.style.max_lines = Some(1);
        let layout = layout_paragraph(cx.text, &para, avail_w, &NoHyphenation);
        layout.draw(cx.text, p, curr_x, baseline_y);
        if self.focused {
            let cursor_x = (curr_x + layout.width + 2.0).min((rect.right() - 4) as f32);
            p.fill_rect(
                Rect::new(cursor_x as i32, baseline_y as i32, 2, line_h as i32),
                0,
            );
        }
    }
}

/// One field of the form: its label, its box and the action that focuses it.
#[allow(clippy::too_many_arguments)]
fn field_col(
    t: &mut Tree,
    s: &Styles,
    label: &str,
    placeholder: &str,
    value: &str,
    secret: bool,
    focused: bool,
    committed: &str,
    pinyin: &str,
    action: ActionId,
    trailing: Option<NodeId>,
) -> NodeId {
    let label_node = t.label(label, s.font_xs().bold());
    let height_px = s.density.px(72).max(36);
    let font_px = (s.density.px(28) as f32).max(14.0);
    let box_node = t.add(
        Node::new(Kind::Custom(Box::new(LoginInputBoxWidget {
            text: if focused {
                committed.to_string()
            } else {
                value.to_string()
            },
            pinyin: if focused {
                pinyin.to_string()
            } else {
                String::new()
            },
            placeholder: placeholder.to_string(),
            secret,
            focused,
            height_px,
            font_px,
            pad_x: s.density.px(16) as f32,
            radius: s.density.px(14) as f32,
        }))),
        [],
    );
    let mut touch_node = Node::new(Kind::MinSize(Size::new(0, 72)))
        .action(action)
        .feedback(Feedback::None);
    if trailing.is_some() {
        touch_node = touch_node.flex(1);
    }
    let touch = t.add(touch_node, [box_node]);
    let input_row = match trailing {
        Some(extra) => t.add(
            Node::row(s.density.px(8)).cross(Cross::Center),
            [touch, extra],
        ),
        None => touch,
    };
    let gap = t.gap(0, 4);
    t.add(Node::column(0).flex(1), [label_node, gap, input_row])
}

/// Builds the Nutstore setup screen's tree.
pub fn build_jianguoyun_login_screen(
    t: &mut Tree,
    s: &Styles,
    state: &JianguoyunLoginState,
    status: &StatusLine,
) -> NodeId {
    let bar = status_line(t, s, status);
    let status_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

    // Header: Back button + the title.
    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(
        tr(Msg::JianguoyunLoginTitle),
        s.font_lg().bold().line_height(1.2),
    );
    let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    let header = t.fixed(None, Some(80), Some(header));
    let header_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

    let mut form_items = Vec::new();

    // The error banner, when the form was refused.
    if let Some(err) = &state.error_message {
        let err_icon = t.icon(Icon::SquareInfo, 28, INK);
        let mut ep = Paragraph::plain(err, s.font_xs().bold().gray(INK), Align::Left);
        ep.style.max_lines = Some(1);
        let err_label = t.text(ep);
        let banner_content = t.add(Node::row(12).cross(Cross::Center), [err_icon, err_label]);
        let banner = t.add(
            Node::new(Kind::Padding(Insets::vh(8, 16)))
                .bg(CARD)
                .border(2, INK, 14),
            [banner_content],
        );
        form_items.push(banner);
        form_items.push(t.gap(0, 6));
    }

    // Row 1: Server URL + Remote folder side by side so all four fields fit
    // above the on-screen keyboard on 600x800 panels.
    let server_col = field_col(
        t,
        s,
        tr(Msg::JianguoyunServerUrl),
        tr(Msg::JianguoyunServerUrlPlaceholder),
        &state.server_url,
        false,
        state.active_field == JianguoyunLoginField::ServerUrl,
        &state.keyboard.committed,
        &state.keyboard.pinyin,
        action::FOCUS_SERVER_URL,
        None,
    );
    let remote_col = field_col(
        t,
        s,
        tr(Msg::JianguoyunRemoteDir),
        tr(Msg::JianguoyunRemoteDirPlaceholder),
        &state.remote_dir,
        false,
        state.active_field == JianguoyunLoginField::RemoteDir,
        &state.keyboard.committed,
        &state.keyboard.pinyin,
        action::FOCUS_REMOTE_DIR,
        None,
    );
    let row1 = t.add(Node::row(16).cross(Cross::Start), [server_col, remote_col]);
    form_items.push(row1);
    form_items.push(t.gap(0, 6));

    // Row 2: Account email + App password (with eye toggle).
    let email_col = field_col(
        t,
        s,
        tr(Msg::JianguoyunEmail),
        tr(Msg::JianguoyunEmailPlaceholder),
        &state.email,
        false,
        state.active_field == JianguoyunLoginField::Email,
        &state.keyboard.committed,
        &state.keyboard.pinyin,
        action::FOCUS_EMAIL,
        None,
    );
    let eye_icon = t.icon(
        if state.show_password {
            Icon::EyeOff
        } else {
            Icon::Eye
        },
        36,
        INK_2,
    );
    let eye_center = t.align(0.5, 0.5, eye_icon);
    let eye_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(s.density.px(72)),
            h: Some(s.density.px(72)),
        })
        .in_pixels()
        .action(action::TOGGLE_SHOW_PASSWORD)
        .feedback(Feedback::Invert),
        [eye_center],
    );
    let password_col = field_col(
        t,
        s,
        tr(Msg::JianguoyunAppPassword),
        tr(Msg::JianguoyunAppPasswordPlaceholder),
        &state.password,
        !state.show_password,
        state.active_field == JianguoyunLoginField::Password,
        &state.keyboard.committed,
        &state.keyboard.pinyin,
        action::FOCUS_PASSWORD,
        Some(eye_btn),
    );
    let row2 = t.add(Node::row(16).cross(Cross::Start), [email_col, password_col]);
    form_items.push(row2);
    form_items.push(t.gap(0, 6));

    // The hint on where the app password comes from.
    let mut hp = Paragraph::plain(
        tr(Msg::JianguoyunAppPasswordHint),
        s.font_xs().gray(INK_MID).line_height(1.2),
        Align::Left,
    );
    hp.style.max_lines = Some(1);
    let hint = t.text(hp);
    form_items.push(hint);
    form_items.push(t.gap(0, 6));

    // The submit button.
    let btn_text = if state.is_saving {
        tr(Msg::JianguoyunLoading)
    } else {
        tr(Msg::JianguoyunSubmit)
    };
    let btn_label = t.label(btn_text, s.font_base().bold().gray(PAPER));
    let btn_center = t.align(0.5, 0.5, btn_label);
    let submit_btn = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(72),
        })
        .bg(INK)
        .border(0, INK, 22)
        .action(action::SUBMIT)
        .feedback(Feedback::Invert),
        [btn_center],
    );
    form_items.push(submit_btn);

    if state.error_message.is_none() {
        form_items.push(t.gap(0, 4));
        let mut up = Paragraph::plain(
            tr(Msg::JianguoyunUsbHint),
            s.font_xs().gray(INK_3).line_height(1.2),
            Align::Left,
        );
        up.style.max_lines = Some(1);
        let usb = t.text(up);
        form_items.push(usb);
    }

    let form_col = t.add(Node::column(0).flex(1), form_items);
    let form_pad = t.pad(Insets::new(4, home_margin(), 4, home_margin()), form_col);

    // The keyboard at the bottom.
    let kb_node = build_keyboard(t, s, &state.keyboard, tr(Msg::JianguoyunSubmit));

    t.add(Node::column(0), [status_pad, header_pad, form_pad, kb_node])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_text(t: &Tree, id: NodeId, text: &str) -> bool {
        match &t.node(id).kind {
            Kind::Text { para, .. } => para.spans.iter().any(|span| span.text == text),
            _ => t.node(id).children.iter().any(|&c| find_text(t, c, text)),
        }
    }

    #[test]
    fn the_debug_output_keeps_the_password_out() {
        let mut state = JianguoyunLoginState::new();
        state.email = "anna@example.com".into();
        state.password = "hunter2-secret".into();

        let debug = format!("{state:?}");
        assert!(
            !debug.contains("hunter2-secret"),
            "password leaked into Debug: {debug}"
        );
        assert!(debug.contains("[REDACTED]"));

        let outcome = JianguoyunLoginOutcome::Submit {
            server_url: DEFAULT_SERVER_URL.into(),
            email: "anna@example.com".into(),
            password: "hunter2-secret".into(),
            remote_dir: String::new(),
        };
        let debug = format!("{outcome:?}");
        assert!(
            !debug.contains("hunter2-secret"),
            "password leaked into Debug: {debug}"
        );
    }

    #[test]
    fn the_form_starts_with_nutstore_own_server() {
        let state = JianguoyunLoginState::new();
        assert_eq!(state.server_url, "https://dav.jianguoyun.com/dav/");
        assert_eq!(state.active_field, JianguoyunLoginField::ServerUrl);
        assert!(state.error_message.is_none());
    }

    #[test]
    fn an_empty_field_is_named_in_the_error() {
        let mut state = JianguoyunLoginState::new();
        state.server_url.clear();
        assert_eq!(
            state.handle_action(action::SUBMIT),
            JianguoyunLoginOutcome::Changed
        );
        assert_eq!(
            state.error_message.as_deref(),
            Some(tr(Msg::JianguoyunErrServerUrl))
        );

        state.server_url = DEFAULT_SERVER_URL.to_string();
        state.email = "anna@example.com".into();
        state.handle_action(action::SUBMIT);
        assert_eq!(
            state.error_message.as_deref(),
            Some(tr(Msg::JianguoyunErrPassword))
        );

        state.email.clear();
        state.password = "app-password".into();
        state.handle_action(action::SUBMIT);
        assert_eq!(
            state.error_message.as_deref(),
            Some(tr(Msg::JianguoyunErrEmail))
        );
    }

    #[test]
    fn a_server_address_that_is_not_a_url_is_refused() {
        let mut state = JianguoyunLoginState::new();
        state.server_url = "dav.jianguoyun.com".into();
        state.email = "anna@example.com".into();
        state.password = "app-password".into();
        state.handle_action(action::SUBMIT);
        assert_eq!(
            state.error_message.as_deref(),
            Some(tr(Msg::JianguoyunErrServerUrl))
        );
    }

    #[test]
    fn a_complete_form_is_submitted() {
        let mut state = JianguoyunLoginState::new();
        state.email = "anna@example.com".into();
        state.password = "app-password".into();
        state.remote_dir = "Books".into();
        let outcome = state.handle_action(action::SUBMIT);
        assert_eq!(
            outcome,
            JianguoyunLoginOutcome::Submit {
                server_url: DEFAULT_SERVER_URL.to_string(),
                email: "anna@example.com".to_string(),
                password: "app-password".to_string(),
                remote_dir: "Books".to_string(),
            }
        );
        assert!(state.error_message.is_none());
        assert!(state.is_saving);
    }

    #[test]
    fn the_keyboard_types_into_the_active_field_only() {
        let mut state = JianguoyunLoginState::new();
        state.switch_field(JianguoyunLoginField::Email);
        state.keyboard.committed = "anna@example".into();
        state.handle_action(kb_action::KEY_BASE + 'c' as u32);
        assert_eq!(state.email, "anna@examplec");
        assert_eq!(state.server_url, DEFAULT_SERVER_URL);

        state.switch_field(JianguoyunLoginField::RemoteDir);
        assert_eq!(state.remote_dir, "");
        state.keyboard.committed = "Books".into();
        state.sync_committed_from_keyboard();
        assert_eq!(state.remote_dir, "Books");
        assert_eq!(state.email, "anna@examplec");
    }

    #[test]
    fn the_show_password_button_toggles_the_mask() {
        let mut state = JianguoyunLoginState::new();
        assert!(!state.show_password);
        state.handle_action(action::TOGGLE_SHOW_PASSWORD);
        assert!(state.show_password);
        state.handle_action(action::TOGGLE_SHOW_PASSWORD);
        assert!(!state.show_password);
    }

    #[test]
    fn the_fields_step_forward_and_back() {
        let state = JianguoyunLoginState::new();
        assert_eq!(state.active_field.next(), JianguoyunLoginField::Email);
        assert_eq!(
            JianguoyunLoginField::RemoteDir.next(),
            JianguoyunLoginField::ServerUrl
        );
        assert_eq!(
            JianguoyunLoginField::ServerUrl.previous(),
            JianguoyunLoginField::RemoteDir
        );
    }

    #[test]
    fn the_screen_has_the_four_fields_and_the_submit_button() {
        let mut tree = Tree::new();
        let styles = Styles::new(FamilyId(0));
        let status = StatusLine {
            clock: "12:00",
            battery: None,
            charging: false,
        };
        let root = build_jianguoyun_login_screen(
            &mut tree,
            &styles,
            &JianguoyunLoginState::new(),
            &status,
        );
        tree.set_root(root);

        for act in [
            action::FOCUS_SERVER_URL,
            action::FOCUS_EMAIL,
            action::FOCUS_PASSWORD,
            action::FOCUS_REMOTE_DIR,
            action::SUBMIT,
            action::TOGGLE_SHOW_PASSWORD,
            action::BACK,
        ] {
            let found = (0..tree.len())
                .map(|i| NodeId(i as u32))
                .any(|n| tree.node(n).action == Some(act));
            assert!(found, "action {act} is missing");
        }
        assert!(find_text(&tree, root, tr(Msg::JianguoyunAppPasswordHint)));
    }
}
