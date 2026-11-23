// SPDX-License-Identifier: AGPL-3.0-or-later
//! The WeRead sign-in screen: a QR code, its status, and the way back.
//!
//! The app owns the login state machine (ticket, long poll, exchange); this
//! screen shows the code the app handed it and the words for the state. A
//! 402 from the poll reaches the app, which makes a new code and hands it
//! over; the screen itself never talks to the network.
//!
//! The code is drawn from `qrcode`'s module matrix into one grayscale bitmap
//! at a whole number of pixels per module: e-ink panels have no gray for
//! antialiasing, and a blurred module edge costs scannability.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::text::TextEngine;
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_3, PAPER};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use crate::widgets::GrayImage;
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// Leave the login screen.
    pub const BACK: ActionId = 1;
    /// Ask the app for a new QR code.
    pub const REFRESH: ActionId = 2;
    /// OTP keypad: `WEB_DIGIT + n` types the digit `n`.
    pub const WEB_DIGIT: ActionId = 1_000_000;
    /// Remove the last OTP digit.
    pub const WEB_DELETE: ActionId = 1_000_010;
    /// Submit the four OTP digits.
    pub const WEB_CONFIRM: ActionId = 1_000_011;
}

/// Which of the two sign-in stages the screen shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WereadLoginStage {
    /// The E-Ink QR code (`vid`/`accessToken`).
    #[default]
    Eink,
    /// The Web QR code (`wr_vid`/`wr_skey`) and its OTP.
    Web,
}

/// The visible states of the Web sign-in stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WereadWebPhase {
    #[default]
    Fetching,
    Waiting,
    NeedOtp,
    OtpMismatch,
    Expired,
    Failed,
    Confirmed,
}

/// The number of modules in the code's quiet zone (the QR standard's four).
pub const QUIET_ZONE: usize = 4;

/// What the code is waiting for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WereadLoginPhase {
    /// The code is on the screen; nobody scanned it yet.
    #[default]
    Waiting,
    /// 404: the phone scanned the code.
    Scanned,
    /// 405: the phone confirmed; the app is exchanging the code.
    Confirmed,
    /// 402: the code expired; the app is making another one.
    Expired,
    /// 403: the reader declined on the phone.
    Declined,
    /// A failure with its own message.
    Failed,
}

/// A rendered sign-in code and the words that go with it.
pub struct WereadLoginState {
    /// Which sign-in stage this screen belongs to.
    pub stage: WereadLoginStage,
    /// The Web stage's state.
    pub web_phase: WereadWebPhase,
    /// The four OTP digits typed so far (never logged).
    pub otp: String,
    /// The code bitmap; `None` while a new one is being made.
    qr: Option<GrayImage>,
    /// The confirmation URL, kept to redraw the code after a mode change.
    /// It is a one-time sign-in token: `Debug` never prints it.
    qr_url: Option<String>,
    /// Which request the fields belong to: a late reply is dropped.
    pub generation: u64,
    pub phase: WereadLoginPhase,
    /// A failure in the reader's language, shown above the hint.
    pub message: Option<String>,
}

impl Default for WereadLoginState {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for WereadLoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WereadLoginState")
            .field("has_qr", &self.qr.is_some())
            .field("qr_url", &self.qr_url.as_ref().map(|_| "[REDACTED]"))
            .field("generation", &self.generation)
            .field("stage", &self.stage)
            .field("phase", &self.phase)
            .field("web_phase", &self.web_phase)
            .field("otp_len", &self.otp.len())
            .field("message", &self.message)
            .finish()
    }
}

impl WereadLoginState {
    pub fn new() -> Self {
        Self {
            stage: WereadLoginStage::Eink,
            web_phase: WereadWebPhase::Fetching,
            otp: String::new(),
            qr: None,
            qr_url: None,
            generation: 0,
            phase: WereadLoginPhase::Waiting,
            message: None,
        }
    }

    /// Start a new attempt: a new generation, no code yet.
    pub fn restart(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.stage = WereadLoginStage::Eink;
        self.web_phase = WereadWebPhase::Fetching;
        self.otp.clear();
        self.qr = None;
        self.qr_url = None;
        self.phase = WereadLoginPhase::Waiting;
        self.message = None;
        self.generation
    }

    /// Move to the Web stage of request `generation`.
    pub fn begin_web(&mut self, generation: u64) {
        self.generation = generation;
        self.stage = WereadLoginStage::Web;
        self.web_phase = WereadWebPhase::Fetching;
        self.otp.clear();
        self.qr = None;
        self.qr_url = None;
        self.phase = WereadLoginPhase::Waiting;
        self.message = None;
    }

    /// The Web stage's code; older replies are ignored.
    pub fn set_web_code(&mut self, generation: u64, url: &str) -> bool {
        if generation != self.generation {
            return false;
        }
        self.qr = render_qr(url, QR_PIXELS);
        self.qr_url = Some(url.to_string());
        self.web_phase = WereadWebPhase::Waiting;
        self.message = None;
        self.qr.is_some()
    }

    /// Update the Web stage's state; older replies are ignored.
    pub fn set_web_phase(&mut self, generation: u64, phase: WereadWebPhase, otp: &str) -> bool {
        if generation != self.generation {
            return false;
        }
        self.web_phase = phase;
        self.otp = otp.chars().filter(char::is_ascii_digit).take(4).collect();
        if !matches!(phase, WereadWebPhase::Expired | WereadWebPhase::Failed) {
            self.message = None;
        }
        true
    }

    /// Append one OTP digit, at most four.
    pub fn push_otp(&mut self, digit: char) -> bool {
        if self.otp.len() >= 4 || !digit.is_ascii_digit() {
            return false;
        }
        self.otp.push(digit);
        true
    }

    /// Remove the last OTP digit.
    pub fn delete_otp(&mut self) -> bool {
        self.otp.pop().is_some()
    }

    /// Clear the OTP after a rejected code.
    pub fn clear_otp(&mut self) {
        self.otp.clear();
    }

    /// Whether the OTP step is the state the screen should show.
    pub fn asks_for_otp(&self) -> bool {
        self.stage == WereadLoginStage::Web
            && matches!(
                self.web_phase,
                WereadWebPhase::NeedOtp | WereadWebPhase::OtpMismatch
            )
    }

    /// The Web stage's status line.
    pub fn web_phase_label(&self) -> &'static str {
        match self.web_phase {
            WereadWebPhase::Fetching => tr(Msg::WereadWebStatusFetching),
            WereadWebPhase::Waiting => tr(Msg::WereadWebStatusWaiting),
            WereadWebPhase::NeedOtp => tr(Msg::WereadWebStatusNeedOtp),
            WereadWebPhase::OtpMismatch => tr(Msg::WereadWebStatusOtpBad),
            WereadWebPhase::Expired => tr(Msg::WereadWebStatusExpired),
            WereadWebPhase::Failed => tr(Msg::WereadWebStatusFailed),
            WereadWebPhase::Confirmed => tr(Msg::WereadWebStatusConfirmed),
        }
    }

    /// The stage caption above the hint.
    pub fn stage_label(&self) -> &'static str {
        match self.stage {
            WereadLoginStage::Eink => tr(Msg::WereadWebStageEink),
            WereadLoginStage::Web => tr(Msg::WereadWebStageWeb),
        }
    }

    /// Take the code of request `generation`; older replies are ignored.
    pub fn set_code(&mut self, generation: u64, url: &str) -> bool {
        if generation != self.generation {
            return false;
        }
        self.qr = render_qr(url, QR_PIXELS);
        self.qr_url = Some(url.to_string());
        self.phase = WereadLoginPhase::Waiting;
        self.message = None;
        self.qr.is_some()
    }

    /// Change the phase of request `generation`; older replies are ignored.
    pub fn set_phase(&mut self, generation: u64, phase: WereadLoginPhase) -> bool {
        if generation != self.generation {
            return false;
        }
        self.phase = phase;
        if !matches!(
            phase,
            WereadLoginPhase::Expired | WereadLoginPhase::Declined | WereadLoginPhase::Failed
        ) {
            self.message = None;
        }
        true
    }

    /// Show a failure of request `generation`.
    pub fn fail(&mut self, generation: u64, message: String) -> bool {
        if generation != self.generation {
            return false;
        }
        self.phase = WereadLoginPhase::Failed;
        self.qr = None;
        self.qr_url = None;
        self.message = Some(message);
        true
    }

    /// The code, for tests and the preview.
    pub fn qr(&self) -> Option<&GrayImage> {
        self.qr.as_ref()
    }

    /// The confirmation URL; never part of a log line.
    pub fn qr_url(&self) -> Option<&str> {
        self.qr_url.as_deref()
    }

    /// What the screen says under the code.
    pub fn phase_label(&self) -> &'static str {
        match self.phase {
            WereadLoginPhase::Waiting => tr(Msg::WereadStatusWaiting),
            WereadLoginPhase::Scanned => tr(Msg::WereadStatusScanned),
            WereadLoginPhase::Confirmed => tr(Msg::WereadStatusConfirmed),
            WereadLoginPhase::Expired => tr(Msg::WereadStatusExpired),
            WereadLoginPhase::Declined => tr(Msg::WereadStatusDeclined),
            WereadLoginPhase::Failed => tr(Msg::WereadStatusFailed),
        }
    }
}

/// The width of the code's square, in reference pixels.
pub const QR_PIXELS: i32 = 400;

/// Render a QR code of `url` into a square grayscale bitmap of at most
/// `target` pixels, one whole number of pixels per module.
///
/// Returns `None` when the data does not fit a QR code at all; the app then
/// shows the failure instead of a code that cannot be scanned.
pub fn render_qr(url: &str, target: i32) -> Option<GrayImage> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let modules = code.width();
    let side = modules + QUIET_ZONE * 2;
    let scale = (target.max(1) as usize / side).max(1);
    let pixels_side = side * scale;
    let mut pixels = vec![255u8; pixels_side * pixels_side];
    let colors = code.to_colors();
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    let row = (y + QUIET_ZONE) * scale + dy;
                    for dx in 0..scale {
                        let column = (x + QUIET_ZONE) * scale + dx;
                        pixels[row * pixels_side + column] = 0;
                    }
                }
            }
        }
    }
    Some(GrayImage::new(
        pixels_side as u32,
        pixels_side as u32,
        pixels,
    ))
}

/// The code as a leaf widget: a white card, then the bitmap at its own size.
struct QrCode {
    image: GrayImage,
}

impl Widget for QrCode {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(self.image.width as i32, self.image.height as i32))
    }

    fn paint(&self, rect: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        p.fill_rect(rect, PAPER);
        let x = rect.x + (rect.w - self.image.width as i32) / 2;
        let y = rect.y + (rect.h - self.image.height as i32) / 2;
        p.blit_gray(
            x,
            y,
            self.image.width,
            self.image.height,
            self.image.width as usize,
            &self.image.pixels,
        );
    }
}

/// What a tap on the login screen asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WereadLoginOutcome {
    #[default]
    None,
    Changed,
    Back,
    Refresh,
}

/// Builds the WeRead login screen.
pub fn build_weread_login_screen(
    t: &mut Tree,
    s: &Styles,
    state: &WereadLoginState,
    status: &StatusLine,
) -> (NodeId, WereadLoginUi) {
    let bar = status_line(t, s, status);
    let status_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(
        tr(Msg::WereadLoginTitle),
        s.font_lg().bold().line_height(1.2),
    );
    let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    let header = t.fixed(None, Some(104), Some(header));
    let header_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

    let mut column = Vec::new();
    let stage = t.label(state.stage_label(), s.font_xs().gray(INK_2));
    column.push(t.pad(Insets::new(4, home_margin(), 0, home_margin()), stage));
    let hint_text = match state.stage {
        WereadLoginStage::Eink => tr(Msg::WereadLoginHint),
        WereadLoginStage::Web => tr(Msg::WereadWebLoginHint),
    };
    let hint = t.label(hint_text, s.font_sm().line_height(1.35));
    column.push(t.pad(Insets::new(6, home_margin(), 0, home_margin()), hint));
    column.push(t.gap(0, 16));

    // The code on its own white card; a frame keeps black modules off the
    // page's edge and gives the scanner the contrast it wants.
    let code: NodeId = if state.asks_for_otp() {
        build_otp_card(t, s, state)
    } else if let Some(image) = state.qr() {
        let widget = t.custom(QrCode {
            image: image.clone(),
        });
        let code = t.fixed(Some(QR_PIXELS), Some(QR_PIXELS), Some(widget));
        let pad = t.pad(Insets::all(20), code);
        t.add(Node::column(0).bg(PAPER).border(2, INK, 12), [pad])
    } else {
        // No code to show (a failure or a new attempt): a compact card
        // instead of the code's square, so the screen does not look empty.
        let icon = t.icon(Icon::SquareInfo, 64, INK_3);
        let centered = t.align(0.5, 0.5, icon);
        let box_ = t.fixed(Some(QR_PIXELS + 40), Some(220), Some(centered));
        let card = t.add(Node::column(0).bg(CARD).border(2, INK_3, 12), [box_]);
        t.fixed(Some(QR_PIXELS + 40), Some(220), Some(card))
    };
    column.push(code);
    column.push(t.gap(0, 16));

    let phase_text = match state.stage {
        WereadLoginStage::Eink => state.phase_label(),
        WereadLoginStage::Web => state.web_phase_label(),
    };
    let phase = t.label(phase_text, s.font_base().bold().line_height(1.3));
    column.push(phase);

    if let Some(message) = &state.message {
        column.push(t.gap(0, 12));
        let icon = t.icon(Icon::SquareInfo, 36, INK);
        let text = t.label(message, s.font_sm().bold().gray(INK));
        let row = t.add(Node::row(16).cross(Cross::Center), [icon, text]);
        let banner = t.add(
            Node::new(Kind::Padding(Insets::vh(14, 24)))
                .bg(CARD)
                .border(2, INK, 16),
            [row],
        );
        column.push(t.fixed(Some(QR_PIXELS + 120), None, Some(banner)));
    }

    if state.stage == WereadLoginStage::Eink {
        column.push(t.gap(0, 12));
        let scan_hint = t.label(tr(Msg::WereadLoginScanHint), s.font_xs().gray(INK_2));
        column.push(scan_hint);
    }
    column.push(t.gap(0, 20));

    let refresh_label = t.label(tr(Msg::WereadRefreshQr), s.font_link().gray(INK_2));
    let refresh_center = t.align(0.5, 0.5, refresh_label);
    let refresh = t.add(
        Node::new(Kind::Fixed {
            w: Some(QR_PIXELS + 40),
            h: Some(88),
        })
        .action(action::REFRESH)
        .feedback(Feedback::Invert),
        [refresh_center],
    );
    column.push(refresh);

    let body = t.add(Node::column(0).cross(Cross::Center), column);
    let body = t.pad(Insets::new(0, home_margin(), 0, home_margin()), body);
    let body = t.add(Node::column(0).flex(1), [body]);

    let root = t.add(Node::column(0), [status_pad, header_pad, body]);
    (root, WereadLoginUi)
}

/// The OTP step: four digit cells and a compact numeric keypad.
fn build_otp_card(t: &mut Tree, s: &Styles, state: &WereadLoginState) -> NodeId {
    let mut cells = Vec::new();
    for index in 0..4 {
        let text = state
            .otp
            .chars()
            .nth(index)
            .map(|digit| digit.to_string())
            .unwrap_or_else(|| "·".to_string());
        let label = t.label(&text, s.font_lg().bold().line_height(1.0));
        let center = t.align(0.5, 0.5, label);
        let cell = t.add(
            Node::new(Kind::Fixed {
                w: Some(84),
                h: Some(104),
            })
            .bg(PAPER)
            .border(2, INK, 12),
            [center],
        );
        cells.push(cell);
    }
    let row = t.add(Node::row(18).cross(Cross::Center), cells);

    let mut key_rows = Vec::new();
    for digits in [['1', '2', '3'], ['4', '5', '6'], ['7', '8', '9']] {
        let keys: Vec<NodeId> = digits
            .iter()
            .map(|&digit| {
                otp_key(
                    t,
                    s,
                    &digit.to_string(),
                    action::WEB_DIGIT + u32::from(digit as u8 - b'0'),
                    false,
                    116,
                )
            })
            .collect();
        key_rows.push(t.add(Node::row(8).cross(Cross::Center), keys));
    }
    let delete = otp_key(
        t,
        s,
        tr(Msg::WereadWebOtpDelete),
        action::WEB_DELETE,
        true,
        140,
    );
    let zero = otp_key(t, s, "0", action::WEB_DIGIT, false, 96);
    let confirm = otp_key(
        t,
        s,
        tr(Msg::WereadWebOtpConfirm),
        action::WEB_CONFIRM,
        true,
        140,
    );
    key_rows.push(t.add(Node::row(6).cross(Cross::Center), [delete, zero, confirm]));

    let keypad = t.add(Node::column(12).cross(Cross::Center), key_rows);
    let card = t.add(Node::column(16).cross(Cross::Center), [row, keypad]);
    let pad = t.pad(Insets::vh(18, 20), card);
    let width = t.fixed(Some(QR_PIXELS + 40), None, Some(pad));
    t.add(Node::column(0).bg(CARD).border(2, INK_3, 16), [width])
}

fn otp_key(
    t: &mut Tree,
    s: &Styles,
    text: &str,
    id: ActionId,
    compact: bool,
    width: i32,
) -> NodeId {
    let font = if compact {
        s.font_xs().bold()
    } else {
        s.font_base().bold()
    };
    let label = t.label(text, font.line_height(1.0));
    let center = t.align(0.5, 0.5, label);
    t.add(
        Node::new(Kind::Fixed {
            w: Some(width),
            h: Some(68),
        })
        .bg(PAPER)
        .border(2, INK, 12)
        .action(id)
        .feedback(Feedback::Invert),
        [center],
    )
}

/// Handles to the login screen's nodes.
pub struct WereadLoginUi;

impl WereadLoginUi {
    /// Applies a tap. `Changed` asks the app to rebuild the screen.
    pub fn handle(&mut self, _t: &mut Tree, id: ActionId) -> WereadLoginOutcome {
        match id {
            action::BACK => WereadLoginOutcome::Back,
            action::REFRESH => WereadLoginOutcome::Refresh,
            action::WEB_DELETE | action::WEB_CONFIRM => WereadLoginOutcome::Changed,
            id if (action::WEB_DIGIT..action::WEB_DIGIT + 10).contains(&id) => {
                WereadLoginOutcome::Changed
            }
            _ => WereadLoginOutcome::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
    use crate::text::{FamilyId, Fonts, TextEngine};
    use crate::tree::NodeId;
    use crate::ui::Ui;
    use aristotle_i18n::with_language;

    fn find_action(tree: &Tree, id: ActionId) -> Option<NodeId> {
        (0..tree.len())
            .map(|n| NodeId(n as u32))
            .find(|n| tree.node(*n).action == Some(id))
    }

    #[test]
    fn a_code_has_a_quiet_zone_and_whole_pixels_per_module() {
        let image = render_qr(
            "https://open.weixin.qq.com/connect/confirm?uuid=fixture",
            400,
        )
        .expect("code");
        assert_eq!(image.width, image.height);
        // One pixel per module at least, and the quiet zone is white.
        let side = image.width as usize;
        for i in 0..side {
            assert_eq!(image.pixels[i], 255, "top quiet zone");
            assert_eq!(image.pixels[(side - 1) * side + i], 255, "bottom");
        }
        // A QR code is not blank: some modules are black.
        assert!(image.pixels.contains(&0));
        // The four-module quiet zone plus the matrix fit the requested box.
        assert!(image.width as i32 <= 400);
    }

    #[test]
    fn a_new_attempt_ignores_the_old_reply() {
        let mut state = WereadLoginState::new();
        let generation = state.restart();
        assert!(state.set_code(
            generation,
            "https://open.weixin.qq.com/connect/confirm?uuid=a"
        ));
        assert!(state.qr().is_some());
        // A reply of the previous request must not replace the new code.
        assert!(!state.set_code(generation - 1, "https://example.com/old"));
        assert!(!state.set_phase(generation - 1, WereadLoginPhase::Scanned));
        assert_eq!(state.phase, WereadLoginPhase::Waiting);
        assert!(state.set_phase(generation, WereadLoginPhase::Scanned));
        assert_eq!(state.phase, WereadLoginPhase::Scanned);
    }

    #[test]
    fn expiration_clears_the_word_but_keeps_the_code_until_the_next() {
        let mut state = WereadLoginState::new();
        let generation = state.restart();
        state.set_code(
            generation,
            "https://open.weixin.qq.com/connect/confirm?uuid=b",
        );
        state.set_phase(generation, WereadLoginPhase::Expired);
        assert_eq!(state.phase, WereadLoginPhase::Expired);
        assert!(state.qr().is_some());
    }

    #[test]
    fn the_url_never_reaches_debug_output() {
        let mut state = WereadLoginState::new();
        let generation = state.restart();
        let url = "https://open.weixin.qq.com/connect/confirm?uuid=secret-uuid";
        state.set_code(generation, url);
        let debug = format!("{state:?}");
        assert!(!debug.contains("secret-uuid"), "leaked: {debug}");
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn the_screen_offers_a_new_code_and_a_way_back() {
        with_language("en", || {
            let mut state = WereadLoginState::new();
            let generation = state.restart();
            state.set_code(
                generation,
                "https://open.weixin.qq.com/connect/confirm?uuid=c",
            );
            let text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let status = StatusLine {
                clock: "12:00",
                battery: Some(0.5),
                charging: false,
            };
            let mut tree = Tree::new();
            let (root, _) = build_weread_login_screen(&mut tree, &styles, &state, &status);
            tree.set_root(root);
            let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
            ui.layout();
            assert!(find_action(&ui.tree, action::BACK).is_some());
            assert!(find_action(&ui.tree, action::REFRESH).is_some());
            assert_eq!(state.phase_label(), "Waiting for the scan…");
        });
    }

    #[test]
    fn the_web_stage_has_its_code_its_otp_keypad_and_its_own_words() {
        with_language("en", || {
            let mut state = WereadLoginState::new();
            state.begin_web(5);
            assert_eq!(state.stage, WereadLoginStage::Web);
            assert!(state.set_web_code(5, "https://weread.qq.com/web/confirm?uid=sample-uid"));
            assert_eq!(state.stage_label(), "Step 2 of 2 · Progress sync");
            assert_eq!(state.web_phase_label(), "Scan the code with WeChat…");
            assert!(state.set_web_phase(5, WereadWebPhase::NeedOtp, "12"));
            assert_eq!(state.otp, "12");
            assert!(state.asks_for_otp());
            assert_eq!(
                state.web_phase_label(),
                "Enter the four-digit code shown on your phone."
            );
            // A stale reply cannot replace the current stage.
            assert!(!state.set_web_phase(4, WereadWebPhase::Expired, ""));

            let text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let status = StatusLine {
                clock: "12:00",
                battery: Some(0.5),
                charging: false,
            };
            let mut tree = Tree::new();
            let (root, _) = build_weread_login_screen(&mut tree, &styles, &state, &status);
            tree.set_root(root);
            let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
            ui.layout();
            assert!(find_action(&ui.tree, action::BACK).is_some());
            assert!(find_action(&ui.tree, action::REFRESH).is_some());
            for digit in ['1', '2', '9'] {
                assert!(
                    find_action(&ui.tree, action::WEB_DIGIT + u32::from(digit as u8 - b'0'))
                        .is_some(),
                    "{digit}"
                );
            }
            assert!(find_action(&ui.tree, action::WEB_DIGIT).is_some(), "zero");
            assert!(find_action(&ui.tree, action::WEB_DELETE).is_some());
            assert!(find_action(&ui.tree, action::WEB_CONFIRM).is_some());
        });
    }

    #[test]
    fn otp_keypad_emits_numeric_digit_actions_accepted_by_handle() {
        with_language("en", || {
            let mut state = WereadLoginState::new();
            state.begin_web(3);
            assert!(state.set_web_phase(3, WereadWebPhase::NeedOtp, ""));
            let text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let status = StatusLine {
                clock: "12:00",
                battery: Some(0.5),
                charging: false,
            };
            let mut tree = Tree::new();
            let (root, mut screen_ui) =
                build_weread_login_screen(&mut tree, &styles, &state, &status);
            tree.set_root(root);
            let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
            ui.layout();

            for digit in '0'..='9' {
                let expected_action = action::WEB_DIGIT + u32::from(digit as u8 - b'0');
                assert!(
                    find_action(&ui.tree, expected_action).is_some(),
                    "missing keypad action for digit {digit}"
                );
                assert_eq!(
                    screen_ui.handle(&mut ui.tree, expected_action),
                    WereadLoginOutcome::Changed,
                    "handle must accept digit {digit}"
                );
            }
            // Guard against ASCII char-code regression ('1' as u32 == 49).
            assert!(find_action(&ui.tree, action::WEB_DIGIT + '1' as u32).is_none());
            assert_eq!(
                screen_ui.handle(&mut ui.tree, action::WEB_DIGIT + '1' as u32),
                WereadLoginOutcome::None
            );
        });
    }

    #[test]
    fn otp_digits_are_bounded_and_never_in_debug() {
        let mut state = WereadLoginState::new();
        state.begin_web(2);
        state.set_web_code(2, "https://weread.qq.com/web/confirm?uid=secret-uid");
        state.set_web_phase(2, WereadWebPhase::NeedOtp, "");
        assert!(state.push_otp('1'));
        assert!(state.push_otp('2'));
        assert!(state.push_otp('3'));
        assert!(state.push_otp('4'));
        assert!(!state.push_otp('5'));
        assert_eq!(state.otp, "1234");
        assert!(state.delete_otp());
        let debug = format!("{state:?}");
        assert!(!debug.contains("secret-uid"), "{debug}");
        assert!(!debug.contains("1234"), "{debug}");
        assert!(debug.contains("otp_len"));
    }

    #[test]
    fn a_failed_code_shows_its_message() {
        let mut state = WereadLoginState::new();
        let generation = state.restart();
        assert!(state.fail(generation, "Network unavailable".to_string()));
        assert_eq!(state.phase, WereadLoginPhase::Failed);
        assert_eq!(state.message.as_deref(), Some("Network unavailable"));
        assert!(state.qr().is_none());
    }
}
