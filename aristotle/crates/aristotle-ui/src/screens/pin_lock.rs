// SPDX-License-Identifier: AGPL-3.0-or-later
//! 4-digit PIN passcode screen for both unlocking the app and managing
//! (setting, changing, or disabling) the lock passcode from Settings.

use aristotle_i18n::{tr, Msg};

use crate::geom::Insets;
use crate::screens::library::{status_line, StatusLine};
use crate::text::{Align, Paragraph};
use crate::theme::{Styles, INK, PAPER};
use crate::tree::{ActionId, Cross, Kind, Node, NodeId, Tree};

/// Fixed length of the app lock PIN.
pub const PIN_LEN: usize = 4;

/// Actions emitted by [`PinLockView`].
pub mod action {
    use crate::tree::ActionId;

    /// Digit buttons `0..=9` emit `DIGIT_BASE + digit`.
    pub const DIGIT_BASE: ActionId = 100;
    /// Clear all entered digits.
    pub const CLEAR: ActionId = 120;
    /// Delete the last entered digit.
    pub const BACKSPACE: ActionId = 121;
    /// Cancel and return to Settings (hidden in `Unlock` mode).
    pub const CANCEL: ActionId = 122;
    /// From `Manage` mode: start setting a new PIN.
    pub const CHANGE_PIN: ActionId = 123;
    /// From `Manage` mode: turn off the app passcode.
    pub const DISABLE_PIN: ActionId = 124;
}

/// Which step of the PIN flow the screen is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMode {
    /// App launch or wakeup unlock: cannot be cancelled.
    Unlock,
    /// Entering a brand-new 4-digit PIN.
    SetNew,
    /// Re-entering the new 4-digit PIN to confirm.
    ConfirmNew,
    /// Verifying the existing PIN before changing or disabling it.
    VerifyOld,
    /// Existing PIN verified: choose between changing or disabling the PIN.
    Manage,
}

/// View model for the PIN lock / setup screen.
pub struct PinLockView<'a> {
    pub status: StatusLine<'a>,
    pub mode: PinMode,
    /// Number of digits currently entered (`0..=PIN_LEN`).
    pub entered_len: usize,
    /// Optional inline error message (e.g. wrong PIN or confirmation mismatch).
    pub error: Option<&'a str>,
}

/// Build the PIN lock / passcode management screen tree.
pub fn build(s: &Styles, v: &PinLockView<'_>) -> Tree {
    let mut t = Tree::new();
    let status = status_line(&mut t, s, &v.status).root;
    let sep = t.rule(2, INK);

    let title_text = tr(Msg::DeviceSettingsDevPinTitle);
    let prompt_text = match v.mode {
        PinMode::Unlock => tr(Msg::DeviceSettingsDevPinEnter),
        PinMode::SetNew => tr(Msg::DeviceSettingsDevPinSet),
        PinMode::ConfirmNew => tr(Msg::DeviceSettingsDevPinConfirm),
        PinMode::VerifyOld => tr(Msg::DeviceSettingsDevPinVerify),
        PinMode::Manage => tr(Msg::DeviceSettingsDevPinOn),
    };

    let title = t.text(Paragraph::plain(title_text, s.title(), Align::Center));
    let prompt = t.text(Paragraph::plain(prompt_text, s.ui(), Align::Center));

    let mut body_items = vec![title, t.gap(1, 18), prompt, t.gap(1, 28)];

    if v.mode == PinMode::Manage {
        let change_btn = action_button(
            &mut t,
            s,
            tr(Msg::DeviceSettingsDevPinChange),
            action::CHANGE_PIN,
            true,
        );
        let disable_btn = action_button(
            &mut t,
            s,
            tr(Msg::DeviceSettingsDevPinDisable),
            action::DISABLE_PIN,
            false,
        );
        let cancel_btn = action_button(
            &mut t,
            s,
            tr(Msg::DeviceSettingsDevPinCancel),
            action::CANCEL,
            false,
        );
        body_items.push(change_btn);
        body_items.push(t.gap(1, 24));
        body_items.push(disable_btn);
        body_items.push(t.gap(1, 24));
        body_items.push(cancel_btn);
    } else {
        // 4-dot PIN indicator row.
        let mut dots = String::with_capacity(PIN_LEN * 4);
        let filled = v.entered_len.min(PIN_LEN);
        for i in 0..PIN_LEN {
            if i > 0 {
                dots.push_str("   ");
            }
            dots.push(if i < filled { '●' } else { '○' });
        }
        let dots_node = t.text(Paragraph::plain(&dots, s.title(), Align::Center));
        body_items.push(dots_node);
        body_items.push(t.gap(1, 18));

        if let Some(err) = v.error {
            let err_node = t.text(Paragraph::plain(err, s.ui(), Align::Center));
            body_items.push(err_node);
            body_items.push(t.gap(1, 18));
        } else {
            body_items.push(t.gap(1, 40));
        }

        // 3x4 numeric keypad.
        let rows: [[(String, ActionId); 3]; 4] = [
            [
                ("1".into(), action::DIGIT_BASE + 1),
                ("2".into(), action::DIGIT_BASE + 2),
                ("3".into(), action::DIGIT_BASE + 3),
            ],
            [
                ("4".into(), action::DIGIT_BASE + 4),
                ("5".into(), action::DIGIT_BASE + 5),
                ("6".into(), action::DIGIT_BASE + 6),
            ],
            [
                ("7".into(), action::DIGIT_BASE + 7),
                ("8".into(), action::DIGIT_BASE + 8),
                ("9".into(), action::DIGIT_BASE + 9),
            ],
            [
                (tr(Msg::DeviceSettingsDevPinClear).into(), action::CLEAR),
                ("0".into(), action::DIGIT_BASE),
                (
                    tr(Msg::DeviceSettingsDevPinBackspace).into(),
                    action::BACKSPACE,
                ),
            ],
        ];

        let mut keypad_rows = Vec::with_capacity(4);
        for row_spec in rows {
            let mut row_cells = Vec::with_capacity(3);
            for (label, act) in row_spec {
                let lbl = t.label(&label, s.item());
                let center = t.add(Node::new(Kind::Align { x: 0.5, y: 0.5 }), [lbl]);
                let cell = t.add(
                    Node::new(Kind::Fixed {
                        w: Some(164),
                        h: Some(100),
                    })
                    .border(2, INK, 16)
                    .bg(PAPER)
                    .action(act),
                    [center],
                );
                row_cells.push(cell);
            }
            let row = t.add(Node::row(18).cross(Cross::Center), row_cells);
            keypad_rows.push(row);
        }
        let keypad = t.add(Node::column(18).cross(Cross::Center), keypad_rows);
        body_items.push(keypad);

        if v.mode != PinMode::Unlock {
            body_items.push(t.gap(1, 28));
            let cancel_btn = action_button(
                &mut t,
                s,
                tr(Msg::DeviceSettingsDevPinCancel),
                action::CANCEL,
                false,
            );
            body_items.push(cancel_btn);
        }
    }

    let col = t.add(Node::column(0).cross(Cross::Center), body_items);
    let centered = t.add(Node::new(Kind::Align { x: 0.5, y: 0.45 }), [col]);
    let body = t.add(
        Node::new(Kind::Padding(Insets::vh(24, 40))).flex(1),
        [centered],
    );
    let page = t.add(
        Node::column(0).cross(Cross::Stretch).bg(PAPER),
        [status, sep, body],
    );
    t.set_root(page);
    t
}

fn action_button(t: &mut Tree, s: &Styles, label: &str, act: ActionId, primary: bool) -> NodeId {
    let lbl = t.label(label, s.ui());
    let center = t.add(Node::new(Kind::Align { x: 0.5, y: 0.5 }), [lbl]);
    t.add(
        Node::new(Kind::Fixed {
            w: Some(420),
            h: Some(92),
        })
        .border(2, INK, 16)
        .bg(PAPER)
        .inverted(primary)
        .action(act),
        [center],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use crate::text::{FamilyId, Fonts, TextEngine};
    use crate::ui::Ui;

    fn status() -> StatusLine<'static> {
        StatusLine {
            clock: "21:38",
            battery: Some(0.84),
            charging: false,
        }
    }

    fn find_action(tree: &Tree, target: ActionId) -> Option<NodeId> {
        (0..tree.len() as u32)
            .map(NodeId)
            .find(|&id| tree.node(id).action == Some(target))
    }

    #[test]
    fn keypad_builds_and_all_digits_are_hittable() {
        let text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(FamilyId(0));
        let tree = build(
            &s,
            &PinLockView {
                status: status(),
                mode: PinMode::SetNew,
                entered_len: 2,
                error: None,
            },
        );
        let mut ui = Ui::new(tree, text, 1072, 1448);
        ui.render();

        let bounds = ui.bounds();
        for digit in 0..=9 {
            let act = action::DIGIT_BASE + digit;
            let node =
                find_action(&ui.tree, act).unwrap_or_else(|| panic!("missing digit {digit}"));
            let rect = ui.tree.rect(node);
            assert!(rect.w > 0 && rect.h > 0, "digit {digit} has empty rect");
            assert!(
                rect.x >= 0
                    && rect.y >= 0
                    && rect.right() <= bounds.right()
                    && rect.bottom() <= bounds.bottom(),
                "digit {digit} out of bounds: {rect:?}"
            );
            let hit = ui.tree.hit(Point {
                x: rect.x + rect.w / 2,
                y: rect.y + rect.h / 2,
            });
            assert_eq!(
                hit,
                crate::tree::Hit::Target(node),
                "digit {digit} not hittable"
            );
        }
        assert!(find_action(&ui.tree, action::CLEAR).is_some());
        assert!(find_action(&ui.tree, action::BACKSPACE).is_some());
        assert!(find_action(&ui.tree, action::CANCEL).is_some());
    }

    #[test]
    fn unlock_mode_hides_cancel_button() {
        let text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(FamilyId(0));
        let tree = build(
            &s,
            &PinLockView {
                status: status(),
                mode: PinMode::Unlock,
                entered_len: 0,
                error: Some("密码错误"),
            },
        );
        let mut ui = Ui::new(tree, text, 1072, 1448);
        ui.render();
        assert!(find_action(&ui.tree, action::CANCEL).is_none());
        assert!(find_action(&ui.tree, action::DIGIT_BASE).is_some());
    }

    #[test]
    fn manage_mode_exposes_change_and_disable_actions() {
        let text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(FamilyId(0));
        let tree = build(
            &s,
            &PinLockView {
                status: status(),
                mode: PinMode::Manage,
                entered_len: 0,
                error: None,
            },
        );
        let mut ui = Ui::new(tree, text, 1072, 1448);
        ui.render();
        assert!(find_action(&ui.tree, action::CHANGE_PIN).is_some());
        assert!(find_action(&ui.tree, action::DISABLE_PIN).is_some());
        assert!(find_action(&ui.tree, action::CANCEL).is_some());
        assert!(find_action(&ui.tree, action::DIGIT_BASE).is_none());
    }

    #[test]
    fn keypad_fits_compact_600x800_and_oasis_1264x1680_without_overlap() {
        for (w, h, dpi) in [(600u32, 800u32, 167u16), (1264, 1680, 300)] {
            let text = TextEngine::new(Fonts::embedded());
            let profile = crate::geom::Screen::new(w, h, dpi).unwrap();
            let s = Styles::new(FamilyId(0)).with_density(profile.density);
            let tree = build(
                &s,
                &PinLockView {
                    status: status(),
                    mode: PinMode::ConfirmNew,
                    entered_len: 3,
                    error: Some("两次输入不一致，请重新设置"),
                },
            );
            let mut ui = Ui::with_screen(tree, text, profile);
            ui.render();
            let bounds = ui.bounds();
            let mut rects = Vec::new();
            for digit in 0..=9 {
                let n = find_action(&ui.tree, action::DIGIT_BASE + digit).unwrap();
                let r = ui.tree.rect(n);
                assert!(r.w > 0 && r.h > 0, "{w}x{h} digit {digit} has empty rect");
                assert!(
                    r.x >= 0
                        && r.y >= 0
                        && r.right() <= bounds.right()
                        && r.bottom() <= bounds.bottom(),
                    "{w}x{h} digit {digit} out of bounds: {r:?}"
                );
                for prev in &rects {
                    assert!(
                        r.intersect(prev).is_none(),
                        "{w}x{h} digit {digit} overlaps another key: {r:?} vs {prev:?}"
                    );
                }
                rects.push(r);
            }
        }
    }
}
