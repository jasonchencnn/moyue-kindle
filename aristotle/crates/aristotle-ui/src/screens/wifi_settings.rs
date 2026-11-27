// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wi-Fi connection and airplane mode management screen (`SYS-001`).

use super::library::{page_of, status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::Toggle;
use aristotle_i18n::{tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const PREV_PAGE: ActionId = 5;
    pub const NEXT_PAGE: ActionId = 6;
    pub const AIRPLANE: ActionId = 10;
    pub const WIFI_TOGGLE: ActionId = 11;
    pub const SCAN: ActionId = 12;
    pub const DISCONNECT: ActionId = 13;
    pub const FORGET_CURRENT: ActionId = 14;
    /// `CONNECT_BASE + i` selects `networks[i]`.
    pub const CONNECT_BASE: ActionId = 100;
}

/// One Wi-Fi network row displayed in the scan list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiRow {
    pub ssid: String,
    pub signal_bars: u8,
    pub secured: bool,
    pub connected: bool,
    pub saved: bool,
}

/// Data rendered by the Wi-Fi settings screen.
#[derive(Clone)]
pub struct WifiSettingsView<'a> {
    pub status: StatusLine<'a>,
    pub airplane_mode: bool,
    pub wifi_enabled: bool,
    pub scanning: bool,
    pub connected_ssid: Option<&'a str>,
    pub ip: Option<&'a str>,
    pub message: Option<&'a str>,
    pub networks: &'a [WifiRow],
}

/// Handles to dynamic nodes on the Wi-Fi settings screen.
pub struct WifiSettingsUi {
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    pub loading: LoadingNodes,
}

impl WifiSettingsUi {
    pub fn build(_text: &mut TextEngine, s: &Styles, v: &WifiSettingsView<'_>) -> (Tree, Self) {
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
            tr(Msg::DeviceSettingsWifiTitle),
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

        let mut blocks = Vec::new();

        // Radio controls block
        let c_airplane = toggle_card(
            &mut t,
            s,
            tr(Msg::DeviceSettingsWifiAirplane),
            v.airplane_mode,
            action::AIRPLANE,
        );
        let c_wifi = toggle_card(
            &mut t,
            s,
            tr(Msg::DeviceSettingsWifiEnabled),
            v.wifi_enabled && !v.airplane_mode,
            action::WIFI_TOGGLE,
        );
        let scan_label = if v.scanning {
            tr(Msg::DeviceSettingsWifiScanning)
        } else {
            tr(Msg::DeviceSettingsWifiScan)
        };
        let c_scan = action_card(&mut t, s, scan_label, "", action::SCAN);
        blocks.push(t.add(Node::column(12), [c_airplane, c_wifi, c_scan]));

        if let Some(msg) = v.message.filter(|m| !m.is_empty()) {
            let lbl = t.label(msg, s.font_xs().gray(INK_MID));
            let pad = t.pad(Insets::vh(14, 24), lbl);
            blocks.push(
                t.add(
                    Node::new(Kind::MinSize(Size::new(0, 64)))
                        .bg(CARD)
                        .border(0, CARD, 20),
                    [pad],
                ),
            );
        }

        // Current connection block
        if let Some(ssid) = v.connected_ssid {
            let sub = match v.ip {
                Some(ip) => format!("{} · {ip}", tr(Msg::DeviceSettingsWifiConnected)),
                None => tr(Msg::DeviceSettingsWifiConnected).to_string(),
            };
            let title_n = t.label(ssid, s.font_sm().bold());
            let sub_n = t.label(&sub, s.font_xs().gray(INK_MID));
            let info = t.add(Node::column(6).flex(1), [title_n, sub_n]);
            let btn_disc = pill_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsWifiDisconnect),
                action::DISCONNECT,
            );
            let btn_forget = pill_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsWifiForget),
                action::FORGET_CURRENT,
            );
            let btns = t.add(Node::row(12).cross(Cross::Center), [btn_disc, btn_forget]);
            let col = t.add(Node::column(14), [info, btns]);
            let pad = t.pad(Insets::vh(18, 24), col);
            blocks.push(
                t.add(
                    Node::new(Kind::MinSize(Size::new(0, 148)))
                        .bg(CARD)
                        .border(2, INK, 20),
                    [pad],
                ),
            );
        }

        // Network list items (each row is its own block so `pages` paginates cleanly)
        if v.networks.is_empty() && !v.airplane_mode && v.wifi_enabled {
            let empty = t.label(
                tr(Msg::DeviceSettingsWifiNoNetworks),
                s.font_xs().gray(INK_MID),
            );
            let pad = t.pad(Insets::vh(18, 24), empty);
            blocks.push(
                t.add(
                    Node::new(Kind::MinSize(Size::new(0, 76)))
                        .bg(CARD)
                        .border(0, CARD, 20),
                    [pad],
                ),
            );
        } else {
            for (i, net) in v.networks.iter().enumerate() {
                let bars = signal_glyph(net.signal_bars);
                let sec = if net.secured {
                    tr(Msg::DeviceSettingsWifiSecured)
                } else {
                    tr(Msg::DeviceSettingsWifiOpen)
                };
                let right_text = if net.connected {
                    format!("{} · {bars}", tr(Msg::DeviceSettingsWifiConnected))
                } else {
                    format!("{sec} · {bars}")
                };
                blocks.push(action_card(
                    &mut t,
                    s,
                    &net.ssid,
                    &right_text,
                    action::CONNECT_BASE + i as ActionId,
                ));
            }
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

fn signal_glyph(bars: u8) -> &'static str {
    match bars {
        0 => "····",
        1 => "▂···",
        2 => "▂▄··",
        3 => "▂▄▆·",
        _ => "▂▄▆█",
    }
}

fn pill_btn(t: &mut Tree, s: &Styles, label: &str, act: ActionId) -> NodeId {
    let l = t.label(label, s.font_xs());
    let pad = t.pad(Insets::vh(10, 22), l);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 60)))
            .bg(PAPER)
            .border(2, INK, 16)
            .action(act)
            .feedback(Feedback::Invert),
        [pad],
    )
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

fn action_card(t: &mut Tree, s: &Styles, label: &str, value: &str, act: ActionId) -> NodeId {
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
    fn wifi_settings_lays_out_on_seven_and_six_inch_panels() {
        let nets = vec![
            WifiRow {
                ssid: "HomeWifi".into(),
                signal_bars: 4,
                secured: true,
                connected: true,
                saved: true,
            },
            WifiRow {
                ssid: "CafeOpen".into(),
                signal_bars: 2,
                secured: false,
                connected: false,
                saved: false,
            },
        ];
        let view = WifiSettingsView {
            status: StatusLine {
                clock: "14:20",
                battery: Some(0.85),
                charging: false,
            },
            airplane_mode: false,
            wifi_enabled: true,
            scanning: false,
            connected_ssid: Some("HomeWifi"),
            ip: Some("192.168.1.42"),
            message: None,
            networks: &nets,
        };
        for (w, h, dpi) in [(1264, 1680, 300), (600, 800, 167)] {
            let screen = aristotle_screen::Screen::new(w, h, dpi).unwrap();
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0)).with_density(screen.density);
            let (tree, ids) = WifiSettingsUi::build(&mut text, &styles, &view);
            let mut ui = Ui::with_screen(tree, text, screen);
            ui.layout();
            ids.sync_pager(&mut ui.tree);
            ui.render();
            assert!(ui.tree.page(ids.list).1 >= 1);
        }
    }
}
