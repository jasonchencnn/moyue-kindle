// SPDX-License-Identifier: AGPL-3.0-or-later
//! Configuration screen for the reader's top bar and bottom bar:
//! style presets (`Full`, `Minimal`, `Immersive`, `Custom`), live preview,
//! independent header/footer toggles, left/center/right slot pickers,
//! progress bar, dividers, bookmark ribbon, and compact height.

use super::library::{page_of, status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::{FineProgressBar, Toggle};
use aristotle_i18n::{tr, Msg};
use aristotle_screen::{BarItem, ReaderBarConfig, ReaderBarPreset};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const PREV_PAGE: ActionId = 5;
    pub const NEXT_PAGE: ActionId = 6;
    /// `PRESET_BASE + i` selects `ReaderBarPreset::ALL[i]`.
    pub const PRESET_BASE: ActionId = 10;
    pub const SHOW_HEADER: ActionId = 20;
    pub const SHOW_FOOTER: ActionId = 21;
    /// `HEADER_SLOT_BASE + 0..3` cycles left / center / right top-bar slots.
    pub const HEADER_SLOT_BASE: ActionId = 30;
    /// `FOOTER_SLOT_BASE + 0..3` cycles left / center / right bottom-bar slots.
    pub const FOOTER_SLOT_BASE: ActionId = 40;
    pub const PROGRESS_BAR: ActionId = 50;
    pub const DIVIDERS: ActionId = 51;
    pub const BOOKMARK_RIBBON: ActionId = 52;
    pub const COMPACT: ActionId = 53;
}

/// Localized display label for a [`ReaderBarPreset`].
pub fn preset_label(preset: ReaderBarPreset) -> &'static str {
    match preset {
        ReaderBarPreset::Full => tr(Msg::BarsPresetFull),
        ReaderBarPreset::Minimal => tr(Msg::BarsPresetMinimal),
        ReaderBarPreset::Immersive => tr(Msg::BarsPresetImmersive),
        ReaderBarPreset::Custom => tr(Msg::BarsPresetCustom),
    }
}

/// Localized display label for a [`BarItem`].
pub fn item_label(item: BarItem) -> &'static str {
    match item {
        BarItem::None => tr(Msg::BarsItemNone),
        BarItem::BookTitle => tr(Msg::BarsItemBookTitle),
        BarItem::ChapterTitle => tr(Msg::BarsItemChapterTitle),
        BarItem::Clock => tr(Msg::BarsItemClock),
        BarItem::Battery => tr(Msg::BarsItemBattery),
        BarItem::Wifi => tr(Msg::BarsItemWifi),
        BarItem::Frontlight => tr(Msg::BarsItemFrontlight),
        BarItem::ProgressPercent => tr(Msg::BarsItemProgressPercent),
        BarItem::BookPages => tr(Msg::BarsItemBookPages),
        BarItem::ChapterPages => tr(Msg::BarsItemChapterPages),
        BarItem::ChapterTimeLeft => tr(Msg::BarsItemChapterTime),
        BarItem::BookTimeLeft => tr(Msg::BarsItemBookTime),
        BarItem::DeviceStatusGroup => tr(Msg::BarsItemDeviceStatus),
    }
}

/// Apply a UI action from this screen to `config`. Returns `true` if `config`
/// changed.
pub fn apply_action(config: &mut ReaderBarConfig, id: ActionId) -> bool {
    if (action::PRESET_BASE..action::PRESET_BASE + ReaderBarPreset::ALL.len() as ActionId)
        .contains(&id)
    {
        let preset = ReaderBarPreset::from_index((id - action::PRESET_BASE) as usize);
        config.apply_preset(preset);
        return true;
    }
    match id {
        action::SHOW_HEADER => {
            config.show_header = !config.show_header;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        action::SHOW_FOOTER => {
            config.show_footer = !config.show_footer;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        action::PROGRESS_BAR => {
            config.show_progress_bar = !config.show_progress_bar;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        action::DIVIDERS => {
            config.show_dividers = !config.show_dividers;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        action::BOOKMARK_RIBBON => {
            config.show_bookmark_ribbon = !config.show_bookmark_ribbon;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        action::COMPACT => {
            config.compact = !config.compact;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        id if (action::HEADER_SLOT_BASE..action::HEADER_SLOT_BASE + 3).contains(&id) => {
            let slot = (id - action::HEADER_SLOT_BASE) as usize;
            config.header_slots[slot] = config.header_slots[slot].next();
            config.show_header = true;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        id if (action::FOOTER_SLOT_BASE..action::FOOTER_SLOT_BASE + 3).contains(&id) => {
            let slot = (id - action::FOOTER_SLOT_BASE) as usize;
            config.footer_slots[slot] = config.footer_slots[slot].next();
            config.show_footer = true;
            config.preset = ReaderBarPreset::Custom;
            true
        }
        _ => false,
    }
}

/// View data passed to build the reader bar customization screen.
#[derive(Clone)]
pub struct ReaderBarsView<'a> {
    pub status: StatusLine<'a>,
    pub config: ReaderBarConfig,
    pub book_title: &'a str,
    pub chapter_title: &'a str,
    pub clock: &'a str,
    pub battery_pct: u8,
    pub light_step: u32,
    pub light_max: u32,
}

impl<'a> ReaderBarsView<'a> {
    /// Sample view data for desktop preview and layout tests.
    pub fn sample(status: StatusLine<'a>, config: ReaderBarConfig) -> Self {
        Self {
            status,
            config,
            book_title: "曹雪芹 · 红楼梦",
            chapter_title: "第七回 送宫花贾琏戏熙凤",
            clock: "21:38",
            battery_pct: 84,
            light_step: 12,
            light_max: 24,
        }
    }
}

/// Handles to dynamic nodes of the reader bars screen.
pub struct ReaderBarsUi {
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    pub loading: LoadingNodes,
}

impl ReaderBarsUi {
    /// Build the reader top/bottom bar customization screen.
    pub fn build(_text: &mut TextEngine, s: &Styles, v: &ReaderBarsView<'_>) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &v.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

        // Header: Back arrow + Title
        let glyph = t.icon(Icon::ChevronLeft, 44, INK);
        let back = t.add(
            Node::new(Kind::MinSize(Size::new(72, 72)))
                .action(action::BACK)
                .feedback(Feedback::Invert),
            [glyph],
        );
        let title = t.label(
            tr(Msg::BarsTitle),
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

        // Live preview block
        let preview_block = preview_card(&mut t, s, v);

        // Preset selector block
        let preset_block = preset_card(&mut t, s, v.config.preset);

        // Header settings block
        let h_show = toggle_card(
            &mut t,
            s,
            tr(Msg::BarsShowHeader),
            v.config.show_header,
            action::SHOW_HEADER,
        );
        let h_left = value_card(
            &mut t,
            s,
            tr(Msg::BarsHeaderLeft),
            item_label(v.config.header_slots[0]),
            action::HEADER_SLOT_BASE,
        );
        let h_center = value_card(
            &mut t,
            s,
            tr(Msg::BarsHeaderCenter),
            item_label(v.config.header_slots[1]),
            action::HEADER_SLOT_BASE + 1,
        );
        let h_right = value_card(
            &mut t,
            s,
            tr(Msg::BarsHeaderRight),
            item_label(v.config.header_slots[2]),
            action::HEADER_SLOT_BASE + 2,
        );
        let header_block = t.add(Node::column(12), [h_show, h_left, h_center, h_right]);

        // Footer settings block
        let f_show = toggle_card(
            &mut t,
            s,
            tr(Msg::BarsShowFooter),
            v.config.show_footer,
            action::SHOW_FOOTER,
        );
        let f_left = value_card(
            &mut t,
            s,
            tr(Msg::BarsFooterLeft),
            item_label(v.config.footer_slots[0]),
            action::FOOTER_SLOT_BASE,
        );
        let f_center = value_card(
            &mut t,
            s,
            tr(Msg::BarsFooterCenter),
            item_label(v.config.footer_slots[1]),
            action::FOOTER_SLOT_BASE + 1,
        );
        let f_right = value_card(
            &mut t,
            s,
            tr(Msg::BarsFooterRight),
            item_label(v.config.footer_slots[2]),
            action::FOOTER_SLOT_BASE + 2,
        );
        let footer_block = t.add(Node::column(12), [f_show, f_left, f_center, f_right]);

        // Extras block
        let ex_bar = toggle_card(
            &mut t,
            s,
            tr(Msg::BarsShowProgressBar),
            v.config.show_progress_bar,
            action::PROGRESS_BAR,
        );
        let ex_div = toggle_card(
            &mut t,
            s,
            tr(Msg::BarsShowDividers),
            v.config.show_dividers,
            action::DIVIDERS,
        );
        let ex_rib = toggle_card(
            &mut t,
            s,
            tr(Msg::BarsShowBookmarkRibbon),
            v.config.show_bookmark_ribbon,
            action::BOOKMARK_RIBBON,
        );
        let ex_cmp = toggle_card(
            &mut t,
            s,
            tr(Msg::BarsCompact),
            v.config.compact,
            action::COMPACT,
        );
        let extras_block = t.add(Node::column(12), [ex_bar, ex_div, ex_rib, ex_cmp]);

        let list = t.add(
            pages(16).flex(1),
            [
                preview_block,
                preset_block,
                header_block,
                footer_block,
                extras_block,
            ],
        );
        let list_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), list);
        let list_wrap = t.add(Node::column(0).flex(1), [list_pad]);

        // Pager footer
        let prev_ico = t.icon(Icon::ChevronLeft, 44, RULE);
        let prev_pad = t.pad(Insets::all(22), prev_ico);
        let prev_btn = t.add(
            Node::new(Kind::MinSize(Size::new(88, 88)))
                .action(action::PREV_PAGE)
                .feedback(Feedback::None),
            [prev_pad],
        );
        let pager_label = t.text(Paragraph::plain("1 / 2", s.font_sm(), Align::Center));
        let center = t.add(Node::column(0).cross(Cross::Center).flex(1), [pager_label]);
        let next_ico = t.icon(Icon::ChevronRight, 44, INK);
        let next_pad = t.pad(Insets::all(22), next_ico);
        let next_btn = t.add(
            Node::new(Kind::MinSize(Size::new(88, 88)))
                .action(action::NEXT_PAGE)
                .feedback(Feedback::None),
            [next_pad],
        );
        let pager_row = t.add(
            Node::row(0).cross(Cross::Center),
            [prev_btn, center, next_btn],
        );
        let pager = t.fixed(None, Some(96), Some(pager_row));
        let pager = t.pad(Insets::new(0, home_margin(), 16, home_margin()), pager);

        let root = t.add(Node::column(0), [status, header, list_wrap, pager]);
        t.set_root(root);

        (
            t,
            Self {
                list,
                pager_label,
                prev: prev_ico,
                next: next_ico,
                loading: bar.loading,
            },
        )
    }

    /// Synchronize pager label and arrow colors after layout or page change.
    pub fn sync_pager(&self, t: &mut Tree) {
        let (page, count) = t.page(self.list);
        let count = count.max(1);
        t.set_text(self.pager_label, &page_of(page + 1, count));
        t.set_icon_gray(self.prev, if page > 0 { INK } else { RULE });
        t.set_icon_gray(self.next, if page + 1 < count { INK } else { RULE });
    }

    /// Handle prev/next page actions; returns `true` if the page changed.
    pub fn handle_pager(&self, t: &mut Tree, id: ActionId) -> bool {
        let (page, count) = t.page(self.list);
        match id {
            action::PREV_PAGE if page > 0 => {
                t.set_page(self.list, page - 1);
                self.sync_pager(t);
                true
            }
            action::NEXT_PAGE if page + 1 < count => {
                t.set_page(self.list, page + 1);
                self.sync_pager(t);
                true
            }
            _ => false,
        }
    }
}

fn preview_item_text(item: BarItem, v: &ReaderBarsView<'_>) -> String {
    match item {
        BarItem::None => String::new(),
        BarItem::BookTitle => v.book_title.to_string(),
        BarItem::ChapterTitle => v.chapter_title.to_string(),
        BarItem::Clock => v.clock.to_string(),
        BarItem::Battery => {
            if crate::status::show_battery_pct() {
                format!("{}%", v.battery_pct)
            } else {
                String::new()
            }
        }
        BarItem::Wifi => "Wi-Fi".to_string(),
        BarItem::Frontlight => {
            if v.light_step == 0 {
                format!("☀ {}", tr(Msg::BarsLightOff))
            } else {
                format!("☀ {}/{}", v.light_step, v.light_max)
            }
        }
        BarItem::ProgressPercent => "43%".to_string(),
        BarItem::BookPages => "128 / 298".to_string(),
        BarItem::ChapterPages => "5 / 14".to_string(),
        BarItem::ChapterTimeLeft => "12 min".to_string(),
        BarItem::BookTimeLeft => "4h 15m".to_string(),
        BarItem::DeviceStatusGroup => {
            if crate::status::show_battery_pct() {
                format!(
                    "Wi-Fi · ☀{} · {}% · {}",
                    v.light_step, v.battery_pct, v.clock
                )
            } else {
                format!("Wi-Fi · ☀{} · {}", v.light_step, v.clock)
            }
        }
    }
}

fn preview_slot_cell(
    t: &mut Tree,
    s: &Styles,
    item: BarItem,
    v: &ReaderBarsView<'_>,
    align: Align,
) -> NodeId {
    if item == BarItem::Battery && !crate::status::show_battery_pct() {
        let icon = t.icon(Icon::Battery, 24, INK_MID);
        let ax = match align {
            Align::Left | Align::Justify => 0.0,
            Align::Center => 0.5,
            Align::Right => 1.0,
        };
        let aligned = t.align(ax, 0.5, icon);
        return t.add(Node::column(0).flex(1), [aligned]);
    }
    let style = s.font_xs().gray(INK_MID);
    let mut p = Paragraph::plain(preview_item_text(item, v), style, align);
    p.style.max_lines = Some(1);
    let txt = t.text(p);
    t.add(Node::column(0).flex(1), [txt])
}

fn preview_slot_row(
    t: &mut Tree,
    s: &Styles,
    slots: [BarItem; 3],
    v: &ReaderBarsView<'_>,
) -> NodeId {
    let left = preview_slot_cell(t, s, slots[0], v, Align::Left);
    let mid = preview_slot_cell(t, s, slots[1], v, Align::Center);
    let right = preview_slot_cell(t, s, slots[2], v, Align::Right);

    t.add(Node::row(8).cross(Cross::Center), [left, mid, right])
}

fn preview_card(t: &mut Tree, s: &Styles, v: &ReaderBarsView<'_>) -> NodeId {
    let mut rows = Vec::new();
    if v.config.show_header {
        rows.push(preview_slot_row(t, s, v.config.header_slots, v));
        if v.config.show_dividers {
            rows.push(t.rule(2, RULE));
        }
    }

    let mut body = Paragraph::plain(
        "话说周瑞家的送了刘姥姥去后，便上来回王夫人话，谁知王夫人不在上房。",
        s.font_xs(),
        Align::Left,
    );
    body.style.max_lines = Some(2);
    let body_text = t.text(body);
    let body_node = t.pad(Insets::vh(12, 0), body_text);
    rows.push(body_node);

    if v.config.show_footer {
        if v.config.show_dividers {
            rows.push(t.rule(2, RULE));
        }
        rows.push(preview_slot_row(t, s, v.config.footer_slots, v));
        if v.config.show_progress_bar {
            let bar = t.custom(FineProgressBar::new(0.43, CARD));
            let bar_wrap = t.fixed(None, Some(10), Some(bar));
            rows.push(bar_wrap);
        }
    }

    let inner = t.add(Node::column(8), rows);
    let pad = t.pad(Insets::vh(18, 24), inner);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 176)))
            .bg(PAPER)
            .border(2, INK, 20),
        [pad],
    )
}

fn preset_card(t: &mut Tree, s: &Styles, current: ReaderBarPreset) -> NodeId {
    let title = t.label(tr(Msg::BarsPresetLabel), s.font_sm());
    let mut chips = Vec::new();
    for (i, preset) in ReaderBarPreset::ALL.iter().copied().enumerate() {
        let selected = preset == current;
        let style = if selected {
            s.font_xs().bold().line_height(1.0)
        } else {
            s.font_xs().line_height(1.0)
        };
        let lb = t.label(preset_label(preset), style);
        let pad = t.pad(Insets::new(2, 20, 0, 20), lb);
        let chip = t.add(
            Node::row(0)
                .cross(Cross::Center)
                .border(if selected { 0 } else { 2 }, INK, 20)
                .bg(PAPER)
                .inverted(selected)
                .feedback(Feedback::Invert)
                .action(action::PRESET_BASE + i as ActionId),
            [pad],
        );
        chips.push(t.fixed(None, Some(68), Some(chip)));
    }
    let chip_row = t.add(Node::row(12).cross(Cross::Center), chips);
    let col = t.add(Node::column(14), [title, chip_row]);
    let pad = t.pad(Insets::vh(18, 24), col);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 152)))
            .bg(CARD)
            .border(0, CARD, 20),
        [pad],
    )
}

fn single_card(t: &mut Tree, content: NodeId, act: ActionId, feedback: Feedback) -> NodeId {
    let pad = t.pad(Insets::vh(14, 28), content);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 84)))
            .bg(CARD)
            .border(0, CARD, 20)
            .action(act)
            .feedback(feedback),
        [pad],
    )
}

fn toggle_card(t: &mut Tree, s: &Styles, label: &str, on: bool, act: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let toggle = t.custom(Toggle { on });
    let row = t.add(Node::row(0).cross(Cross::Center), [left_col, toggle]);
    single_card(t, row, act, Feedback::Outline)
}

fn value_card(t: &mut Tree, s: &Styles, label: &str, value: &str, act: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let val = t.label(value, s.font_xs().gray(INK_MID));
    let chevron = t.icon(Icon::ChevronRight, 28, INK);
    let right = t.add(Node::row(10).cross(Cross::Center), [val, chevron]);
    let row = t.add(Node::row(0).cross(Cross::Center), [left_col, right]);
    single_card(t, row, act, Feedback::Invert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Ui;

    #[test]
    fn actions_update_presets_and_switch_to_custom() {
        let mut cfg = ReaderBarConfig::default();
        assert_eq!(cfg.preset, ReaderBarPreset::Full);

        assert!(apply_action(&mut cfg, action::PRESET_BASE + 2));
        assert_eq!(cfg.preset, ReaderBarPreset::Immersive);
        assert!(!cfg.show_header);
        assert!(!cfg.show_footer);

        assert!(apply_action(&mut cfg, action::SHOW_HEADER));
        assert_eq!(cfg.preset, ReaderBarPreset::Custom);
        assert!(cfg.show_header);

        let before = cfg.header_slots[2];
        assert!(apply_action(&mut cfg, action::HEADER_SLOT_BASE + 2));
        assert_eq!(cfg.header_slots[2], before.next());
    }

    #[test]
    fn screen_lays_out_cleanly_on_seven_and_six_inch_panels() {
        use crate::text::{FamilyId, Fonts};
        let status = StatusLine {
            clock: "21:38",
            battery: Some(0.84),
            charging: false,
        };
        for (w, h, dpi) in [(1264, 1680, 300), (600, 800, 167)] {
            let screen = aristotle_screen::Screen::new(w, h, dpi).unwrap();
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0)).with_density(screen.density);
            let view = ReaderBarsView::sample(status, ReaderBarConfig::default());
            let (tree, ui_handles) = ReaderBarsUi::build(&mut text, &styles, &view);
            let mut ui = Ui::with_screen(tree, text, screen);
            ui.layout();
            ui_handles.sync_pager(&mut ui.tree);
            ui.render();
            let (_, pages) = ui.tree.page(ui_handles.list);
            assert!(pages >= 1, "expected at least one page on {w}x{h}");
        }
    }
}
