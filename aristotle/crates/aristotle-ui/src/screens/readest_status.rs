// SPDX-License-Identifier: AGPL-3.0-or-later
//! Screen displaying Readest synchronization status, manual sync trigger,
//! server configuration, and logout.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size, MIN_TARGET};
use crate::icons::Icon;
use crate::text::{Align, Paragraph};
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_3, PAPER, RULE};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use aristotle_i18n::{tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    pub const BACK: ActionId = 1;
    pub const SYNC_NOW: ActionId = 10;
    pub const LOGOUT: ActionId = 11;
    /// Turn the automatic push (on close, sleep, quit) on or off.
    pub const AUTO_PUSH: ActionId = 12;
    pub const EDIT_API_BASE: ActionId = 20;
    pub const EDIT_SUPABASE_URL: ActionId = 21;
    pub const EDIT_ANON_KEY: ActionId = 22;
    pub const RESTORE_DEFAULT: ActionId = 23;
    pub const SIGN_IN: ActionId = 30;
}

/// View model for the Readest status screen.
pub struct ReadestStatusView<'a> {
    pub status: StatusLine<'a>,
    pub email: Option<&'a str>,
    pub last_sync: Option<&'a str>,
    pub pending_count: usize,
    pub is_syncing: bool,
    pub message: Option<&'a str>,
    /// Whether closing, sleeping or quitting pushes the open book.
    pub auto_push: bool,
    pub api_base: &'a str,
    pub supabase_url: &'a str,
    pub anon_key_masked: &'a str,
    /// Whether any endpoint is custom, so restoring the official defaults
    /// has something to do.
    pub show_restore_default: bool,
}

#[allow(clippy::too_many_arguments)]
fn row(
    t: &mut Tree,
    s: &Styles,
    icon: Option<Icon>,
    title: &str,
    detail: &str,
    detail_lines: usize,
    right: Option<NodeId>,
    id: Option<ActionId>,
) -> NodeId {
    let mut tp = Paragraph::plain(title, s.item(), Align::Left);
    tp.style.max_lines = Some(2);
    let mut words = vec![t.text(tp)];
    if !detail.is_empty() {
        let mut dp = Paragraph::plain(detail, s.small().line_height(1.2), Align::Left);
        dp.style.max_lines = Some(detail_lines);
        words.push(t.text(dp));
    }
    let words = t.add(Node::column(4).flex(1).cross(Cross::Start), words);
    let mut children = Vec::new();
    if let Some(i) = icon {
        children.push(t.icon(i, 32, INK));
    }
    children.push(words);
    children.extend(right);
    let r = t.add(Node::row(16), children);
    let r = t.pad(Insets::vh(14, 0), r);
    let line = t.rule(1, RULE);
    let item = t.add(Node::column(0), [r, line]);
    let mut n = Node::new(Kind::MinSize(Size::new(0, MIN_TARGET)));
    if let Some(id) = id {
        n = n.action(id).feedback(Feedback::Invert);
    }
    t.add(n, [item])
}

fn chevron(t: &mut Tree) -> NodeId {
    t.icon(Icon::ChevronRight, 28, INK_3)
}

/// A labelled switch: the whole row is the target (at least 64 px high), the
/// same shape the settings screen uses for its toggles.
fn toggle_row(
    t: &mut Tree,
    s: &Styles,
    title: &str,
    detail: &str,
    on: bool,
    id: ActionId,
) -> NodeId {
    let mut tp = Paragraph::plain(title, s.item(), Align::Left);
    tp.style.max_lines = Some(1);
    let mut words = vec![t.text(tp)];
    if !detail.is_empty() {
        let mut dp = Paragraph::plain(detail, s.small().line_height(1.2), Align::Left);
        dp.style.max_lines = Some(2);
        words.push(t.text(dp));
    }
    let words = t.add(Node::column(4).flex(1).cross(Cross::Start), words);
    let toggle = t.custom(crate::widgets::Toggle { on });
    let row = t.add(Node::row(16).cross(Cross::Center), [words, toggle]);
    let row = t.pad(Insets::vh(14, 0), row);
    let line = t.rule(1, RULE);
    let item = t.add(Node::column(0), [row, line]);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, MIN_TARGET)))
            .action(id)
            .feedback(Feedback::Invert),
        [item],
    )
}

/// Builds the Readest synchronization status screen.
pub fn build_readest_status_screen(t: &mut Tree, s: &Styles, v: &ReadestStatusView) -> NodeId {
    let bar = status_line(t, s, &v.status);
    let status_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

    // Header: Back button + Title "Readest 同步"
    let glyph = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(tr(Msg::ReadestTitle), s.font_lg().bold().line_height(1.2));
    let header = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    let header = t.fixed(None, Some(104), Some(header));
    let header_pad = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);

    let mut items = Vec::new();

    // 1. Account info card
    let is_logged_in = v.email.is_some();
    let account_title = v.email.unwrap_or(tr(Msg::ReadestAccountNotLoggedIn));
    let last_sync_text = match v.last_sync {
        Some(time) => tf!(Msg::ReadestLastSync, time = time),
        None => tr(Msg::ReadestNeverSynced).to_string(),
    };
    let pending_text = tf!(Msg::ReadestPendingItems, count = v.pending_count);
    let detail_line = format!("{last_sync_text} · {pending_text}");

    let c = if !is_logged_in {
        Some(chevron(t))
    } else {
        None
    };
    let account_action = if !is_logged_in {
        Some(action::SIGN_IN)
    } else {
        None
    };
    items.push(row(
        t,
        s,
        Some(Icon::User),
        account_title,
        &detail_line,
        2,
        c,
        account_action,
    ));

    // Message banner if any
    if let Some(msg) = v.message {
        let msg_para = Paragraph::plain(msg, s.small().gray(INK_2), Align::Left);
        let msg_node = t.text(msg_para);
        items.push(t.pad(Insets::vh(10, 0), msg_node));
    }

    // 2. Action button: "立即同步" (Sync now)
    if is_logged_in {
        let sync_text = if v.is_syncing {
            tr(Msg::ReadestSyncing)
        } else {
            tr(Msg::ReadestSyncNow)
        };
        let sync_icon = t.icon(Icon::Sync, 32, PAPER);
        let sync_label = t.label(sync_text, s.font_base().bold().gray(PAPER));
        let sync_content = t.add(Node::row(12).cross(Cross::Center), [sync_icon, sync_label]);
        let sync_center = t.align(0.5, 0.5, sync_content);
        let sync_btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(80),
            })
            .bg(INK)
            .border(0, INK, 20)
            .action(action::SYNC_NOW)
            .feedback(Feedback::Invert),
            [sync_center],
        );
        items.push(t.gap(0, 16));
        items.push(sync_btn);
        // The automatic push, right under the manual one, so the pair reads as
        // "sync by hand" and "keep it synced by itself".
        items.push(toggle_row(
            t,
            s,
            tr(Msg::ReadestAutoPush),
            tr(Msg::ReadestAutoPushHint),
            v.auto_push,
            action::AUTO_PUSH,
        ));
        items.push(t.gap(0, 24));
    }

    // 3. Settings / Endpoints Section
    let config_label = t.label(tr(Msg::SettingsSectionNetworkSync), s.font_sm().bold());
    items.push(t.pad(Insets::new(12, 0, 8, 0), config_label));

    let c = chevron(t);
    items.push(row(
        t,
        s,
        None,
        tr(Msg::ReadestApiBase),
        v.api_base,
        1,
        Some(c),
        Some(action::EDIT_API_BASE),
    ));

    let c = chevron(t);
    items.push(row(
        t,
        s,
        None,
        tr(Msg::ReadestSupabaseUrl),
        v.supabase_url,
        1,
        Some(c),
        Some(action::EDIT_SUPABASE_URL),
    ));

    let c = chevron(t);
    items.push(row(
        t,
        s,
        None,
        tr(Msg::ReadestSupabaseAnonKey),
        v.anon_key_masked,
        1,
        Some(c),
        Some(action::EDIT_ANON_KEY),
    ));

    if v.show_restore_default {
        items.push(t.gap(0, 12));
        let restore_label = t.label(
            tr(Msg::ReadestRestoreDefault),
            s.font_base().bold().gray(INK),
        );
        let restore_center = t.align(0.5, 0.5, restore_label);
        let restore_btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(80),
            })
            .bg(CARD)
            .border(2, INK, 20)
            .action(action::RESTORE_DEFAULT)
            .feedback(Feedback::Invert),
            [restore_center],
        );
        items.push(restore_btn);
    }

    // 4. Logout button (if signed in)
    if is_logged_in {
        items.push(t.gap(0, 28));
        let logout_label = t.label(tr(Msg::ReadestLogoutBtn), s.font_base().bold().gray(INK));
        let logout_center = t.align(0.5, 0.5, logout_label);
        let logout_btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(80),
            })
            .bg(CARD)
            .border(2, INK, 20)
            .action(action::LOGOUT)
            .feedback(Feedback::Invert),
            [logout_center],
        );
        items.push(logout_btn);
    }

    let col = t.add(Node::column(0).flex(1), items);
    let pad = t.pad(Insets::new(12, home_margin(), 24, home_margin()), col);

    t.add(Node::column(0), [status_pad, header_pad, pad])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::FamilyId;

    fn view(show_restore_default: bool) -> ReadestStatusView<'static> {
        ReadestStatusView {
            status: StatusLine {
                clock: "12:00",
                battery: None,
                charging: false,
            },
            email: None,
            last_sync: None,
            pending_count: 0,
            is_syncing: false,
            message: None,
            auto_push: true,
            api_base: "https://web.readest.com/api",
            supabase_url: "https://auth-cf.readest.com",
            anon_key_masked: "••••••••",
            show_restore_default,
        }
    }

    fn has_action(t: &Tree, action: ActionId) -> bool {
        (0..t.len())
            .map(|i| crate::tree::NodeId(i as u32))
            .any(|id| t.node(id).action == Some(action))
    }

    #[test]
    fn the_automatic_push_switch_is_present_only_when_signed_in() {
        let styles = Styles::new(FamilyId(0));
        // Signed in: the switch is there, and its state comes from the view.
        let mut v = view(false);
        v.email = Some("reader@example.com");
        let mut tree = Tree::new();
        build_readest_status_screen(&mut tree, &styles, &v);
        assert!(has_action(&tree, action::AUTO_PUSH));
        assert_eq!(toggle_states(&tree), vec![true]);
        v.auto_push = false;
        let mut tree = Tree::new();
        build_readest_status_screen(&mut tree, &styles, &v);
        assert_eq!(toggle_states(&tree), vec![false]);

        // Signed out: nothing to push automatically.
        let mut tree = Tree::new();
        build_readest_status_screen(&mut tree, &styles, &view(false));
        assert!(!has_action(&tree, action::AUTO_PUSH));
    }

    /// The state of every [`crate::widgets::Toggle`] in the tree.
    fn toggle_states(t: &Tree) -> Vec<bool> {
        (0..t.len())
            .map(|i| crate::tree::NodeId(i as u32))
            .filter_map(|id| t.widget::<crate::widgets::Toggle>(id).map(|w| w.on))
            .collect()
    }

    #[test]
    fn restore_default_shows_only_with_custom_endpoints() {
        let styles = Styles::new(FamilyId(0));
        for signed_out in [true, false] {
            let mut v = view(true);
            if !signed_out {
                v.email = Some("reader@example.com");
            }
            let mut tree = Tree::new();
            build_readest_status_screen(&mut tree, &styles, &v);
            assert!(
                has_action(&tree, action::RESTORE_DEFAULT),
                "custom endpoints must offer restoring the defaults"
            );

            let mut default_view = view(false);
            default_view.email = v.email;
            let mut tree = Tree::new();
            build_readest_status_screen(&mut tree, &styles, &default_view);
            assert!(
                !has_action(&tree, action::RESTORE_DEFAULT),
                "all-default endpoints need no restore button"
            );
        }
    }
}
