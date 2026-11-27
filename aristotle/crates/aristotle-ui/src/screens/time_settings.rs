// SPDX-License-Identifier: AGPL-3.0-or-later
//! Time, timezone, 12/24-hour clock, and manual/network time sync screen (`SYS-002`).

use super::library::{page_of, status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::Toggle;
use aristotle_i18n::{tr, Msg};
use aristotle_screen::{TimezoneOption, TIMEZONES};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const PREV_PAGE: ActionId = 5;
    pub const NEXT_PAGE: ActionId = 6;
    pub const CLOCK_24H: ActionId = 10;
    pub const SYNC_NET: ActionId = 11;
    pub const YEAR_DOWN: ActionId = 16;
    pub const YEAR_UP: ActionId = 17;
    pub const MONTH_DOWN: ActionId = 18;
    pub const MONTH_UP: ActionId = 19;
    pub const DAY_DOWN: ActionId = 20;
    pub const DAY_UP: ActionId = 21;
    pub const HOUR_DOWN: ActionId = 22;
    pub const HOUR_UP: ActionId = 23;
    pub const MINUTE_DOWN: ActionId = 24;
    pub const MINUTE_UP: ActionId = 25;
    /// `TZ_BASE + i` selects `TIMEZONES[i]`.
    pub const TZ_BASE: ActionId = 100;
}

/// Localized display label for a timezone option.
pub fn timezone_label(tz: &TimezoneOption) -> &'static str {
    if aristotle_i18n::language().starts_with("zh") {
        tz.label_zh
    } else {
        tz.label_en
    }
}

/// View data for the time & timezone settings screen.
#[derive(Clone)]
pub struct TimeSettingsView<'a> {
    pub status: StatusLine<'a>,
    /// Formatted date and time banner, e.g. `"2026-11-24  21:38"`.
    pub now_display: &'a str,
    pub clock_24h: bool,
    pub timezone_id: &'a str,
    pub syncing: bool,
    pub message: Option<&'a str>,
}

/// Handles to dynamic nodes on the time settings screen.
pub struct TimeSettingsUi {
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    pub loading: LoadingNodes,
}

impl TimeSettingsUi {
    pub fn build(_text: &mut TextEngine, s: &Styles, v: &TimeSettingsView<'_>) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &v.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

        let glyph = t.icon(Icon::ChevronLeft, 44, INK);
        let back = t.add(
            Node::new(Kind::MinSize(Size::new(72, 72)))
                .action(action::BACK)
                .feedback(Feedback::Invert),
            [glyph],
        );
        let title = t.label(
            tr(Msg::DeviceSettingsTimeTitle),
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

        let mut blocks = Vec::new();

        // Current clock banner
        let now_p = Paragraph::plain(v.now_display, s.font_lg().bold(), Align::Center);
        let now_node = t.text(now_p);
        let now_pad = t.pad(Insets::vh(20, 24), now_node);
        blocks.push(
            t.add(
                Node::new(Kind::MinSize(Size::new(0, 96)))
                    .bg(CARD)
                    .border(2, INK, 20),
                [now_pad],
            ),
        );

        // 24h toggle + Network sync
        let c_24h = toggle_card(
            &mut t,
            s,
            tr(Msg::DeviceSettingsTimeClock24h),
            v.clock_24h,
            action::CLOCK_24H,
        );
        let sync_lbl = if v.syncing {
            tr(Msg::DeviceSettingsTimeSyncing)
        } else {
            tr(Msg::DeviceSettingsTimeSyncNet)
        };
        let c_sync = value_card(
            &mut t,
            s,
            sync_lbl,
            v.message.unwrap_or(""),
            action::SYNC_NET,
        );
        blocks.push(t.add(Node::column(12), [c_24h, c_sync]));

        // Manual adjustment card
        let man_lbl = t.label(tr(Msg::DeviceSettingsTimeManual), s.font_sm().bold());
        let r_year = stepper_row(
            &mut t,
            s,
            tr(Msg::DeviceSettingsTimeYear),
            action::YEAR_DOWN,
            action::YEAR_UP,
        );
        let r_month = stepper_row(
            &mut t,
            s,
            tr(Msg::DeviceSettingsTimeMonth),
            action::MONTH_DOWN,
            action::MONTH_UP,
        );
        let r_day = stepper_row(
            &mut t,
            s,
            tr(Msg::DeviceSettingsTimeDay),
            action::DAY_DOWN,
            action::DAY_UP,
        );
        let r_hour = stepper_row(
            &mut t,
            s,
            tr(Msg::DeviceSettingsTimeHour),
            action::HOUR_DOWN,
            action::HOUR_UP,
        );
        let r_min = stepper_row(
            &mut t,
            s,
            tr(Msg::DeviceSettingsTimeMinute),
            action::MINUTE_DOWN,
            action::MINUTE_UP,
        );
        let man_col = t.add(
            Node::column(12),
            [man_lbl, r_year, r_month, r_day, r_hour, r_min],
        );
        let man_pad = t.pad(Insets::vh(18, 24), man_col);
        blocks.push(
            t.add(
                Node::new(Kind::MinSize(Size::new(0, 380)))
                    .bg(CARD)
                    .border(0, CARD, 20),
                [man_pad],
            ),
        );

        // Timezone heading + list items
        let tz_heading = t.label(tr(Msg::DeviceSettingsTimeTimezone), s.font_sm().bold());
        blocks.push(tz_heading);
        for (i, tz) in TIMEZONES.iter().enumerate() {
            let selected = tz.id == v.timezone_id;
            let mark = if selected { "✓" } else { "" };
            blocks.push(value_card(
                &mut t,
                s,
                timezone_label(tz),
                mark,
                action::TZ_BASE + i as ActionId,
            ));
        }

        let list = t.add(pages(12).flex(1), blocks);
        let list_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), list);
        let list_wrap = t.add(Node::column(0).flex(1), [list_pad]);

        let prev_ico = t.icon(Icon::ChevronLeft, 44, RULE);
        let prev_pad = t.pad(Insets::all(22), prev_ico);
        let prev_btn = t.add(
            Node::new(Kind::MinSize(Size::new(88, 88)))
                .action(action::PREV_PAGE)
                .feedback(Feedback::None),
            [prev_pad],
        );
        let pager_label = t.text(Paragraph::plain("1 / 1", s.font_sm(), Align::Center));
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

    pub fn sync_pager(&self, t: &mut Tree) {
        let (page, count) = t.page(self.list);
        let count = count.max(1);
        t.set_text(self.pager_label, &page_of(page + 1, count));
        t.set_icon_gray(self.prev, if page > 0 { INK } else { RULE });
        t.set_icon_gray(self.next, if page + 1 < count { INK } else { RULE });
    }

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

fn step_btn(t: &mut Tree, s: &Styles, text: &str, act: ActionId) -> NodeId {
    let lb = t.label(text, s.font_base().bold());
    let center = t.align(0.5, 0.5, lb);
    t.add(
        Node::new(Kind::Fixed {
            w: Some(72),
            h: Some(60),
        })
        .bg(PAPER)
        .border(2, INK, 16)
        .action(act)
        .feedback(Feedback::Invert),
        [center],
    )
}

fn stepper_row(t: &mut Tree, s: &Styles, label: &str, down: ActionId, up: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left = t.add(Node::column(0).flex(1), [l]);
    let minus = step_btn(t, s, "−", down);
    let plus = step_btn(t, s, "+", up);
    t.add(Node::row(12).cross(Cross::Center), [left, minus, plus])
}

fn toggle_card(t: &mut Tree, s: &Styles, label: &str, on: bool, act: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let toggle = t.custom(Toggle { on });
    let row = t.add(Node::row(0).cross(Cross::Center), [left_col, toggle]);
    let pad = t.pad(Insets::vh(14, 28), row);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 84)))
            .bg(CARD)
            .border(0, CARD, 20)
            .action(act)
            .feedback(Feedback::Outline),
        [pad],
    )
}

fn value_card(t: &mut Tree, s: &Styles, label: &str, value: &str, act: ActionId) -> NodeId {
    let l = t.label(label, s.font_sm());
    let left_col = t.add(Node::column(0).flex(1), [l]);
    let val = t.label(value, s.font_xs().gray(INK_MID));
    let chevron = t.icon(Icon::ChevronRight, 28, INK);
    let right = t.add(Node::row(10).cross(Cross::Center), [val, chevron]);
    let row = t.add(Node::row(0).cross(Cross::Center), [left_col, right]);
    let pad = t.pad(Insets::vh(14, 28), row);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 84)))
            .bg(CARD)
            .border(0, CARD, 20)
            .action(act)
            .feedback(Feedback::Invert),
        [pad],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{FamilyId, Fonts};
    use crate::ui::Ui;

    #[test]
    fn time_settings_lays_out_cleanly() {
        let view = TimeSettingsView {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            now_display: "2026-11-24  21:38",
            clock_24h: true,
            timezone_id: "Asia/Shanghai",
            syncing: false,
            message: None,
        };
        for (w, h, dpi) in [(1264, 1680, 300), (600, 800, 167)] {
            let screen = aristotle_screen::Screen::new(w, h, dpi).unwrap();
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0)).with_density(screen.density);
            let (tree, ids) = TimeSettingsUi::build(&mut text, &styles, &view);
            let mut ui = Ui::with_screen(tree, text, screen);
            ui.layout();
            ids.sync_pager(&mut ui.tree);
            ui.render();
            assert!(ui.tree.page(ids.list).1 >= 2);
        }
    }
}
