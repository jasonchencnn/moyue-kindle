// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dedicated audiobook (TTS) playback screen:
//!
//! - **Top region (real-time reading progress page)**: displays the current
//!   spoken paragraph in full, page & chunk progress, and sleep-keepalive
//!   status (Bluetooth + Wi-Fi stay awake while locked).
//! - **Bottom region (main control popup)**: shows active voice configuration
//!   with one-tap live adjustments (switch voice, adjust speed), playback
//!   controls (prev/pause-resume/next/stop), and a shortcut to TTS Settings.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size, MIN_TARGET};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER, RULE};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};

pub mod action {
    use crate::tree::ActionId;
    /// Close player and return to book page (keeps playing if paused=false? or stops).
    pub const BACK: ActionId = 1;
    pub const PREV_CHUNK: ActionId = 2;
    pub const TOGGLE_PAUSE: ActionId = 3;
    pub const NEXT_CHUNK: ActionId = 4;
    pub const STOP: ActionId = 5;
    pub const VOICE_PREV: ActionId = 6;
    pub const VOICE_NEXT: ActionId = 7;
    pub const SPEED_DOWN: ActionId = 8;
    pub const SPEED_UP: ActionId = 9;
    pub const OPEN_SETTINGS: ActionId = 10;
}

/// View model for the live TTS audiobook player screen.
pub struct TtsPlayerView<'a> {
    pub screen: Size,
    pub status: StatusLine<'a>,
    pub book_title: &'a str,
    /// Page & chunk progress line, e.g. `"第 12 / 320 页 · 第 2 / 5 段"`.
    pub progress_label: &'a str,
    /// Status badge, e.g. `"蓝牙朗读中 · 锁屏不断网"` / `"模拟朗读中"` / `"已暂停"`.
    pub state_badge: &'a str,
    /// Full text of the paragraph currently being spoken.
    pub current_paragraph: &'a str,
    /// Optional preview of the next paragraph queued for prefetch.
    pub next_paragraph: Option<&'a str>,
    /// Active provider name (e.g. `"硅基流动"` / `"火山豆包"`).
    pub provider_name: &'a str,
    /// Active voice display name.
    pub voice_name: &'a str,
    /// Active speech rate (`0.5..=2.0`).
    pub speed: f32,
    pub playing: bool,
    pub error: Option<&'a str>,
}

/// Node handles returned after building the TTS player screen.
#[derive(Clone, Copy, Debug)]
pub struct TtsPlayerUi {
    pub loading: LoadingNodes,
}

impl TtsPlayerUi {
    pub fn build(_text: &mut TextEngine, s: &Styles, v: &TtsPlayerView<'_>) -> (Tree, TtsPlayerUi) {
        let mut t = Tree::new();
        let m = home_margin();
        let bar = status_line(&mut t, s, &v.status);
        let status = t.pad(Insets::new(0, m, 0, m), bar.root);

        // 1. Top bar: back to reader + book title + progress
        let back_ic = t.icon(Icon::ChevronLeft, 28, INK);
        let back_lbl = t.label("阅读页", s.font_xs().gray(INK));
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

        let mut title_para = Paragraph::plain(
            v.book_title.to_string(),
            s.font_base().bold().gray(INK),
            Align::Center,
        );
        title_para.style.max_lines = Some(1);
        let title_node = t.text(title_para);
        let title_col = t.add(Node::column(0).flex(1), [title_node]);

        let settings_lbl = t.label("设置", s.font_xs().bold().gray(INK));
        let settings_top_btn = t.add(
            Node::new(Kind::Padding(Insets::vh(12, 18)))
                .bg(PAPER)
                .border(2, INK, 12)
                .action(action::OPEN_SETTINGS)
                .feedback(Feedback::Invert),
            [settings_lbl],
        );

        let top_row = t.add(
            Node::row(8).cross(Cross::Center),
            [back_btn, title_col, settings_top_btn],
        );
        let top_pad = t.pad(Insets::vh(8, m), top_row);
        let top_rule = t.rule(1, RULE);

        // 2. Real-time reading progress page (full current paragraph)
        let badge_lbl = t.label(v.state_badge, s.font_xs().bold().gray(INK));
        let prog_lbl = t.label(v.progress_label, s.font_xs().gray(INK_MID));
        let badge_col = t.add(Node::column(0).flex(1), [badge_lbl]);
        let meta_row = t.add(Node::row(8).cross(Cross::Center), [badge_col, prog_lbl]);

        let mut para_children: Vec<NodeId> =
            vec![meta_row, t.gap(0, 12), t.rule(1, RULE), t.gap(0, 16)];

        if let Some(err) = v.error {
            let ep = Paragraph::plain(
                format!("提示：{err}"),
                s.font_sm().bold().gray(INK),
                Align::Left,
            );
            para_children.push(t.text(ep));
            para_children.push(t.gap(0, 12));
        }

        let spoken_text = if v.current_paragraph.trim().is_empty() {
            "正在准备朗读当前页面…".to_string()
        } else {
            v.current_paragraph.to_string()
        };
        let mut main_para = Paragraph::plain(spoken_text, s.font_lg().gray(INK), Align::Left);
        main_para.style.max_lines = Some(10);
        let main_para_node = t.text(main_para);
        let main_para_box = t.add(Node::column(0).flex(1), [main_para_node]);
        para_children.push(main_para_box);

        if let Some(next) = v.next_paragraph {
            if !next.trim().is_empty() {
                para_children.push(t.gap(0, 12));
                let mut np = Paragraph::plain(
                    format!("下一段：{next}"),
                    s.font_xs().gray(INK_MID),
                    Align::Left,
                );
                np.style.max_lines = Some(2);
                para_children.push(t.text(np));
            }
        }

        let para_inner = t.add(Node::column(0).flex(1), para_children);
        let para_pad = t.pad(Insets::vh(18, 22), para_inner);
        let para_card = t.add(
            Node::column(0).flex(1).bg(PAPER).border(2, INK, 18),
            [para_pad],
        );
        let para_wrap = t.pad(Insets::vh(16, m), para_card);
        let para_section = t.add(Node::column(0).flex(1), [para_wrap]);

        // 3. Main control popup (读书配置 + 临时调声音 + 播放控制)
        let cfg_title_txt = format!("听书配置 · {}", v.provider_name);
        let cfg_title = t.label(&cfg_title_txt, s.font_sm().bold().gray(INK));
        let cfg_title_col = t.add(Node::column(0).flex(1), [cfg_title]);
        let cfg_btn = small_pill(&mut t, s, "API/声音设置", action::OPEN_SETTINGS, false);
        let cfg_head = t.add(Node::row(8).cross(Cross::Center), [cfg_title_col, cfg_btn]);

        // Temporary voice switcher row
        let voice_lbl = t.label("音色", s.font_xs().gray(INK_MID));
        let short_v = short_voice(v.voice_name, 22);
        let voice_val = t.label(&short_v, s.font_sm().bold().gray(INK));
        let voice_info = t.add(Node::column(2).flex(1), [voice_lbl, voice_val]);
        let v_prev = small_pill(&mut t, s, "‹ 换声音", action::VOICE_PREV, false);
        let v_next = small_pill(&mut t, s, "换声音 ›", action::VOICE_NEXT, false);
        let voice_row = t.add(
            Node::row(8).cross(Cross::Center),
            [voice_info, v_prev, v_next],
        );

        // Temporary speed adjustment row
        let spd_lbl = t.label("语速", s.font_xs().gray(INK_MID));
        let spd_lbl_col = t.add(Node::column(0).flex(1), [spd_lbl]);
        let spd_down = small_pill(&mut t, s, " 慢 − ", action::SPEED_DOWN, false);
        let spd_txt = format!("{:.1}x", v.speed);
        let spd_val = t.label(&spd_txt, s.font_sm().bold().gray(INK));
        let spd_val_pad = t.pad(Insets::vh(0, 12), spd_val);
        let spd_up = small_pill(&mut t, s, " 快 + ", action::SPEED_UP, false);
        let speed_row = t.add(
            Node::row(6).cross(Cross::Center),
            [spd_lbl_col, spd_down, spd_val_pad, spd_up],
        );

        // Transport controls: 上一段 | 暂停/继续 | 下一段 | 停止
        let b_prev = transport_btn(&mut t, s, "上一段", action::PREV_CHUNK, false);
        let b_toggle = transport_btn(
            &mut t,
            s,
            if v.playing { "暂停" } else { "继续" },
            action::TOGGLE_PAUSE,
            true,
        );
        let b_next = transport_btn(&mut t, s, "下一段", action::NEXT_CHUNK, false);
        let b_stop = transport_btn(&mut t, s, "停止", action::STOP, false);
        let ctrl_row = t.add(
            Node::row(10).cross(Cross::Center),
            [b_prev, b_toggle, b_next, b_stop],
        );

        let popup_rule = t.rule(1, RULE);
        let popup_inner = t.add(
            Node::column(12),
            [cfg_head, popup_rule, voice_row, speed_row, ctrl_row],
        );
        let popup_pad = t.pad(Insets::vh(18, 22), popup_inner);
        let popup_card = t.add(Node::column(0).bg(CARD).border(2, INK, 22), [popup_pad]);
        let popup_wrap = t.pad(Insets::new(0, m, 20, m), popup_card);

        let root = t.add(
            Node::column(0).bg(PAPER),
            [status, top_pad, top_rule, para_section, popup_wrap],
        );
        t.set_root(root);
        (
            t,
            TtsPlayerUi {
                loading: bar.loading,
            },
        )
    }
}

fn small_pill(t: &mut Tree, s: &Styles, text: &str, act: ActionId, filled: bool) -> NodeId {
    let l = t.label(
        text,
        s.font_xs().bold().gray(if filled { PAPER } else { INK }),
    );
    let mut node = Node::new(Kind::Padding(Insets::vh(12, 18)))
        .border(2, INK, 12)
        .action(act)
        .feedback(Feedback::Invert);
    node = if filled { node.bg(INK) } else { node.bg(PAPER) };
    t.add(node, [l])
}

fn transport_btn(t: &mut Tree, s: &Styles, text: &str, act: ActionId, filled: bool) -> NodeId {
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

fn short_voice(s: &str, max_chars: usize) -> String {
    let name = s.rsplit(':').next().unwrap_or(s);
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max_chars {
        name.to_string()
    } else {
        let mut out: String = chars[..max_chars.saturating_sub(1)].iter().collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Rect, SCREEN_HEIGHT, SCREEN_WIDTH};

    #[test]
    fn tts_player_builds_cleanly() {
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let styles = Styles::new(crate::text::FamilyId(0));
        let view = TtsPlayerView {
            screen: Size::new(SCREEN_WIDTH, SCREEN_HEIGHT),
            status: StatusLine {
                clock: "14:45",
                battery: Some(0.88),
                charging: false,
            },
            book_title: "红楼梦",
            progress_label: "第 12 / 320 页 · 第 2 / 5 段",
            state_badge: "蓝牙朗读中 · 锁屏不断网",
            current_paragraph: "却说那女娲氏炼石补天之时，于大荒山无稽崖练成高经十二丈、方经二十四丈顽石三万六千五百零一块。",
            next_paragraph: Some("娲皇氏只用了三万六千五百块，只单单剩了一块未用。"),
            provider_name: "硅基流动",
            voice_name: "FunAudioLLM/CosyVoice2-0.5B:alex",
            speed: 1.0,
            playing: true,
            error: None,
        };
        let (mut tree, _) = TtsPlayerUi::build(&mut text, &styles, &view);
        tree.layout(Rect::new(0, 0, SCREEN_WIDTH, SCREEN_HEIGHT), &mut text);
    }
}
