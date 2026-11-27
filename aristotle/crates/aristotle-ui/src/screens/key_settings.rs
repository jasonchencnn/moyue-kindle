// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hardware page-turn button settings (shown only on hardware with physical
//! keys) and reader touch-zone layout settings (`SYS-003`).

use super::library::{page_of, status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::Toggle;
use aristotle_i18n::{tr, Msg};
use aristotle_screen::{KeyLongPressAction, TouchZoneLayout};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const PREV_PAGE: ActionId = 5;
    pub const NEXT_PAGE: ActionId = 6;
    pub const INVERT_KEYS: ActionId = 10;
    pub const LONG_PRESS_ACTION: ActionId = 11;
    pub const DISABLE_TOUCH_TURN: ActionId = 12;
    pub const TOUCH_ZONE_LAYOUT: ActionId = 13;
}

/// Localized label for a [`KeyLongPressAction`].
pub fn long_press_label(act: KeyLongPressAction) -> &'static str {
    match act {
        KeyLongPressAction::RepeatTurn => tr(Msg::DeviceSettingsKeysLpRepeat),
        KeyLongPressAction::ChapterJump => tr(Msg::DeviceSettingsKeysLpChapter),
        KeyLongPressAction::ToggleLight => tr(Msg::DeviceSettingsKeysLpLight),
        KeyLongPressAction::FullRefresh => tr(Msg::DeviceSettingsKeysLpRefresh),
        KeyLongPressAction::Disabled => tr(Msg::DeviceSettingsKeysLpDisabled),
    }
}

/// Localized label for a [`TouchZoneLayout`].
pub fn touch_zone_label(layout: TouchZoneLayout) -> &'static str {
    match layout {
        TouchZoneLayout::Default30_70 => tr(Msg::DeviceSettingsKeysZoneDefault),
        TouchZoneLayout::Split50_50 => tr(Msg::DeviceSettingsKeysZoneSplit),
        TouchZoneLayout::NextOnly => tr(Msg::DeviceSettingsKeysZoneNextOnly),
    }
}

/// View data for the keys & touch settings screen.
#[derive(Clone)]
pub struct KeySettingsView<'a> {
    pub status: StatusLine<'a>,
    /// True only on hardware that carries physical page-turn buttons
    /// (Oasis 1/2/3, Voyage, Kobo Libra 2).
    pub has_page_keys: bool,
    pub invert_page_keys: bool,
    pub long_press: KeyLongPressAction,
    pub disable_touch_turn: bool,
    pub touch_zone: TouchZoneLayout,
}

/// Handles to dynamic nodes on the keys & touch settings screen.
pub struct KeySettingsUi {
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    pub loading: LoadingNodes,
}

impl KeySettingsUi {
    pub fn build(_text: &mut TextEngine, s: &Styles, v: &KeySettingsView<'_>) -> (Tree, Self) {
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
            tr(Msg::DeviceSettingsKeysTitle),
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

        let mut blocks = Vec::new();
        if v.has_page_keys {
            let heading = t.label(tr(Msg::DeviceSettingsKeysSectionHw), s.font_sm().bold());
            let c_inv = toggle_card(
                &mut t,
                s,
                tr(Msg::DeviceSettingsKeysInvert),
                v.invert_page_keys,
                action::INVERT_KEYS,
            );
            let c_lp = value_card(
                &mut t,
                s,
                tr(Msg::DeviceSettingsKeysLongPress),
                long_press_label(v.long_press),
                action::LONG_PRESS_ACTION,
            );
            let c_touch_off = toggle_card(
                &mut t,
                s,
                tr(Msg::DeviceSettingsKeysDisableTouch),
                v.disable_touch_turn,
                action::DISABLE_TOUCH_TURN,
            );
            blocks.push(t.add(Node::column(12), [heading, c_inv, c_lp, c_touch_off]));
        }

        let c_zone = value_card(
            &mut t,
            s,
            tr(Msg::DeviceSettingsKeysTouchZone),
            touch_zone_label(v.touch_zone),
            action::TOUCH_ZONE_LAYOUT,
        );
        blocks.push(c_zone);

        let list = t.add(pages(16).flex(1), blocks);
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
    fn hardware_section_hidden_without_page_keys() {
        let screen = aristotle_screen::Screen::new(1264, 1680, 300).unwrap();
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0)).with_density(screen.density);
        let view = KeySettingsView {
            status: StatusLine::default(),
            has_page_keys: false,
            invert_page_keys: false,
            long_press: KeyLongPressAction::RepeatTurn,
            disable_touch_turn: false,
            touch_zone: TouchZoneLayout::Default30_70,
        };
        let (tree, _) = KeySettingsUi::build(&mut text, &styles, &view);
        assert!(
            !(0..tree.len())
                .any(|i| tree.node(NodeId(i as u32)).action == Some(action::INVERT_KEYS)),
            "hardware key toggle must be hidden when device lacks physical keys"
        );
        assert!(
            (0..tree.len())
                .any(|i| tree.node(NodeId(i as u32)).action == Some(action::TOUCH_ZONE_LAYOUT)),
            "touch zone layout must remain visible on touch-only models"
        );
    }

    #[test]
    fn key_settings_lays_out_with_hardware_keys() {
        let view = KeySettingsView {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.80),
                charging: false,
            },
            has_page_keys: true,
            invert_page_keys: true,
            long_press: KeyLongPressAction::ChapterJump,
            disable_touch_turn: false,
            touch_zone: TouchZoneLayout::Split50_50,
        };
        for (w, h, dpi) in [(1264, 1680, 300), (600, 800, 167)] {
            let screen = aristotle_screen::Screen::new(w, h, dpi).unwrap();
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0)).with_density(screen.density);
            let (tree, ids) = KeySettingsUi::build(&mut text, &styles, &view);
            let mut ui = Ui::with_screen(tree, text, screen);
            ui.layout();
            ids.sync_pager(&mut ui.tree);
            ui.render();
            assert!(ui.tree.page(ids.list).1 >= 1);
        }
    }
}
