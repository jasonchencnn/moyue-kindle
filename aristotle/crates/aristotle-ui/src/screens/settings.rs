// SPDX-License-Identifier: AGPL-3.0-or-later
//! Settings screen matching the prototype: light, night mode, library covers,
//! home screen components, reading typography and animation, power and screensaver,
//! network and sync, device info and about card.
//!
//! Paged into 3 pages per the prototype specification:
//! - Page 1: Screen (Frontlight slider with presets, Night mode toggle, Library covers chips)
//!   and Home components (Clock style, Today's reading stats toggle, Daily quote toggle, Desk pet toggle)
//! - Page 2: Reading (Font, Page turn animation, Full refresh interval, Clippings toggle),
//!   Power (Auto sleep, Screensaver with preview), and Network & Sync (Readest account, Progress sync,
//!   Book catalogs, Wi-Fi transfer)
//! - Page 3: Device (Date & time, Storage bar, USB mode, UI language, Battery info) and About (Device facts)

use super::library::{status_line, CoverMode, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER, RULE};
use crate::tree::{
    pages, ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use crate::widgets::Toggle;
use aristotle_i18n::{fmt_bytes, fmt_decimal, tf, tr, trn, Msg, Plural};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const PREV_PAGE: ActionId = 5;
    pub const NEXT_PAGE: ActionId = 6;
    pub const LIGHT_DOWN: ActionId = 10;
    pub const LIGHT_UP: ActionId = 11;
    pub const LIGHT_SET: ActionId = 12;
    /// `LIGHT_PRESET + i` picks `LIGHT_PRESETS[i]`.
    pub const LIGHT_PRESET: ActionId = 20;
    pub const NIGHT: ActionId = 30;
    /// `COVERS + i` picks `CoverMode::ALL[i]`.
    pub const COVERS: ActionId = 40;
    pub const SLEEP: ActionId = 50;
    pub const SLEEP_SCREEN: ActionId = 56;
    /// The "choose" link of the sleep screen row: the page that picks
    /// the screensaver's own picture.
    pub const SLEEP_IMAGE: ActionId = 57;
    pub const USB: ActionId = 60;
    pub const LANGUAGE: ActionId = 70;
    pub const CLIPPINGS: ActionId = 80;
    pub const BATTERY: ActionId = 85;
    pub const SYNC: ActionId = 90;
    pub const CATALOGS: ActionId = 91;
    pub const RETURN_KINDLE: ActionId = 92;
    pub const OPEN_KUAL: ActionId = 93;
    pub const CLOCK_STYLE: ActionId = 100;
    pub const HOME_STATS: ActionId = 110;
    pub const HOME_QUOTE: ActionId = 111;
    pub const HOME_PET: ActionId = 112;
    pub const FONT: ActionId = 120;
    pub const REFRESH_INTERVAL: ActionId = 122;
    pub const ACCOUNT: ActionId = 123;
    pub const WIFI_TRANSFER: ActionId = 124;
}

/// Light presets: Off (0), Night (2), Medium (10), Bright (20).
pub const LIGHT_PRESETS: [(Msg, u32); 4] = [
    (Msg::SettingsPresetOff, 0),
    (Msg::SettingsPresetNight, 2),
    (Msg::SettingsPresetMedium, 10),
    (Msg::SettingsPresetBright, 20),
];

/// Screensaver delays offered, in minutes; `None` is never.
pub const SLEEP_MINUTES: [Option<u32>; 5] = [Some(5), Some(10), Some(15), Some(30), None];

/// Full refresh interval options.
pub const REFRESH_OPTIONS: [Msg; 3] = [
    Msg::SettingsRefreshPage,
    Msg::SettingsRefresh10,
    Msg::SettingsRefresh30,
];

/// Clock style options.
pub const CLOCK_STYLES: [Msg; 4] = [
    Msg::SettingsClockBigLunar,
    Msg::SettingsClockBigWeek,
    Msg::SettingsClockLine,
    Msg::SettingsClockOff,
];

/// Screensaver options.
pub const SLEEP_SCREEN_OPTIONS: [Msg; 5] = [
    Msg::SettingsSleepScreenLast,
    Msg::SettingsSleepScreenCover,
    Msg::SettingsSleepScreenQuote,
    Msg::SettingsSleepScreenClip,
    Msg::SettingsSleepScreenCustom,
];

/// The USB port mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usb {
    Network,
    Storage { minutes_left: u32 },
}

/// Battery facts for diagnostics.
#[derive(Clone, Copy, Debug)]
pub struct BatteryInfo<'a> {
    pub level: f32,
    pub charging: bool,
    pub state: &'a str,
    pub voltage_mv: u32,
    pub temperature_c: i32,
}

/// Everything the settings screen shows.
#[derive(Clone)]
pub struct SystemView<'a> {
    pub status: StatusLine<'a>,
    /// Current light step and total steps (e.g. 20, 24).
    pub light: Option<(u32, u32)>,
    pub night: bool,
    pub covers: CoverMode,
    pub sleep_minutes: Option<u32>,
    pub sleep_screen: usize,
    pub sleep_screen_cover: bool,
    pub clippings: bool,
    pub battery: Option<BatteryInfo<'a>>,
    pub storage: Option<(u64, u64)>,
    pub usb: Option<Usb>,
    pub sync: Option<&'a str>,
    pub catalogs: usize,
    pub exit_message: Option<&'a str>,
    pub clock_style: usize,
    pub home_stats: bool,
    pub home_quote: bool,
    pub home_pet: bool,
    pub font: &'a str,
    pub refresh_interval: usize,
    pub wifi_transfer: &'a str,
    pub account: Option<&'a str>,
}

/// Handles to the nodes the app updates in place.
pub struct Settings {
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    pub title: NodeId,
    pub loading: LoadingNodes,
    pub light: Option<NodeId>,
}

/// "3.98 V" ("3,98 V").
pub fn volts(mv: u32) -> String {
    tf!(Msg::FormatVolts, value = fmt_decimal(mv as f64 / 1000.0, 2))
}

/// "4.1 GB free of 6.2 GB".
pub fn free_of(free: u64, total: u64) -> String {
    tf!(
        Msg::SettingsFreeOf,
        free = fmt_bytes(free),
        total = fmt_bytes(total)
    )
}

/// Screensaver duration string.
pub fn sleep_label(minutes: Option<u32>) -> String {
    match minutes {
        Some(m) => tf!(Msg::FormatMinutes, m = m),
        None => tr(Msg::SettingsSleepNever).to_string(),
    }
}

/// A horizontal rounded bar with white track and black fill.
#[derive(Clone, Copy, Debug)]
pub struct SettingBar {
    pub value: f32,
    pub height: i32,
}

impl SettingBar {
    pub fn new(value: f32, height: i32) -> Self {
        Self {
            value: value.clamp(0.0, 1.0),
            height,
        }
    }
}

impl Widget for SettingBar {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let d = text.density();
        c.constrain(Size::new(c.max.w, d.px(self.height)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        let radius = r.h as f32 / 2.0;
        p.fill_round_rect(r, radius, PAPER);
        let w = (r.w as f32 * self.value.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            p.fill_round_rect(Rect::new(r.x, r.y, w, r.h), radius, INK);
        }
    }
}

fn setting_chip(t: &mut Tree, s: &Styles, label: &str, selected: bool, id: ActionId) -> NodeId {
    let style = if selected {
        s.font_xs().bold().line_height(1.0)
    } else {
        s.font_xs().line_height(1.0)
    };
    let l = t.label(label, style);
    let pad = t.pad(Insets::new(2, 24, 0, 24), l);
    let row = t.add(
        Node::row(0)
            .cross(Cross::Center)
            .border(if selected { 0 } else { 2 }, INK, 20)
            .bg(PAPER)
            .inverted(selected)
            .feedback(Feedback::Invert)
            .action(id),
        [pad],
    );
    t.fixed(None, Some(72), Some(row))
}

fn single_card(
    t: &mut Tree,
    content: NodeId,
    action: Option<ActionId>,
    feedback: Feedback,
) -> NodeId {
    let pad = t.pad(Insets::vh(14, 28), content);
    let mut node = Node::new(Kind::MinSize(Size::new(0, 88)))
        .bg(CARD)
        .border(0, CARD, 20);
    if let Some(act) = action {
        node = node.action(act).feedback(feedback);
    }
    t.add(node, [pad])
}

fn toggle_card(t: &mut Tree, s: &Styles, label: &str, on: bool, action: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let toggle = t.custom(Toggle { on });
    let row = t.add(Node::row(0).cross(Cross::Center), [left_col, toggle]);
    single_card(t, row, Some(action), Feedback::Outline)
}

fn value_card(t: &mut Tree, s: &Styles, label: &str, value: &str, action: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let val = t.label(value, s.font_xs().gray(INK_MID));
    let chevron = t.icon(Icon::ChevronRight, 28, INK);
    let right = t.add(Node::row(10).cross(Cross::Center), [val, chevron]);
    let row = t.add(Node::row(0).cross(Cross::Center), [left_col, right]);
    single_card(t, row, Some(action), Feedback::Invert)
}

fn screensaver_card(
    t: &mut Tree,
    s: &Styles,
    label: &str,
    value: &str,
    cycle_action: ActionId,
    choose: bool,
) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let val = t.label(value, s.font_xs().gray(INK_MID));
    let chevron = t.icon(Icon::ChevronRight, 28, INK);
    let cycle_content = t.add(Node::row(10).cross(Cross::Center), [val, chevron]);
    let cycle_btn = t.add(
        Node::new(Kind::MinSize(Size::new(0, 64)))
            .action(cycle_action)
            .feedback(Feedback::Invert),
        [cycle_content],
    );
    let mut right = vec![cycle_btn];
    if choose {
        // The screensaver shows a picture of the reader's own: the row
        // offers the way to the page that picks it.
        let choose_label = t.label(tr(Msg::SettingsSleepImageChoose), s.font_xs());
        let choose_chevron = t.icon(Icon::ChevronRight, 24, INK);
        let choose_content = t.add(
            Node::row(6).cross(Cross::Center),
            [choose_label, choose_chevron],
        );
        let choose_btn = t.add(
            Node::new(Kind::MinSize(Size::new(0, 64)))
                .action(action::SLEEP_IMAGE)
                .feedback(Feedback::Invert),
            [choose_content],
        );
        right.push(choose_btn);
    }
    let mut row_children = vec![left_col];
    row_children.extend(right);
    let row = t.add(Node::row(20).cross(Cross::Center), row_children);
    single_card(t, row, None, Feedback::None)
}

fn light_card(
    t: &mut Tree,
    s: &Styles,
    step: u32,
    steps: u32,
    presets: &[(Msg, u32)],
    light_node: &mut Option<NodeId>,
) -> NodeId {
    let value_str = if step == 0 {
        tr(Msg::SettingsLightOff).to_string()
    } else {
        tf!(Msg::SettingsFrontlightLevel, step = step, steps = steps)
    };
    let l = t.label(tr(Msg::SettingsFrontlight), s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let v = t.label(&value_str, s.font_xs().gray(INK_MID));
    let head = t.add(Node::row(0).cross(Cross::Center), [left_col, v]);

    let progress = step as f32 / steps.max(1) as f32;
    let bar_widget = t.custom(SettingBar::new(progress, 14));
    let bar_col = t.add(Node::column(0).flex(1).cross(Cross::Stretch), [bar_widget]);
    let bar_action = t.add(
        Node::new(Kind::MinSize(Size::new(0, 72)))
            .action(action::LIGHT_SET)
            .feedback(Feedback::None),
        [bar_col],
    );
    *light_node = Some(bar_action);
    let bar_flex = t.add(Node::column(0).flex(1).cross(Cross::Stretch), [bar_action]);

    let active_preset = presets.iter().position(|(_, p)| *p == step);
    let mut chip_nodes = Vec::new();
    for (i, (msg, _)) in presets.iter().enumerate() {
        let label = tr(*msg);
        chip_nodes.push(setting_chip(
            t,
            s,
            label,
            active_preset == Some(i),
            action::LIGHT_PRESET + i as ActionId,
        ));
    }
    let chips_row = t.add(Node::row(14).cross(Cross::Center), chip_nodes);
    let bottom_row = t.add(Node::row(20).cross(Cross::Center), [bar_flex, chips_row]);

    let col = t.add(Node::column(16), [head, bottom_row]);
    let pad = t.pad(Insets::vh(20, 28), col);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 171)))
            .bg(CARD)
            .border(0, CARD, 20),
        [pad],
    )
}

fn covers_card(t: &mut Tree, s: &Styles, current: CoverMode) -> NodeId {
    let l = t.label(tr(Msg::SettingsCovers), s.font_sm());
    let selected_idx = CoverMode::ALL.iter().position(|&m| m == current);
    let labels = [
        tr(Msg::SettingsCoverNone),
        tr(Msg::SettingsCoverCard),
        tr(Msg::SettingsCoverPhoto),
    ];
    let mut chip_nodes = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        chip_nodes.push(setting_chip(
            t,
            s,
            label,
            selected_idx == Some(i),
            action::COVERS + i as ActionId,
        ));
    }
    let chips_row = t.add(Node::row(14).cross(Cross::Center), chip_nodes);
    let col = t.add(Node::column(16), [l, chips_row]);
    let pad = t.pad(Insets::vh(20, 28), col);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 171)))
            .bg(CARD)
            .border(0, CARD, 20),
        [pad],
    )
}

fn storage_card(t: &mut Tree, s: &Styles, storage: Option<(u64, u64)>) -> NodeId {
    let (free_str, used_pct) = match storage {
        Some((free, total)) => (
            free_of(free, total),
            1.0 - free as f32 / total.max(1) as f32,
        ),
        None => ("--".to_string(), 0.0),
    };
    let l = t.label(tr(Msg::SettingsStorageLabel), s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let v = t.label(&free_str, s.font_xs().gray(INK_MID));
    let head = t.add(Node::row(0).cross(Cross::Center), [left_col, v]);
    let bar_widget = t.custom(SettingBar::new(used_pct, 14));
    let col = t.add(Node::column(14), [head, bar_widget]);
    let pad = t.pad(Insets::vh(20, 28), col);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 114)))
            .bg(CARD)
            .border(0, CARD, 20),
        [pad],
    )
}

fn exit_pill(t: &mut Tree, s: &Styles, icon: Icon, label: &str, action: ActionId) -> NodeId {
    let ico = t.icon(icon, 32, INK);
    let l = t.label(label, s.font_xs());
    let row = t.add(Node::row(12).cross(Cross::Center), [ico, l]);
    let pad = t.pad(Insets::vh(0, 32), row);
    let fixed = t.fixed(None, Some(88), Some(pad));
    t.add(
        Node::new(Kind::Padding(Insets::all(0)))
            .border(2, INK, 20)
            .bg(PAPER)
            .action(action)
            .feedback(Feedback::Invert),
        [fixed],
    )
}

fn pager_block(
    t: &mut Tree,
    s: &Styles,
    prev_node: &mut NodeId,
    pager_label_node: &mut NodeId,
    next_node: &mut NodeId,
) -> NodeId {
    let prev_ico = t.icon(Icon::ChevronLeft, 44, RULE);
    let prev_pad = t.pad(Insets::all(22), prev_ico);
    let prev_btn = t.add(
        Node::new(Kind::MinSize(Size::new(88, 88)))
            .action(action::PREV_PAGE)
            .feedback(Feedback::None),
        [prev_pad],
    );
    *prev_node = prev_ico;

    let p_label = t.text(Paragraph::plain(
        tf!(Msg::SettingsPagerPageOf, page = 1, pages = 3),
        s.font_sm(),
        Align::Center,
    ));
    *pager_label_node = p_label;
    let hint_label = t.label(tr(Msg::SettingsPagerHint), s.font_xs().gray(INK_MID));
    let center = t.add(
        Node::column(4).cross(Cross::Center).flex(1),
        [p_label, hint_label],
    );

    let next_ico = t.icon(Icon::ChevronRight, 44, INK);
    let next_pad = t.pad(Insets::all(22), next_ico);
    let next_btn = t.add(
        Node::new(Kind::MinSize(Size::new(88, 88)))
            .action(action::NEXT_PAGE)
            .feedback(Feedback::None),
        [next_pad],
    );
    *next_node = next_ico;

    let pager = t.add(
        Node::row(0).cross(Cross::Center),
        [prev_btn, center, next_btn],
    );
    t.fixed(None, Some(104), Some(pager))
}

impl Settings {
    #[allow(clippy::vec_init_then_push)]
    pub fn build(_text: &mut TextEngine, s: &Styles, v: &SystemView) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &v.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

        // Header: Back button + Title "设置"
        let glyph = t.icon(Icon::ChevronLeft, 44, INK);
        let back = t.add(
            Node::new(Kind::MinSize(Size::new(72, 72)))
                .action(action::BACK)
                .feedback(Feedback::Invert),
            [glyph],
        );
        let title = t.label(
            tr(Msg::SettingsTitle),
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

        let mut light_node = None;

        // --- Page 1: Screen & Home components ---
        let mut p1_items = Vec::new();
        p1_items.push(t.label(
            tr(Msg::SettingsSectionScreen),
            s.font_sm().bold().line_height(1.2),
        ));
        p1_items.push(t.gap(0, 12));
        if let Some((step, steps)) = v.light {
            p1_items.push(light_card(
                &mut t,
                s,
                step,
                steps,
                &LIGHT_PRESETS,
                &mut light_node,
            ));
        }
        p1_items.push(t.gap(0, 12));
        p1_items.push(toggle_card(
            &mut t,
            s,
            tr(Msg::SettingsNight),
            v.night,
            action::NIGHT,
        ));
        p1_items.push(t.gap(0, 12));
        p1_items.push(covers_card(&mut t, s, v.covers));

        p1_items.push(t.gap(0, 28));
        p1_items.push(t.label(
            tr(Msg::SettingsHomeSection),
            s.font_sm().bold().line_height(1.2),
        ));
        p1_items.push(t.gap(0, 12));
        let clock_style_idx = v.clock_style.min(CLOCK_STYLES.len() - 1);
        p1_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsClockStyle),
            tr(CLOCK_STYLES[clock_style_idx]),
            action::CLOCK_STYLE,
        ));
        p1_items.push(t.gap(0, 12));
        p1_items.push(toggle_card(
            &mut t,
            s,
            tr(Msg::SettingsHomeStats),
            v.home_stats,
            action::HOME_STATS,
        ));
        p1_items.push(t.gap(0, 12));
        p1_items.push(toggle_card(
            &mut t,
            s,
            tr(Msg::SettingsHomeQuote),
            v.home_quote,
            action::HOME_QUOTE,
        ));
        p1_items.push(t.gap(0, 12));
        p1_items.push(toggle_card(
            &mut t,
            s,
            tr(Msg::SettingsHomePet),
            v.home_pet,
            action::HOME_PET,
        ));

        let page1 = t.add(Node::column(0), p1_items);

        // --- Page 2: Reading, Power, Network & Sync ---
        let mut p2_items = Vec::new();
        p2_items.push(t.label(
            tr(Msg::SettingsSectionReading),
            s.font_sm().bold().line_height(1.2),
        ));
        p2_items.push(t.gap(0, 12));
        p2_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsFont),
            v.font,
            action::FONT,
        ));
        p2_items.push(t.gap(0, 12));
        let refresh_idx = v.refresh_interval.min(REFRESH_OPTIONS.len() - 1);
        p2_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsRefresh),
            tr(REFRESH_OPTIONS[refresh_idx]),
            action::REFRESH_INTERVAL,
        ));

        p2_items.push(t.gap(0, 28));
        p2_items.push(t.label(
            tr(Msg::SettingsSectionPower),
            s.font_sm().bold().line_height(1.2),
        ));
        p2_items.push(t.gap(0, 12));
        let sleep_str = sleep_label(v.sleep_minutes);
        p2_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsAutoSleep),
            &sleep_str,
            action::SLEEP,
        ));
        p2_items.push(t.gap(0, 12));
        let sleep_screen_idx = v.sleep_screen.min(SLEEP_SCREEN_OPTIONS.len() - 1);
        p2_items.push(screensaver_card(
            &mut t,
            s,
            tr(Msg::SettingsSleepScreen),
            tr(SLEEP_SCREEN_OPTIONS[sleep_screen_idx]),
            action::SLEEP_SCREEN,
            sleep_screen_idx == SLEEP_SCREEN_OPTIONS.len() - 1,
        ));

        p2_items.push(t.gap(0, 28));
        p2_items.push(t.label(
            tr(Msg::SettingsSectionNetworkSync),
            s.font_sm().bold().line_height(1.2),
        ));
        p2_items.push(t.gap(0, 12));
        let account_str = match v.account {
            Some(name) => tf!(Msg::SettingsAccountSignedIn, user = name),
            None => tr(Msg::SettingsAccountGuest).to_string(),
        };
        p2_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsReadestAccount),
            &account_str,
            action::ACCOUNT,
        ));
        p2_items.push(t.gap(0, 12));
        let sync_str = match v.sync {
            Some(_) => tr(Msg::SettingsSyncKosyncOn).to_string(),
            None => tr(Msg::SettingsSyncKosyncOff).to_string(),
        };
        p2_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsProgressSync),
            &sync_str,
            action::SYNC,
        ));
        p2_items.push(t.gap(0, 12));
        let cat_str = trn(Plural::SettingsCatalogsNum, v.catalogs);
        p2_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsCatalogsLabel),
            &cat_str,
            action::CATALOGS,
        ));
        p2_items.push(t.gap(0, 12));
        p2_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsWifiTransfer),
            v.wifi_transfer,
            action::WIFI_TRANSFER,
        ));

        let page2 = t.add(Node::column(0), p2_items);

        // --- Page 3: Device & About ---
        let mut p3_items = Vec::new();
        p3_items.push(t.label(
            tr(Msg::SettingsSectionDevice),
            s.font_sm().bold().line_height(1.2),
        ));
        p3_items.push(t.gap(0, 12));
        p3_items.push(storage_card(&mut t, s, v.storage));
        p3_items.push(t.gap(0, 12));
        p3_items.push(toggle_card(
            &mut t,
            s,
            tr(Msg::SettingsClippings),
            v.clippings,
            action::CLIPPINGS,
        ));
        p3_items.push(t.gap(0, 12));
        // Without the agent (`aristotlectl`) the mode cannot be changed, so
        // the row is hidden rather than offering a switch that always fails.
        if let Some(usb) = v.usb {
            let usb_str = match usb {
                Usb::Network => tr(Msg::SettingsUsbNetSsh).to_string(),
                Usb::Storage { .. } => tr(Msg::SettingsUsbDisk).to_string(),
            };
            p3_items.push(value_card(
                &mut t,
                s,
                tr(Msg::SettingsUsbMode),
                &usb_str,
                action::USB,
            ));
            p3_items.push(t.gap(0, 12));
        }
        let locales = aristotle_i18n::locales();
        let cur_loc = locales
            .iter()
            .find(|l| l.code == aristotle_i18n::language())
            .map(|l| l.name)
            .unwrap_or("中文");
        p3_items.push(value_card(
            &mut t,
            s,
            tr(Msg::SettingsUiLanguage),
            cur_loc,
            action::LANGUAGE,
        ));
        if let Some(b) = v.battery {
            p3_items.push(t.gap(0, 12));
            let bat_str = format!(
                "{:.0}% · {} · {} · {} °C",
                b.level * 100.0,
                b.state,
                volts(b.voltage_mv),
                b.temperature_c
            );
            p3_items.push(value_card(
                &mut t,
                s,
                tr(Msg::SettingsBattery),
                &bat_str,
                action::BATTERY,
            ));
        }

        let page3 = t.add(Node::column(0), p3_items);

        let list = t.add(pages(0).flex(1), vec![page1, page2, page3]);

        // Bottom area: Exits + Pager
        let kindle_btn = exit_pill(
            &mut t,
            s,
            Icon::ChevronLeft,
            tr(Msg::SettingsReturnKindle),
            action::RETURN_KINDLE,
        );
        let kual_btn = exit_pill(
            &mut t,
            s,
            Icon::App,
            tr(Msg::SettingsOpenKual),
            action::OPEN_KUAL,
        );
        let exits_row = t.add(Node::row(16).cross(Cross::Center), [kindle_btn, kual_btn]);
        let exit_msg = v.exit_message.unwrap_or_else(|| tr(Msg::SettingsExitHint));
        let hint = t.label(exit_msg, s.font_xs().gray(INK_MID).line_height(1.3));
        let exits_block = t.add(Node::column(12), [exits_row, hint]);

        let mut prev_node = NodeId(0);
        let mut pager_label_node = NodeId(0);
        let mut next_node = NodeId(0);
        let pager = pager_block(
            &mut t,
            s,
            &mut prev_node,
            &mut pager_label_node,
            &mut next_node,
        );

        let bottom_block = t.add(Node::column(24), [exits_block, pager]);
        let content = t.add(Node::column(0).flex(1), [list, bottom_block]);
        let content = t.pad(Insets::new(24, home_margin(), 28, home_margin()), content);

        let root = t.add(Node::column(0).flex(1), [status, header, content]);
        t.set_root(root);

        let ids = Settings {
            list,
            pager_label: pager_label_node,
            prev: prev_node,
            next: next_node,
            title,
            loading: bar.loading,
            light: light_node,
        };
        (t, ids)
    }

    /// Updates the pager after layout.
    pub fn sync(&self, t: &mut Tree) {
        let (page, count) = t.page(self.list);
        t.set_text(
            self.pager_label,
            &tf!(
                Msg::SettingsPagerPageOf,
                page = page + 1,
                pages = count.max(1)
            ),
        );
        t.set_icon_gray(self.prev, if page == 0 { RULE } else { INK });
        t.set_icon_gray(self.next, if page + 1 >= count { RULE } else { INK });
    }

    /// Turns the section pages; true if `id` was a pager action.
    pub fn handle(&self, t: &mut Tree, id: ActionId) -> bool {
        let (page, _) = t.page(self.list);
        match id {
            action::PREV_PAGE => t.set_page(self.list, page.saturating_sub(1)),
            action::NEXT_PAGE => t.set_page(self.list, page + 1),
            _ => return false,
        };
        true
    }
}

/// A plausible device, for the preview and tests.
pub fn sample_about() -> Vec<(&'static str, String)> {
    vec![
        (tr(Msg::SettingsAboutModel), "Kindle Oasis 2".to_string()),
        (tr(Msg::SettingsAboutFirmware), "5.16.2.1".to_string()),
        (tr(Msg::SettingsAboutMemory), "1 GB".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{FamilyId, Fonts};
    use crate::ui::Ui;

    fn view<'a>() -> SystemView<'a> {
        SystemView {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            light: Some((20, 24)),
            night: false,
            covers: CoverMode::Cards,
            sleep_minutes: Some(10),
            sleep_screen: 2,
            sleep_screen_cover: false,
            clippings: true,
            battery: Some(BatteryInfo {
                level: 0.82,
                charging: false,
                state: "Discharging",
                voltage_mv: 3980,
                temperature_c: 26,
            }),
            storage: Some((3_200_000_000, 6_000_000_000)),
            usb: Some(Usb::Network),
            sync: Some("anna at http://192.168.15.201:17200"),
            catalogs: 3,
            exit_message: None,
            clock_style: 0,
            home_stats: true,
            home_quote: true,
            home_pet: true,
            font: "Noto Serif SC",
            refresh_interval: 1,
            wifi_transfer: "192.168.1.23",
            account: None,
        }
    }

    #[test]
    fn the_leftovers_of_finished_features_are_gone() {
        let mut text = TextEngine::new(Fonts::embedded());
        let (tree, _ids) = Settings::build(&mut text, &Styles::new(FamilyId(0)), &view());
        // 57 was the screensaver preview and 125 the date-and-time row;
        // the about card had no action and is checked by the page count:
        // page 3 now ends with the battery.
        assert!(
            !(0..tree.len()).any(|i| tree.node(NodeId(i as u32)).action == Some(57)),
            "the screensaver preview button must be gone"
        );
        assert!(
            !(0..tree.len()).any(|i| tree.node(NodeId(i as u32)).action == Some(125)),
            "the date and time row must be gone"
        );
    }

    #[test]
    fn formats() {
        aristotle_i18n::with_language("pl", || {
            assert_eq!(volts(3980), "3,98 V");
            assert_eq!(
                free_of(4_100_000_000, 6_200_000_000),
                "4,1 GB wolne z 6,2 GB"
            );
        });
        aristotle_i18n::with_language("en", || {
            assert_eq!(volts(3980), "3.98 V");
            assert_eq!(free_of(392_000_000, 494_000_000), "392 MB free of 494 MB");
        });
    }

    #[test]
    fn has_three_pages_and_controls_are_targets() {
        for lang in ["zh", "en", "pl"] {
            aristotle_i18n::with_language(lang, || {
                let mut text = TextEngine::new(Fonts::embedded());
                let (tree, ids) = Settings::build(&mut text, &Styles::new(FamilyId(0)), &view());
                let mut ui = Ui::new(tree, text, 1264, 1680);
                ui.layout();
                ids.sync(&mut ui.tree);
                let (_, count) = ui.tree.page(ids.list);
                assert_eq!(count, 3, "{lang}: must have exactly 3 pages");

                // Page 1: night mode action
                let night_node = (0..ui.tree.len())
                    .map(|i| NodeId(i as u32))
                    .find(|id| ui.tree.node(*id).action == Some(action::NIGHT))
                    .expect("night action on page 1");
                let r = ui.tree.rect(night_node);
                assert!(r.h >= 64, "{lang}: night row height must be >= 64");

                // Page 2: wifi transfer card must not overlap exit buttons
                ids.handle(&mut ui.tree, action::NEXT_PAGE);
                ui.layout();
                ids.sync(&mut ui.tree);
                let wifi = (0..ui.tree.len())
                    .map(|i| NodeId(i as u32))
                    .find(|id| ui.tree.node(*id).action == Some(action::WIFI_TRANSFER))
                    .expect("wifi action on page 2");
                let kindle = (0..ui.tree.len())
                    .map(|i| NodeId(i as u32))
                    .find(|id| ui.tree.node(*id).action == Some(action::RETURN_KINDLE))
                    .expect("return kindle action");
                assert!(
                    ui.tree.rect(wifi).bottom() <= ui.tree.rect(kindle).y,
                    "{lang}: page 2 content must not overlap exit button: wifi bottom {} > kindle y {}",
                    ui.tree.rect(wifi).bottom(),
                    ui.tree.rect(kindle).y
                );
            });
        }
    }

    #[test]
    fn exit_buttons_are_touch_targets_on_every_page_in_all_languages() {
        for code in ["zh", "en", "pl"] {
            aristotle_i18n::with_language(code, || {
                let mut text = TextEngine::new(Fonts::embedded());
                let (tree, ids) = Settings::build(&mut text, &Styles::new(FamilyId(0)), &view());
                let mut ui = Ui::new(tree, text, 1264, 1680);
                ui.layout();
                let (_, count) = ui.tree.page(ids.list);
                assert_eq!(count, 3);
                for page in 0..count {
                    ui.tree.set_page(ids.list, page);
                    ui.layout();
                    let back = (0..ui.tree.len())
                        .map(|i| NodeId(i as u32))
                        .find(|id| ui.tree.node(*id).action == Some(action::BACK))
                        .unwrap();
                    let r = ui.tree.rect(back);
                    assert!(r.w >= 64 && r.h >= 64);

                    for act in [action::RETURN_KINDLE, action::OPEN_KUAL] {
                        let id = (0..ui.tree.len())
                            .map(|i| NodeId(i as u32))
                            .find(|node_id| ui.tree.node(*node_id).action == Some(act))
                            .unwrap();
                        let r = ui.tree.rect(id);
                        assert!(r.h >= 64, "{code} page {page}: exit button height >= 64");
                    }
                }
            });
        }
    }

    #[test]
    fn the_usb_row_is_hidden_without_the_agent() {
        let mut v = view();
        v.usb = None;
        let mut text = TextEngine::new(Fonts::embedded());
        let (tree, ids) = Settings::build(&mut text, &Styles::new(FamilyId(0)), &v);
        let mut ui = Ui::new(tree, text, 1264, 1680);
        ui.layout();
        // The row sits on page 3, but it is built only when `aristotlectl`
        // is there; a row that always fails is worse than no row.
        for page in 0..ui.tree.page(ids.list).1 {
            ui.tree.set_page(ids.list, page);
            ui.layout();
            assert!(
                !(0..ui.tree.len())
                    .any(|i| ui.tree.node(NodeId(i as u32)).action == Some(action::USB)),
                "page {page}: USB row must be hidden"
            );
        }
    }
}
