// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bluetooth page-turner settings screen: radio toggle, device discovery &
//! pairing, PIN/Passkey entry with on-screen keyboard, and SSP passkey
//! confirmation.

use super::library::{page_of, status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::keyboard::{
    action as kb_action, build_input_box, build_keyboard, InputMode, KeyboardPage, KeyboardState,
};
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
    pub const BT_TOGGLE: ActionId = 10;
    pub const SCAN: ActionId = 11;
    pub const DISCONNECT: ActionId = 12;
    pub const FORGET_CURRENT: ActionId = 13;
    pub const PIN_CANCEL: ActionId = 20;
    pub const PIN_CONFIRM: ActionId = 21;
    pub const PIN_PRESET_0000: ActionId = 22;
    pub const PIN_PRESET_1234: ActionId = 23;
    pub const PIN_PRESET_8888: ActionId = 24;
    pub const PASSKEY_ACCEPT: ActionId = 25;
    pub const PASSKEY_CANCEL: ActionId = 26;
    /// `CONNECT_BASE + i` pairs/connects `devices[i]`.
    pub const CONNECT_BASE: ActionId = 100;
    /// `PAIR_PIN_BASE + i` opens the PIN-entry modal for `devices[i]`.
    pub const PAIR_PIN_BASE: ActionId = 300;
}

/// One Bluetooth device row displayed in the scan/paired list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothRow {
    pub mac: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub rssi: Option<i32>,
}

/// Active pairing authentication prompt overlay inside the Bluetooth screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BluetoothAuthPrompt {
    /// Prompt the user to type a PIN code or passkey with the on-screen keyboard.
    PinEntry {
        mac: String,
        name: String,
        prompt: Option<String>,
        keyboard: KeyboardState,
    },
    /// Prompt the user to confirm an SSP 6-digit passkey shown on both devices.
    ConfirmPasskey {
        mac: String,
        name: String,
        passkey: String,
    },
}

impl BluetoothAuthPrompt {
    /// Construct a fresh PIN-entry prompt defaulting to English + Symbols/Digits page.
    pub fn new_pin(mac: String, name: String, prompt: Option<String>) -> Self {
        let mut keyboard = KeyboardState::new();
        keyboard.set_mode(InputMode::English);
        keyboard.page = KeyboardPage::Symbols;
        Self::PinEntry {
            mac,
            name,
            prompt,
            keyboard,
        }
    }

    /// Handle a keyboard or preset action inside `PinEntry`. Returns `true` if
    /// the keyboard state changed and needs a redraw.
    pub fn handle_keyboard_action(&mut self, id: ActionId) -> bool {
        let Self::PinEntry { keyboard, .. } = self else {
            return false;
        };
        match id {
            action::PIN_PRESET_0000 => {
                keyboard.committed = "0000".to_string();
                keyboard.pinyin.clear();
                true
            }
            action::PIN_PRESET_1234 => {
                keyboard.committed = "1234".to_string();
                keyboard.pinyin.clear();
                true
            }
            action::PIN_PRESET_8888 => {
                keyboard.committed = "8888".to_string();
                keyboard.pinyin.clear();
                true
            }
            kb_action::MODE_ZH => {
                keyboard.set_mode(InputMode::Chinese);
                true
            }
            kb_action::MODE_EN => {
                keyboard.set_mode(InputMode::English);
                true
            }
            kb_action::TOGGLE_PAGE => {
                keyboard.toggle_page();
                true
            }
            kb_action::SHIFT => {
                keyboard.press_shift();
                true
            }
            kb_action::SPACE => {
                keyboard.press_space();
                true
            }
            kb_action::DELETE => {
                keyboard.press_delete();
                true
            }
            id if (kb_action::KEY_BASE..kb_action::KEY_BASE + 0x1000).contains(&id) => {
                if let Some(ch) = char::from_u32(id - kb_action::KEY_BASE) {
                    if keyboard.committed.chars().count() < 16 {
                        keyboard.press_key(ch);
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }
}

/// Data rendered by the Bluetooth settings screen.
#[derive(Clone)]
pub struct BluetoothSettingsView<'a> {
    pub status: StatusLine<'a>,
    pub available: bool,
    pub powered: bool,
    pub scanning: bool,
    pub connected_device: Option<&'a BluetoothRow>,
    pub message: Option<&'a str>,
    pub devices: &'a [BluetoothRow],
    pub auth_prompt: Option<&'a BluetoothAuthPrompt>,
}

/// Handles to dynamic nodes on the Bluetooth settings screen.
pub struct BluetoothSettingsUi {
    pub list: Option<NodeId>,
    pub pager_label: Option<NodeId>,
    pub prev: Option<NodeId>,
    pub next: Option<NodeId>,
    pub loading: LoadingNodes,
}

impl BluetoothSettingsUi {
    pub fn build(
        _text: &mut TextEngine,
        s: &Styles,
        v: &BluetoothSettingsView<'_>,
    ) -> (Tree, Self) {
        if let Some(prompt) = v.auth_prompt {
            return build_auth_screen(s, &v.status, prompt);
        }

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
            tr(Msg::DeviceSettingsBtTitle),
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

        let mut blocks = Vec::new();

        // Radio toggle + scan action
        let c_power = toggle_card(
            &mut t,
            s,
            tr(Msg::DeviceSettingsBtEnabled),
            v.powered,
            action::BT_TOGGLE,
        );
        let scan_label = if v.scanning {
            tr(Msg::DeviceSettingsBtScanning)
        } else {
            tr(Msg::DeviceSettingsBtScan)
        };
        let c_scan = action_card(&mut t, s, scan_label, "", action::SCAN);
        blocks.push(t.add(Node::column(12), [c_power, c_scan]));

        if !v.available {
            blocks.push(info_banner(&mut t, s, tr(Msg::DeviceSettingsBtUnavailable)));
        }
        if let Some(msg) = v.message.filter(|m| !m.is_empty()) {
            blocks.push(info_banner(&mut t, s, msg));
        }

        // Current connected device highlight card
        if let Some(dev) = v.connected_device {
            let sub = format!("{} · {}", tr(Msg::DeviceSettingsBtConnected), dev.mac);
            let title_n = t.label(&dev.name, s.font_sm().bold());
            let sub_n = t.label(&sub, s.font_xs().gray(INK_MID));
            let info = t.add(Node::column(6).flex(1), [title_n, sub_n]);
            let btn_disc = pill_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsBtDisconnect),
                action::DISCONNECT,
            );
            let btn_forget = pill_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsBtForget),
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

        // Discovered & paired device rows
        if v.devices.is_empty() && v.powered {
            blocks.push(info_banner(&mut t, s, tr(Msg::DeviceSettingsBtNoDevices)));
        } else {
            for (i, dev) in v.devices.iter().enumerate() {
                blocks.push(device_row_card(
                    &mut t,
                    s,
                    dev,
                    action::CONNECT_BASE + i as ActionId,
                    action::PAIR_PIN_BASE + i as ActionId,
                ));
            }
        }

        // Usage hint footer card
        blocks.push(info_banner(&mut t, s, tr(Msg::DeviceSettingsBtHint)));

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
                list: Some(list),
                pager_label: Some(pager_label),
                prev: Some(prev_ico),
                next: Some(next_ico),
                loading: bar.loading,
            },
        )
    }

    pub fn sync_pager(&self, t: &mut Tree) {
        let (Some(list), Some(pager_label), Some(prev), Some(next)) =
            (self.list, self.pager_label, self.prev, self.next)
        else {
            return;
        };
        let (page, count) = t.page(list);
        let count = count.max(1);
        t.set_text(pager_label, &page_of(page + 1, count));
        t.set_icon_gray(prev, if page > 0 { INK } else { RULE });
        t.set_icon_gray(next, if page + 1 < count { INK } else { RULE });
    }

    pub fn handle_pager(&self, t: &mut Tree, id: ActionId) -> bool {
        let Some(list) = self.list else {
            return false;
        };
        let (page, count) = t.page(list);
        match id {
            action::PREV_PAGE if page > 0 => {
                t.set_page(list, page - 1);
                self.sync_pager(t);
                true
            }
            action::NEXT_PAGE if page + 1 < count => {
                t.set_page(list, page + 1);
                self.sync_pager(t);
                true
            }
            _ => false,
        }
    }
}

fn build_auth_screen(
    s: &Styles,
    status_line_data: &StatusLine<'_>,
    prompt: &BluetoothAuthPrompt,
) -> (Tree, BluetoothSettingsUi) {
    let mut t = Tree::new();
    let bar = status_line(&mut t, s, status_line_data);
    let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

    let back_action = match prompt {
        BluetoothAuthPrompt::PinEntry { .. } => action::PIN_CANCEL,
        BluetoothAuthPrompt::ConfirmPasskey { .. } => action::PASSKEY_CANCEL,
    };
    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(back_action)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title_str = match prompt {
        BluetoothAuthPrompt::PinEntry { .. } => tr(Msg::DeviceSettingsBtPinTitle),
        BluetoothAuthPrompt::ConfirmPasskey { .. } => tr(Msg::DeviceSettingsBtPasskeyPrompt),
    };
    let title = t.label(title_str, s.font_lg().bold().line_height(1.2));
    let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    let header = t.fixed(None, Some(112), Some(header));
    let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

    match prompt {
        BluetoothAuthPrompt::PinEntry {
            mac,
            name,
            prompt: msg,
            keyboard,
        } => {
            let mut form_items = Vec::new();
            let dev_lbl = t.label(&format!("{name} ({mac})"), s.font_sm().bold());
            form_items.push(dev_lbl);
            form_items.push(t.gap(0, 12));

            if let Some(m) = msg.as_deref().filter(|m| !m.is_empty()) {
                form_items.push(info_banner(&mut t, s, m));
                form_items.push(t.gap(0, 12));
            }

            let input_box = build_input_box(
                &mut t,
                s,
                &keyboard.committed,
                &keyboard.pinyin,
                tr(Msg::DeviceSettingsBtPinPlaceholder),
            );
            form_items.push(input_box);
            form_items.push(t.gap(0, 16));

            // Quick common PIN presets so users rarely need to type on e-ink.
            let p0 = pill_btn(&mut t, s, "0000", action::PIN_PRESET_0000);
            let p1 = pill_btn(&mut t, s, "1234", action::PIN_PRESET_1234);
            let p2 = pill_btn(&mut t, s, "8888", action::PIN_PRESET_8888);
            let presets = t.add(Node::row(12).cross(Cross::Center), [p0, p1, p2]);
            form_items.push(presets);
            form_items.push(t.gap(0, 20));

            let btn_cancel = pill_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsBtPinCancel),
                action::PIN_CANCEL,
            );
            let btn_ok = primary_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsBtPinConfirm),
                action::PIN_CONFIRM,
            );
            let actions = t.add(Node::row(16).cross(Cross::Center), [btn_cancel, btn_ok]);
            form_items.push(actions);

            let form_col = t.add(Node::column(0).flex(1), form_items);
            let form_pad = t.pad(Insets::new(16, home_margin(), 16, home_margin()), form_col);

            let kb = build_keyboard(&mut t, s, keyboard, tr(Msg::DeviceSettingsBtPinConfirm));
            let root = t.add(Node::column(0), [status, header, form_pad, kb]);
            t.set_root(root);
        }
        BluetoothAuthPrompt::ConfirmPasskey { mac, name, passkey } => {
            let dev_lbl = t.label(&format!("{name} ({mac})"), s.font_sm().bold());
            let code_lbl = t.text(Paragraph::plain(
                passkey,
                s.font_lg().bold().tracking(10.0),
                Align::Center,
            ));
            let code_pad = t.pad(Insets::vh(36, 24), code_lbl);
            let code_card = t.add(
                Node::new(Kind::MinSize(Size::new(0, 140)))
                    .bg(CARD)
                    .border(2, INK, 20),
                [code_pad],
            );
            let hint = info_banner(&mut t, s, tr(Msg::DeviceSettingsBtPasskeyPrompt));
            let btn_cancel = pill_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsBtPinCancel),
                action::PASSKEY_CANCEL,
            );
            let btn_ok = primary_btn(
                &mut t,
                s,
                tr(Msg::DeviceSettingsBtPasskeyAccept),
                action::PASSKEY_ACCEPT,
            );
            let actions = t.add(Node::row(16).cross(Cross::Center), [btn_cancel, btn_ok]);
            let body = t.add(
                Node::column(20).flex(1),
                [dev_lbl, code_card, hint, actions],
            );
            let body_pad = t.pad(Insets::new(24, home_margin(), 24, home_margin()), body);
            let root = t.add(Node::column(0), [status, header, body_pad]);
            t.set_root(root);
        }
    }

    (
        t,
        BluetoothSettingsUi {
            list: None,
            pager_label: None,
            prev: None,
            next: None,
            loading: bar.loading,
        },
    )
}

fn info_banner(t: &mut Tree, s: &Styles, msg: &str) -> NodeId {
    let lbl = t.label(msg, s.font_xs().gray(INK_MID));
    let pad = t.pad(Insets::vh(14, 24), lbl);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 64)))
            .bg(CARD)
            .border(0, CARD, 20),
        [pad],
    )
}

fn toggle_card(t: &mut Tree, s: &Styles, label: &str, on: bool, act: ActionId) -> NodeId {
    let lbl = t.label(label, s.font_sm());
    let lbl_w = t.add(Node::column(0).flex(1), [lbl]);
    let tog = t.add(Node::new(Kind::Custom(Box::new(Toggle { on }))), []);
    let row = t.add(Node::row(16).cross(Cross::Center), [lbl_w, tog]);
    let pad = t.pad(Insets::vh(18, 24), row);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 88)))
            .bg(CARD)
            .border(0, CARD, 20)
            .action(act)
            .feedback(Feedback::Invert),
        [pad],
    )
}

fn action_card(t: &mut Tree, s: &Styles, label: &str, right: &str, act: ActionId) -> NodeId {
    let lbl = t.label(label, s.font_sm());
    let lbl_w = t.add(Node::column(0).flex(1), [lbl]);
    let r = t.label(right, s.font_xs().gray(INK_MID));
    let row = t.add(Node::row(16).cross(Cross::Center), [lbl_w, r]);
    let pad = t.pad(Insets::vh(18, 24), row);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 88)))
            .bg(CARD)
            .border(0, CARD, 20)
            .action(act)
            .feedback(Feedback::Invert),
        [pad],
    )
}

fn device_row_card(
    t: &mut Tree,
    s: &Styles,
    dev: &BluetoothRow,
    connect_act: ActionId,
    pin_act: ActionId,
) -> NodeId {
    let status_txt = if dev.connected {
        tr(Msg::DeviceSettingsBtConnected)
    } else if dev.paired {
        tr(Msg::DeviceSettingsBtPaired)
    } else {
        tr(Msg::DeviceSettingsBtUnpaired)
    };
    let sub = match dev.rssi {
        Some(dbm) => format!("{status_txt} · {} · {dbm} dBm", dev.mac),
        None => format!("{status_txt} · {}", dev.mac),
    };
    let name_n = t.label(&dev.name, s.font_sm().bold());
    let sub_n = t.label(&sub, s.font_xs().gray(INK_MID));
    let info = t.add(Node::column(4).flex(1), [name_n, sub_n]);
    let main_tap = t.add(
        Node::new(Kind::MinSize(Size::new(0, 68)))
            .flex(1)
            .action(connect_act)
            .feedback(Feedback::Invert),
        [info],
    );

    let row = if dev.connected {
        t.add(Node::row(12).cross(Cross::Center), [main_tap])
    } else {
        let pin_btn = pill_btn(t, s, tr(Msg::DeviceSettingsBtPairPin), pin_act);
        t.add(Node::row(12).cross(Cross::Center), [main_tap, pin_btn])
    };
    let pad = t.pad(Insets::vh(14, 24), row);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, 96)))
            .bg(CARD)
            .border(0, CARD, 20),
        [pad],
    )
}

fn pill_btn(t: &mut Tree, s: &Styles, label: &str, act: ActionId) -> NodeId {
    let lbl = t.label(label, s.font_xs().bold());
    let pad = t.pad(Insets::vh(14, 22), lbl);
    t.add(
        Node::new(Kind::MinSize(Size::new(88, 64)))
            .bg(PAPER)
            .border(2, INK, 16)
            .action(act)
            .feedback(Feedback::Invert),
        [pad],
    )
}

fn primary_btn(t: &mut Tree, s: &Styles, label: &str, act: ActionId) -> NodeId {
    let lbl = t.label(label, s.font_xs().bold().gray(PAPER));
    let pad = t.pad(Insets::vh(14, 28), lbl);
    t.add(
        Node::new(Kind::MinSize(Size::new(120, 64)))
            .bg(INK)
            .border(2, INK, 16)
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

    fn sample_devices() -> Vec<BluetoothRow> {
        vec![
            BluetoothRow {
                mac: "AA:BB:CC:DD:EE:FF".into(),
                name: "8BitDo Micro".into(),
                paired: true,
                connected: true,
                rssi: Some(-52),
            },
            BluetoothRow {
                mac: "11:22:33:44:55:66".into(),
                name: "Page Turner Pro".into(),
                paired: false,
                connected: false,
                rssi: Some(-68),
            },
        ]
    }

    #[test]
    fn bluetooth_settings_lays_out_on_seven_and_six_inch_panels() {
        let devices = sample_devices();
        let view = BluetoothSettingsView {
            status: StatusLine {
                clock: "12:00",
                battery: Some(0.88),
                charging: false,
            },
            available: true,
            powered: true,
            scanning: false,
            connected_device: Some(&devices[0]),
            message: None,
            devices: &devices,
            auth_prompt: None,
        };
        for (w, h, dpi) in [(1264, 1680, 300), (600, 800, 167)] {
            let screen = aristotle_screen::Screen::new(w, h, dpi).unwrap();
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0)).with_density(screen.density);
            let (tree, ids) = BluetoothSettingsUi::build(&mut text, &styles, &view);
            let mut ui = Ui::with_screen(tree, text, screen);
            ui.layout();
            ids.sync_pager(&mut ui.tree);
            ui.render();
            assert!(ids.list.is_some());
        }
    }

    #[test]
    fn bluetooth_pin_and_passkey_modals_lay_out_and_accept_presets() {
        let mut pin = BluetoothAuthPrompt::new_pin(
            "11:22:33:44:55:66".into(),
            "Page Turner Pro".into(),
            Some("Enter PIN code".into()),
        );
        assert!(pin.handle_keyboard_action(action::PIN_PRESET_1234));
        if let BluetoothAuthPrompt::PinEntry { keyboard, .. } = &pin {
            assert_eq!(keyboard.committed, "1234");
        } else {
            panic!("expected PinEntry");
        }

        let screen = aristotle_screen::Screen::new(1264, 1680, 300).unwrap();
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0)).with_density(screen.density);
        let status = StatusLine {
            clock: "12:00",
            battery: Some(0.88),
            charging: false,
        };
        let view = BluetoothSettingsView {
            status,
            available: true,
            powered: true,
            scanning: false,
            connected_device: None,
            message: None,
            devices: &[],
            auth_prompt: Some(&pin),
        };
        let (tree, ids) = BluetoothSettingsUi::build(&mut text, &styles, &view);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        ui.render();
        assert!(ids.list.is_none());

        let ssp = BluetoothAuthPrompt::ConfirmPasskey {
            mac: "11:22:33:44:55:66".into(),
            name: "Page Turner Pro".into(),
            passkey: "482910".into(),
        };
        let mut text2 = TextEngine::new(Fonts::embedded());
        let view2 = BluetoothSettingsView {
            auth_prompt: Some(&ssp),
            ..view
        };
        let (tree2, _) = BluetoothSettingsUi::build(&mut text2, &styles, &view2);
        let mut ui2 = Ui::with_screen(tree2, text2, screen);
        ui2.layout();
        ui2.render();
    }
}
