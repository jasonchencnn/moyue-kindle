// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dedicated TTS (audiobook) settings screen: API provider/endpoint/key
//! configuration and voice/model/speed configuration, with inline on-screen
//! keyboard editing.

use super::library::{status_line, StatusLine};
use super::segmented;
use super::weread_login::render_qr;
use crate::geom::{Insets, Rect, Size, MIN_TARGET};
use crate::icons::Icon;
use crate::keyboard::{
    action as kb_action, build_input_box_secret, build_keyboard, to_simplified_str, InputMode,
    KeyboardState,
};
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER, RULE};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use crate::widgets::{GrayImage, Toggle};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    /// `PROVIDER_BASE + i` selects provider preset `0..=3`:
    /// 0 = 硅基流动 (CosyVoice2), 1 = OpenAI, 2 = 火山豆包, 3 = MiniMax.
    pub const PROVIDER_BASE: ActionId = 10;
    pub const EDIT_ENDPOINT: ActionId = 20;
    pub const EDIT_API_KEY: ActionId = 21;
    pub const EDIT_APP_ID: ActionId = 22;
    pub const EDIT_MODEL: ActionId = 23;
    pub const EDIT_VOICE: ActionId = 24;
    pub const VOICE_PREV: ActionId = 30;
    pub const VOICE_NEXT: ActionId = 31;
    pub const SPEED_DOWN: ActionId = 32;
    pub const SPEED_UP: ActionId = 33;
    pub const TOGGLE_SIMULATE: ActionId = 34;
    pub const FIELD_SAVE: ActionId = 40;
    pub const FIELD_CANCEL: ActionId = 41;
    /// Open LAN QR-code scan-to-input panel.
    pub const SCAN_QR: ActionId = 50;
    /// Close LAN QR-code panel.
    pub const QR_CLOSE: ActionId = 51;
}

/// Which text field is currently open in the inline keyboard editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditField {
    Endpoint,
    ApiKey,
    AppId,
    Model,
    Voice,
}

impl EditField {
    pub fn title(self) -> &'static str {
        match self {
            EditField::Endpoint => "编辑 API 地址",
            EditField::ApiKey => "编辑 API Key / Token",
            EditField::AppId => "编辑火山引擎 App ID",
            EditField::Model => "编辑模型名称",
            EditField::Voice => "编辑音色 Voice ID",
        }
    }

    pub fn is_secret(self) -> bool {
        matches!(self, EditField::ApiKey)
    }
}

/// Inline keyboard editor state for a single TTS configuration field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldEditor {
    pub field: EditField,
    pub keyboard: KeyboardState,
}

impl FieldEditor {
    pub fn new(field: EditField, initial: &str) -> Self {
        let mut keyboard = KeyboardState::new();
        keyboard.set_mode(InputMode::English);
        keyboard.committed = initial.to_string();
        Self { field, keyboard }
    }

    pub fn value(&self) -> String {
        let mut out = self.keyboard.committed.clone();
        if !self.keyboard.pinyin.is_empty() {
            if self.keyboard.mode == InputMode::Chinese {
                out.push_str(&to_simplified_str(&self.keyboard.pinyin));
            } else {
                out.push_str(&self.keyboard.pinyin);
            }
        }
        out.trim().to_string()
    }

    /// Process a keyboard action. Returns `true` if the editor state changed.
    pub fn handle_keyboard(&mut self, id: ActionId) -> bool {
        match id {
            kb_action::CAND_PREV => {
                self.keyboard.prev_cand_page();
                true
            }
            kb_action::CAND_NEXT => {
                self.keyboard.next_cand_page();
                true
            }
            id if (kb_action::CAND_BASE..kb_action::CAND_BASE + 10).contains(&id) => self
                .keyboard
                .pick_candidate((id - kb_action::CAND_BASE) as usize),
            kb_action::MODE_ZH => {
                self.keyboard.set_mode(InputMode::Chinese);
                true
            }
            kb_action::MODE_EN => {
                self.keyboard.set_mode(InputMode::English);
                true
            }
            kb_action::TOGGLE_PAGE => {
                self.keyboard.toggle_page();
                true
            }
            kb_action::SHIFT => {
                self.keyboard.press_shift();
                true
            }
            kb_action::SPACE => {
                self.keyboard.press_space();
                true
            }
            kb_action::DELETE => {
                self.keyboard.press_delete();
                true
            }
            id if (kb_action::KEY_BASE..kb_action::KEY_BASE + 0x1000).contains(&id) => {
                if let Some(ch) = char::from_u32(id - kb_action::KEY_BASE) {
                    self.keyboard.press_key(ch);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

/// View model for the TTS settings screen.
pub struct TtsSettingsView<'a> {
    pub screen: Size,
    pub status: StatusLine<'a>,
    /// Selected provider index (`0`=硅基流动, `1`=OpenAI, `2`=火山豆包, `3`=MiniMax).
    pub provider_index: usize,
    pub endpoint: &'a str,
    pub api_key_summary: &'a str,
    pub app_id: &'a str,
    pub show_app_id: bool,
    pub model: &'a str,
    pub voice: &'a str,
    pub speed: f32,
    pub simulate_audio: bool,
    pub message: Option<&'a str>,
    pub editor: Option<&'a FieldEditor>,
    /// When `Some(url)`, displays the LAN QR-code scan-to-input panel.
    pub qr_url: Option<&'a str>,
}

/// Node handles returned after building the TTS settings screen.
#[derive(Clone, Copy, Debug)]
pub struct TtsSettingsUi {
    pub loading: LoadingNodes,
}

impl TtsSettingsUi {
    pub fn build(
        _text: &mut TextEngine,
        s: &Styles,
        v: &TtsSettingsView<'_>,
    ) -> (Tree, TtsSettingsUi) {
        let mut t = Tree::new();
        let m = home_margin();
        let bar = status_line(&mut t, s, &v.status);
        let status = t.pad(Insets::new(0, m, 0, m), bar.root);

        // Header: back button + title
        let back_ic = t.icon(Icon::ChevronLeft, 28, INK);
        let back_lbl = t.label("返回", s.font_xs().gray(INK));
        let back_g = t.gap(8, 1);
        let back_row = t.add(
            Node::row(0).cross(Cross::Center),
            [back_ic, back_g, back_lbl],
        );
        let back_pad = t.pad(Insets::vh(0, 14), back_row);
        let back_btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(MIN_TARGET),
            })
            .action(action::BACK)
            .feedback(Feedback::Invert),
            [back_pad],
        );
        let title_lbl = t.label("听书设置", s.font_lg().bold().gray(INK));
        let title_align = t.align(0.5, 0.5, title_lbl);
        let title_col = t.add(Node::column(0).flex(1), [title_align]);
        let right_spacer = t.gap(88, 1);
        let header_row = t.add(
            Node::row(0).cross(Cross::Center),
            [back_btn, title_col, right_spacer],
        );
        let header_fixed = t.fixed(None, Some(96), Some(header_row));
        let header_pad = t.pad(Insets::vh(4, m), header_fixed);
        let header_rule = t.rule(1, RULE);

        let body = if let Some(url) = v.qr_url {
            build_qr_body(&mut t, s, url, v.editor.map(|e| e.field.title()), m)
        } else if let Some(ed) = v.editor {
            build_editor_body(&mut t, s, ed, m)
        } else {
            build_main_body(&mut t, s, v, m)
        };

        let root = t.add(
            Node::column(0).bg(PAPER),
            [status, header_pad, header_rule, body],
        );
        t.set_root(root);
        (
            t,
            TtsSettingsUi {
                loading: bar.loading,
            },
        )
    }
}

fn build_main_body(t: &mut Tree, s: &Styles, v: &TtsSettingsView<'_>, m: i32) -> NodeId {
    let mut children: Vec<NodeId> = Vec::new();

    if let Some(msg) = v.message {
        let p = Paragraph::plain(msg.to_string(), s.font_xs().gray(INK_MID), Align::Left);
        let msg_node = t.text(p);
        let msg_pad = t.pad(Insets::vh(8, 16), msg_node);
        let msg_card = t.add(Node::column(0).bg(CARD).border(1, RULE, 12), [msg_pad]);
        children.push(msg_card);
        children.push(t.gap(0, 12));
    }

    // Quick LAN QR-code configuration banner
    let qr_btn = pill_btn(
        t,
        s,
        "手机扫码填写 API 配置（局域网）",
        action::SCAN_QR,
        true,
    );
    children.push(qr_btn);
    children.push(t.gap(0, 14));

    // Section 1: API 配置
    children.push(t.label("API 配置（语音大模型）", s.font_sm().bold().gray(INK)));
    children.push(t.gap(0, 10));

    let provider_labels = ["硅基流动", "OpenAI", "火山豆包", "MiniMax"];
    let cells: Vec<NodeId> = provider_labels
        .iter()
        .map(|lbl| t.label(lbl, s.font_xs().gray(INK)))
        .collect();
    let provider_seg = segmented(t, cells, v.provider_index.min(3), action::PROVIDER_BASE);
    children.push(provider_seg);
    children.push(t.gap(0, 12));

    children.push(field_row(
        t,
        s,
        "API 地址",
        short_middle(v.endpoint, 34),
        action::EDIT_ENDPOINT,
    ));
    children.push(t.gap(0, 10));
    children.push(field_row(
        t,
        s,
        "API Key / Token",
        v.api_key_summary.to_string(),
        action::EDIT_API_KEY,
    ));
    if v.show_app_id {
        children.push(t.gap(0, 10));
        let app_val = if v.app_id.is_empty() {
            "未设置（点击输入）".to_string()
        } else {
            short_middle(v.app_id, 28)
        };
        children.push(field_row(t, s, "火山 App ID", app_val, action::EDIT_APP_ID));
    }

    children.push(t.gap(0, 10));
    children.push(toggle_row(
        t,
        s,
        "无耳机模拟模式",
        "不接蓝牙耳机，按语速模拟播放与锁屏保活",
        v.simulate_audio,
        action::TOGGLE_SIMULATE,
    ));

    // Section 2: 声音配置
    children.push(t.gap(0, 20));
    children.push(t.label("声音配置", s.font_sm().bold().gray(INK)));
    children.push(t.gap(0, 10));

    children.push(field_row(
        t,
        s,
        "语音模型",
        short_middle(v.model, 32),
        action::EDIT_MODEL,
    ));
    children.push(t.gap(0, 10));

    // Voice row with quick prev/next buttons + tap to edit
    children.push(voice_row(t, s, v.voice));
    children.push(t.gap(0, 10));

    // Speed row: - / 1.0x / +
    children.push(speed_row(t, s, v.speed));

    let col = t.add(Node::column(0), children);
    let padded = t.pad(Insets::vh(16, m), col);
    t.add(Node::column(0).flex(1), [padded])
}

fn build_editor_body(t: &mut Tree, s: &Styles, ed: &FieldEditor, m: i32) -> NodeId {
    let title = t.label(ed.field.title(), s.font_base().bold().gray(INK));
    let placeholder = if ed.field.is_secret() {
        "输入 API Key…"
    } else {
        "输入配置值…"
    };
    let input_box = build_input_box_secret(
        t,
        s,
        &ed.keyboard.committed,
        &ed.keyboard.pinyin,
        placeholder,
        ed.field.is_secret(),
    );

    let cancel_btn = pill_btn(t, s, "取消", action::FIELD_CANCEL, false);
    let qr_btn = pill_btn(t, s, "扫码输入", action::SCAN_QR, false);
    let save_btn = pill_btn(t, s, "保存", action::FIELD_SAVE, true);
    let btn_gap1 = t.gap(12, 1);
    let btn_gap2 = t.gap(12, 1);
    let btn_row = t.add(
        Node::row(0).cross(Cross::Center),
        [cancel_btn, btn_gap1, qr_btn, btn_gap2, save_btn],
    );

    let kb = build_keyboard(t, s, &ed.keyboard, "保存");
    let g1 = t.gap(0, 12);
    let g2 = t.gap(0, 14);
    let g3 = t.gap(0, 14);
    let col = t.add(Node::column(0), [title, g1, input_box, g2, btn_row, g3, kb]);
    let pad = t.pad(Insets::vh(14, m), col);
    t.add(Node::column(0).flex(1), [pad])
}

struct QrBox {
    image: GrayImage,
}

impl Widget for QrBox {
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

fn build_qr_body(t: &mut Tree, s: &Styles, url: &str, field_title: Option<&str>, m: i32) -> NodeId {
    let heading = match field_title {
        Some(ft) => format!("扫码输入 · {ft}"),
        None => "手机扫码配置听书 API（局域网）".to_string(),
    };
    let h_lbl = t.label(&heading, s.font_base().bold().gray(INK));
    let hint = Paragraph::plain(
        "手机与 Kindle 连同一个 Wi-Fi，用手机相机或微信扫描下方二维码，粘贴 API Key 并提交，Kindle 会自动保存生效。".to_string(),
        s.font_xs().gray(INK_MID),
        Align::Left,
    );
    let hint_node = t.text(hint);

    let qr_card = if let Some(img) = render_qr(url, 420) {
        let w = t.custom(QrBox { image: img });
        let centered = t.align(0.5, 0.5, w);
        let fixed = t.fixed(None, Some(450), Some(centered));
        t.add(Node::column(0).bg(PAPER).border(2, INK, 18), [fixed])
    } else {
        let err = t.label(
            "二维码生成失败，请直接访问下方网址",
            s.font_xs().gray(INK_MID),
        );
        t.pad(Insets::vh(24, 16), err)
    };

    let url_lbl = t.label(url, s.font_sm().bold().gray(INK));
    let url_center = t.align(0.5, 0.5, url_lbl);
    let url_fixed = t.fixed(None, Some(52), Some(url_center));

    let close_btn = pill_btn(t, s, "关闭扫码", action::QR_CLOSE, false);

    let col = t.add(
        Node::column(14),
        [h_lbl, hint_node, qr_card, url_fixed, close_btn],
    );
    let pad = t.pad(Insets::vh(18, m), col);
    t.add(Node::column(0).flex(1), [pad])
}

fn field_row(t: &mut Tree, s: &Styles, label: &str, value: String, act: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm().bold().gray(INK));
    let l_col = t.add(Node::column(0).flex(1), [l]);
    let v = t.label(&value, s.font_xs().gray(INK_MID));
    let g = t.gap(10, 1);
    let chev = t.icon(Icon::ChevronRight, 24, INK_MID);
    let row = t.add(Node::row(0).cross(Cross::Center), [l_col, v, g, chev]);
    let pad = t.pad(Insets::vh(14, 18), row);
    t.add(
        Node::column(0)
            .bg(CARD)
            .border(1, RULE, 14)
            .action(act)
            .feedback(Feedback::Invert),
        [pad],
    )
}

fn toggle_row(t: &mut Tree, s: &Styles, title: &str, sub: &str, on: bool, act: ActionId) -> NodeId {
    let t_lbl = t.label(title, s.font_sm().bold().gray(INK));
    let s_lbl = t.label(sub, s.font_xs().gray(INK_MID));
    let text_col = t.add(Node::column(4).flex(1), [t_lbl, s_lbl]);
    let toggle = t.custom(Toggle { on });
    let row = t.add(Node::row(12).cross(Cross::Center), [text_col, toggle]);
    let pad = t.pad(Insets::vh(12, 18), row);
    t.add(
        Node::column(0)
            .bg(CARD)
            .border(1, RULE, 14)
            .action(act)
            .feedback(Feedback::Invert),
        [pad],
    )
}

fn voice_row(t: &mut Tree, s: &Styles, voice: &str) -> NodeId {
    let lbl = t.label("音色", s.font_sm().bold().gray(INK));
    let short_v = short_middle(voice, 24);
    let val = t.label(&short_v, s.font_xs().gray(INK_MID));
    let info_col = t.add(
        Node::column(2)
            .flex(1)
            .action(action::EDIT_VOICE)
            .feedback(Feedback::Invert),
        [lbl, val],
    );
    let prev = small_btn(t, s, "‹ 换", action::VOICE_PREV);
    let g = t.gap(8, 1);
    let next = small_btn(t, s, "换 ›", action::VOICE_NEXT);
    let row = t.add(Node::row(8).cross(Cross::Center), [info_col, prev, g, next]);
    let pad = t.pad(Insets::vh(10, 18), row);
    t.add(Node::column(0).bg(CARD).border(1, RULE, 14), [pad])
}

fn speed_row(t: &mut Tree, s: &Styles, speed: f32) -> NodeId {
    let lbl = t.label("朗读语速", s.font_sm().bold().gray(INK));
    let lbl_col = t.add(Node::column(0).flex(1), [lbl]);
    let minus = small_btn(t, s, " − ", action::SPEED_DOWN);
    let spd_txt = format!("{speed:.1}x");
    let val = t.label(&spd_txt, s.font_sm().bold().gray(INK));
    let val_pad = t.pad(Insets::vh(0, 12), val);
    let plus = small_btn(t, s, " + ", action::SPEED_UP);
    let row = t.add(
        Node::row(6).cross(Cross::Center),
        [lbl_col, minus, val_pad, plus],
    );
    let pad = t.pad(Insets::vh(10, 18), row);
    t.add(Node::column(0).bg(CARD).border(1, RULE, 14), [pad])
}

fn small_btn(t: &mut Tree, s: &Styles, text: &str, act: ActionId) -> NodeId {
    let l = t.label(text, s.font_xs().bold().gray(INK));
    t.add(
        Node::new(Kind::Padding(Insets::vh(12, 18)))
            .bg(PAPER)
            .border(2, INK, 12)
            .action(act)
            .feedback(Feedback::Invert),
        [l],
    )
}

fn pill_btn(t: &mut Tree, s: &Styles, text: &str, act: ActionId, filled: bool) -> NodeId {
    let l = t.label(
        text,
        s.font_sm().bold().gray(if filled { PAPER } else { INK }),
    );
    let c = t.align(0.5, 0.5, l);
    let mut node = Node::new(Kind::Fixed {
        w: None,
        h: Some(MIN_TARGET),
    })
    .flex(1)
    .border(2, INK, 14)
    .action(act)
    .feedback(Feedback::Invert);
    node = if filled { node.bg(INK) } else { node.bg(PAPER) };
    t.add(node, [c])
}

fn short_middle(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars || max_chars < 6 {
        return s.to_string();
    }
    let head = max_chars / 2;
    let tail = max_chars - head - 1;
    let mut out = String::new();
    for c in &chars[..head] {
        out.push(*c);
    }
    out.push('…');
    for c in &chars[chars.len() - tail..] {
        out.push(*c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Rect, SCREEN_HEIGHT, SCREEN_WIDTH};

    #[test]
    fn tts_settings_builds_cleanly() {
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let styles = Styles::new(crate::text::FamilyId(0));
        let view = TtsSettingsView {
            screen: Size::new(SCREEN_WIDTH, SCREEN_HEIGHT),
            status: StatusLine {
                clock: "14:40",
                battery: Some(0.9),
                charging: false,
            },
            provider_index: 0,
            endpoint: "https://api.siliconflow.cn/v1/audio/speech",
            api_key_summary: "已设置 (sk-…)",
            app_id: "",
            show_app_id: false,
            model: "FunAudioLLM/CosyVoice2-0.5B",
            voice: "FunAudioLLM/CosyVoice2-0.5B:alex",
            speed: 1.0,
            simulate_audio: true,
            message: Some("配置已保存"),
            editor: None,
            qr_url: None,
        };
        let (mut tree, _) = TtsSettingsUi::build(&mut text, &styles, &view);
        tree.layout(Rect::new(0, 0, SCREEN_WIDTH, SCREEN_HEIGHT), &mut text);
        let mut canvas = crate::canvas::Canvas::new(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        let mut painter =
            crate::canvas::Painter::new(&mut canvas, Rect::new(0, 0, SCREEN_WIDTH, SCREEN_HEIGHT));
        tree.paint(&mut painter, &mut text);
        let non_white = canvas.pixels.iter().filter(|&&p| p < 250).count();
        assert!(
            non_white > 100_000,
            "tts_settings rendered blank or collapsed: non_white={non_white}"
        );
    }
}
