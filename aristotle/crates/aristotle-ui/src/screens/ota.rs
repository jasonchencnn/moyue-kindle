// SPDX-License-Identifier: AGPL-3.0-or-later
//! The software update page: the version that is installed, the one on the
//! server, and the way from one to the other.
//!
//! The page shows the update as it happens: the bar grows while the package
//! comes in, the numbers say how many bytes have arrived, and the button
//! offers the step that comes next — look for an update, download it, install
//! it, or try again.

use super::icon_button;
use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::TextEngine;
use crate::theme::{margin, IconSize, Space, Styles, CARD, INK, INK_MID, PAPER};
use crate::tree::{Cross, Feedback, Kind, Node, NodeId, Tree};
use aristotle_i18n::{fmt_bytes, tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// The way back to the page the update was opened from.
    pub const BACK: ActionId = 1;
    /// The button that does the step that comes next.
    pub const PRIMARY: ActionId = 2;
}

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 96;
/// The height of a button, in reference pixels.
const BTN_H: i32 = 76;
/// The height of the bar, in reference pixels.
const BAR_H: i32 = 12;

/// Where the update stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Nothing has been asked for yet.
    Idle,
    /// The manifest is being read.
    Checking,
    /// A newer version is on the server.
    Available,
    /// The installed version is the newest one.
    UpToDate,
    /// The server has an older version: no downgrade.
    Downgrade,
    /// The package is on its way.
    Downloading,
    /// The package's size and digest are being checked.
    Verifying,
    /// The verified package is being put in place.
    Staging,
    /// The package is ready; the reader can hand it over and stop.
    ReadyToApply,
    /// Something went wrong.
    Failed,
}

/// What the update page shows.
pub struct Ota<'a> {
    pub status: StatusLine<'a>,
    /// The version that is installed.
    pub current_version: String,
    /// The version the server has, when the check found one.
    pub remote_version: Option<String>,
    pub stage: Stage,
    /// How far the download has come, from 0 to 100.
    pub percent: u8,
    /// The bytes that have arrived.
    pub downloaded_bytes: u64,
    /// The size of the package, when it is known.
    pub total_bytes: Option<u64>,
    /// What the last step said, when it had something to say.
    pub message: Option<String>,
}

/// The nodes of the card that shows how far the update has come.
///
/// They come as one value with named fields: a tuple of five nodes is easy
/// to take apart in the wrong order.
pub struct Progress {
    /// The card itself.
    pub card: NodeId,
    /// The line that says what the update is doing.
    pub state: NodeId,
    /// The number of the bar.
    pub percent: NodeId,
    /// The bytes under the bar.
    pub bytes: NodeId,
    /// The bar itself.
    pub bar: NodeId,
}

/// Handles to the nodes of the update page.
pub struct OtaUi {
    /// The title of the page.
    pub title: NodeId,
    /// The line that says what the update is doing.
    pub state: NodeId,
    /// The number of the bar.
    pub percent: NodeId,
    /// The bytes under the bar.
    pub bytes: NodeId,
    /// The bar itself.
    pub bar: NodeId,
    /// The loading area in the middle of the status line, hidden.
    pub loading: LoadingNodes,
}

impl OtaUi {
    /// Builds the update page.
    pub fn build(_text: &mut TextEngine, s: &Styles, ota: &Ota) -> (Tree, Self) {
        let size = Size::new(
            s.density.px(crate::geom::SCREEN_WIDTH),
            s.density.px(crate::geom::SCREEN_HEIGHT),
        );
        Self::build_for_size(_text, s, ota, size)
    }

    /// Builds the update page for a screen of `size` device pixels.
    pub fn build_for_size(
        _text: &mut TextEngine,
        s: &Styles,
        ota: &Ota,
        _size: Size,
    ) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &ota.status);

        let back = icon_button(&mut t, Icon::ArrowLeft, IconSize::STANDARD, action::BACK);
        let title = t.label(tr(Msg::OtaTitle), s.display());
        let g = t.gap(Space::S, 1);
        let header = t.add(Node::row(0).cross(Cross::Center), [back, g, title]);
        let header = t.fixed(None, Some(HEADER_H), Some(header));

        let versions = versions_card(&mut t, s, ota);
        let progress = progress_card(&mut t, s, ota);

        let mut parts = vec![bar.root, header, versions, t.gap(0, Space::M)];
        parts.push(progress.card);
        parts.push(t.gap(0, Space::S));
        if let Some((label, id)) = primary_action(ota) {
            let (button, _) = button(&mut t, s, label, id);
            parts.push(button);
            parts.push(t.gap(0, Space::S));
        }
        let (back_button, _) = button(&mut t, s, tr(Msg::OtaBack), action::BACK);
        parts.push(back_button);

        let col = t.add(Node::column(0), parts);
        let col = t.pad(Insets::new(0, margin(), 0, margin()), col);
        let col = t.add(Node::column(0).flex(1), [col]);
        t.set_root(col);
        (
            t,
            OtaUi {
                title,
                state: progress.state,
                percent: progress.percent,
                bytes: progress.bytes,
                bar: progress.bar,
                loading: bar.loading,
            },
        )
    }
}

/// The card that holds the two versions.
fn versions_card(t: &mut Tree, s: &Styles, ota: &Ota) -> NodeId {
    let mut rows = vec![version_row(
        t,
        s,
        tr(Msg::OtaCurrentVersion),
        &ota.current_version,
    )];
    if let Some(remote) = &ota.remote_version {
        rows.push(version_row(t, s, tr(Msg::OtaRemoteVersion), remote));
    }
    let col = t.add(Node::column(Space::S), rows);
    card(t, col)
}

/// A row of the version card: the name on the left, the version on the right.
fn version_row(t: &mut Tree, s: &Styles, name: &str, version: &str) -> NodeId {
    let label = t.label(name, s.small().gray(INK_MID));
    let label = t.fixed(Some(220), None, Some(label));
    let value = t.label(version, s.small().bold());
    let value = t.add(Node::column(0).flex(1), [value]);
    t.add(Node::row(Space::S).cross(Cross::Start), [label, value])
}

/// The card that holds the state of the update: what it is doing, the bar and
/// the numbers.
fn progress_card(t: &mut Tree, s: &Styles, ota: &Ota) -> Progress {
    let state_text = state_message(ota);
    let state = t.label(&state_text, s.small().line_height(1.3));

    let bar = t.custom(crate::screens::library::RoundedBar {
        value: ota.percent as f32 / 100.0,
        height: BAR_H,
        track_gray: PAPER,
    });
    let bar = t.add(Node::column(0).flex(1), [bar]);
    let bar = t.fixed(None, Some(BAR_H), Some(bar));

    let percent = t.label(&format!("{}%", ota.percent), s.small().bold());
    let bytes = t.label(&bytes_line(ota), s.small().gray(INK_MID));
    let row = t.add(Node::row(Space::S).cross(Cross::Center), [percent, bytes]);
    let row = t.fixed(None, Some(40), Some(row));

    let col = t.add(Node::column(Space::S), [state, bar, row]);
    Progress {
        card: card(t, col),
        state,
        percent,
        bytes,
        bar,
    }
}

/// The button that does the step that comes next, and the one that goes back.
fn button(t: &mut Tree, s: &Styles, label: &str, id: crate::tree::ActionId) -> (NodeId, NodeId) {
    let text = t.label(label, s.small().bold());
    let content = t.add(Node::row(Space::S).cross(Cross::Center), [text]);
    let button = t.add(
        Node::new(Kind::MinSize(Size::new(0, BTN_H)))
            .border(2, INK, 20)
            .flex(1)
            .action(id)
            .feedback(Feedback::Invert),
        [content],
    );
    (button, text)
}

/// A card: the content on the paper, with room around it.
fn card(t: &mut Tree, content: NodeId) -> NodeId {
    t.add(
        Node::new(Kind::Padding(Insets::new(
            Space::M,
            Space::M,
            Space::M,
            Space::M,
        )))
        .bg(CARD)
        .border(0, CARD, 24),
        [content],
    )
}

/// The button that does the step that comes next, when there is one.
fn primary_action(ota: &Ota) -> Option<(&'static str, crate::tree::ActionId)> {
    match ota.stage {
        Stage::Idle | Stage::UpToDate | Stage::Downgrade => {
            Some((tr(Msg::OtaCheck), action::PRIMARY))
        }
        Stage::Available => Some((tr(Msg::OtaDownload), action::PRIMARY)),
        Stage::ReadyToApply => Some((tr(Msg::OtaApply), action::PRIMARY)),
        Stage::Failed => Some((tr(Msg::OtaRetry), action::PRIMARY)),
        Stage::Checking | Stage::Downloading | Stage::Verifying | Stage::Staging => None,
    }
}

/// What the update is doing, in one line.
fn state_message(ota: &Ota) -> String {
    if let Some(message) = &ota.message {
        return message.clone();
    }
    match ota.stage {
        Stage::Idle => tr(Msg::OtaIdle).to_string(),
        Stage::Checking => tr(Msg::OtaChecking).to_string(),
        Stage::Available => match &ota.remote_version {
            Some(version) => tf!(Msg::OtaAvailableVersion, version = version),
            None => tr(Msg::OtaAvailable).to_string(),
        },
        Stage::UpToDate => tr(Msg::OtaUpToDate).to_string(),
        Stage::Downgrade => tr(Msg::OtaDowngrade).to_string(),
        Stage::Downloading => tr(Msg::OtaDownloading).to_string(),
        Stage::Verifying => tr(Msg::OtaVerifying).to_string(),
        Stage::Staging => tr(Msg::OtaStaging).to_string(),
        Stage::ReadyToApply => tr(Msg::OtaReady).to_string(),
        Stage::Failed => tr(Msg::OtaFailed).to_string(),
    }
}

/// The bytes that have arrived, and the ones that are still to come.
fn bytes_line(ota: &Ota) -> String {
    let done = fmt_bytes(ota.downloaded_bytes);
    match ota.total_bytes {
        Some(total) => tf!(Msg::OtaBytes, done = done, total = fmt_bytes(total)),
        None => done,
    }
}
