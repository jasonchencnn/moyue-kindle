// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Nutstore (Jianguoyun) shelf: the files in the reader's Nutstore
//! account, browsable folder by folder over WebDAV.
//!
//! It shares the library's frame: the status line, the title, the
//! "Local / Readest / WeRead / Nutstore" segmented control, the account card
//! with the current remote path, the parent-folder button, the refresh
//! button, the filter chips, a paginated list of folders and books, and the
//! pager.
//!
//! The screen is a view over data the app owns. Paging only moves the page
//! of the already-built list: it never reads a file, dials the network or
//! takes a lock (the app fetches the folder when the screen opens or the
//! reader asks for it).

use super::library::{
    list_insets, page_of, segment_control_channels, status_line, ChannelVisibility, Segment,
    StatusLine,
};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::input::Direction;
use crate::text::{Align, Paragraph};
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_3, INK_MID, PAPER, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::Cover;
use aristotle_i18n::{fmt_bytes, tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// The "local" cell of the segmented control.
    pub const LOCAL: ActionId = 1;
    /// The "Readest" cell of the segmented control.
    pub const CLOUD: ActionId = 2;
    /// The "WeRead" cell of the segmented control.
    pub const WEREAD: ActionId = 3;
    /// Ask the app for the folder again.
    pub const SYNC: ActionId = 4;
    /// The guide shown when the account is not configured.
    pub const LOGIN: ActionId = 5;
    /// Sign out of the current Nutstore account so the user can switch.
    pub const LOGOUT: ActionId = 6;
    /// Open the account settings (change the server, the account, the folder).
    pub const CONFIGURE: ActionId = 7;
    /// `FILTER + i` selects `JianguoyunFilter::ALL[i]`.
    pub const FILTER: ActionId = 10;
    pub const PREV_PAGE: ActionId = 20;
    pub const NEXT_PAGE: ActionId = 21;
    /// Step up to the parent folder.
    pub const PARENT: ActionId = 70;
    /// Stride between per-row action ranges so shelves with hundreds of items
    /// never collide across actions.
    pub const ROW_STRIDE: ActionId = 10_000;
    /// `DOWNLOAD + i` downloads (or retries) the book at row `i`.
    pub const DOWNLOAD: ActionId = 100;
    /// `CANCEL + i` stops the download of the book at row `i`.
    pub const CANCEL: ActionId = 10_100;
    /// `OPEN + i` opens the downloaded book at row `i`.
    pub const OPEN: ActionId = 20_100;
    /// `ENTER + i` opens the folder at row `i`.
    pub const ENTER: ActionId = 30_100;
}

/// Which entries the shelf shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JianguoyunFilter {
    #[default]
    All,
    /// Only the books that are not on the device yet.
    NotDownloaded,
    /// Only the books that are already in the library.
    Downloaded,
}

impl JianguoyunFilter {
    pub const ALL: [JianguoyunFilter; 3] = [
        JianguoyunFilter::All,
        JianguoyunFilter::NotDownloaded,
        JianguoyunFilter::Downloaded,
    ];

    pub fn label(self) -> &'static str {
        tr(match self {
            JianguoyunFilter::All => Msg::JianguoyunFilterAll,
            JianguoyunFilter::NotDownloaded => Msg::JianguoyunFilterNotDownloaded,
            JianguoyunFilter::Downloaded => Msg::JianguoyunFilterDownloaded,
        })
    }

    /// Folders are always shown: they are the way into the account.
    pub fn accepts(self, entry: &JianguoyunEntry) -> bool {
        if entry.is_dir {
            return true;
        }
        match self {
            JianguoyunFilter::All => true,
            JianguoyunFilter::NotDownloaded => {
                !matches!(entry.download, JianguoyunDownloadState::Downloaded)
            }
            JianguoyunFilter::Downloaded => {
                matches!(entry.download, JianguoyunDownloadState::Downloaded)
            }
        }
    }
}

/// What the row's download control shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JianguoyunDownloadState {
    /// The book is not on the device yet.
    #[default]
    NotDownloaded,
    /// A download is running, with the percentage reported by the pipeline.
    Downloading { percent: u8 },
    /// The EPUB is in the library.
    Downloaded,
    /// The last download stopped; the control offers a retry.
    Failed,
}

/// One entry of the current remote folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JianguoyunEntry {
    /// The remote path, relative to the account root; the row's identity.
    pub path: String,
    /// The file or folder name.
    pub name: String,
    /// A folder, not a book.
    pub is_dir: bool,
    /// The file size in bytes; a folder has none.
    pub size: u64,
    /// The book format ("EPUB"); empty for a folder.
    pub format: String,
    /// Download state shown on the row and its right-hand control.
    pub download: JianguoyunDownloadState,
}

impl JianguoyunEntry {
    /// A folder in the current listing.
    pub fn folder(path: &str, name: &str) -> Self {
        Self {
            path: path.to_string(),
            name: name.to_string(),
            is_dir: true,
            size: 0,
            format: String::new(),
            download: JianguoyunDownloadState::NotDownloaded,
        }
    }

    /// A book in the current listing.
    pub fn book(path: &str, name: &str, format: &str, size: u64) -> Self {
        Self {
            path: path.to_string(),
            name: name.to_string(),
            is_dir: false,
            size,
            format: format.to_string(),
            download: JianguoyunDownloadState::NotDownloaded,
        }
    }
}

/// Everything the Nutstore shelf shows.
pub struct JianguoyunShelfView<'a> {
    pub status: StatusLine<'a>,
    /// Whether the account is configured and signed in.
    pub configured: bool,
    /// The signed-in account, for the account card.
    pub account: Option<&'a str>,
    /// The remote folder being shown, relative to the account root; an empty
    /// string is the root.
    pub current_dir: &'a str,
    /// How many books the account holds.
    pub book_count: usize,
    /// The local library's count, for the segmented control.
    pub local_books_count: usize,
    /// When the folder was last fetched, in words.
    pub last_sync: Option<&'a str>,
    pub filter: JianguoyunFilter,
    pub loading: bool,
    /// A failure or a hint shown above the list.
    pub message: Option<&'a str>,
    /// The entries of the current folder, in the order the shelf draws them.
    pub entries: &'a [JianguoyunEntry],
}

/// Handles to the nodes the screen updates in place.
pub struct JianguoyunShelfUi {
    /// The list node, when it paginates by height.
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    /// Node of each row drawn on this page, for tests and the preview.
    pub rows: Vec<NodeId>,
    /// The remote path of each row, in action order.
    row_paths: Vec<String>,
    /// The entry behind each row, for in-place status updates.
    row_data: Vec<JianguoyunEntry>,
    /// The status text node of each row.
    row_status: Vec<NodeId>,
    /// The "parent folder" button, when the folder is not the root.
    pub parent: Option<NodeId>,
}

/// The height of a folder row: an icon and one line of air.
pub const FOLDER_H: i32 = 112;
/// The height of a book row: a 108x144 cover and its two lines of air.
pub const BOOK_H: i32 = 188;
/// The gap between two rows on a page.
pub const ROW_GAP: i32 = 16;

fn badge(t: &mut Tree) -> NodeId {
    let icon = t.icon(Icon::Storage, 44, PAPER);
    let aligned = t.align(0.5, 0.5, icon);
    let badge = t.add(Node::column(0).bg(INK).border(0, 0, 20), [aligned]);
    t.fixed(Some(76), Some(76), Some(badge))
}

/// A small round button with a label, at the 9 mm touch target.
fn pill_button(t: &mut Tree, s: &Styles, label: &str, act: ActionId) -> NodeId {
    let label = t.label(label, s.font_xs().bold());
    let pad = t.pad(Insets::vh(0, 12), label);
    let button = t.button(Node::plain().border(2, INK_3, 20), pad, act);
    t.fixed(None, Some(72), Some(button))
}

fn filter_chips(t: &mut Tree, s: &Styles, filter: JianguoyunFilter) -> NodeId {
    let mut chips = Vec::new();
    for (i, f) in JianguoyunFilter::ALL.iter().enumerate() {
        chips.push(crate::components::chip(
            t,
            s,
            f.label(),
            *f == filter,
            action::FILTER + i as ActionId,
        ));
    }
    t.add(Node::row(16).cross(Cross::Center), chips)
}

/// The path of the current folder, as the card shows it.
fn path_label(view: &JianguoyunShelfView) -> String {
    if view.current_dir.trim().is_empty() {
        tr(Msg::JianguoyunRootDir).to_string()
    } else {
        format!("/{}", view.current_dir.trim_matches('/'))
    }
}

/// The account card: whose books these are, where they are, and the way
/// back, out and in. Returns the card and the parent-folder button, when
/// the folder being shown is not the root.
fn account_card(t: &mut Tree, s: &Styles, view: &JianguoyunShelfView) -> (NodeId, Option<NodeId>) {
    let badge = badge(t);
    let account_text = match view.account {
        Some(email) => email.to_string(),
        None => tf!(Msg::JianguoyunAccount, count = view.book_count),
    };
    let mut ap = Paragraph::plain(
        &account_text,
        s.font_sm().bold().line_height(1.25),
        Align::Left,
    );
    ap.style.max_lines = Some(1);
    let account = t.text(ap);
    let line = if let Some(message) = view.message {
        message.to_string()
    } else if let Some(time) = view.last_sync {
        tf!(Msg::JianguoyunSyncLine, time = time)
    } else {
        tr(Msg::JianguoyunSyncNever).to_string()
    };
    let mut lp = Paragraph::plain(
        &line,
        s.font_xs().gray(INK_2).line_height(1.25),
        Align::Left,
    );
    lp.style.max_lines = Some(1);
    let line = t.text(lp);
    let words = t.add(Node::column(2).flex(1), [account, line]);
    let setup = pill_button(t, s, tr(Msg::JianguoyunConfigure), action::CONFIGURE);
    let logout = pill_button(t, s, tr(Msg::JianguoyunSignOut), action::LOGOUT);
    let sync_icon = t.icon(Icon::Refresh, 28, PAPER);
    let sync_label = t.label(tr(Msg::JianguoyunRefresh), s.font_xs().bold().gray(PAPER));
    let sync_row = t.add(Node::row(8).cross(Cross::Center), [sync_icon, sync_label]);
    let sync_pad = t.pad(Insets::vh(0, 14), sync_row);
    let sync = t.button(
        Node::plain().bg(INK).border(0, 0, 20),
        sync_pad,
        action::SYNC,
    );
    let sync = t.fixed(None, Some(72), Some(sync));
    let top = t.add(
        Node::row(10).cross(Cross::Center),
        [badge, words, setup, logout, sync],
    );

    // The second row is dedicated to the current folder path and parent button
    // so `/Books` has plenty of horizontal room on 600x800 English panels.
    let folder_icon = t.icon(Icon::Folder, 32, INK_2);
    let mut pp = Paragraph::plain(
        path_label(view),
        s.font_xs().gray(INK).line_height(1.3),
        Align::Left,
    );
    pp.style.max_lines = Some(1);
    let path = t.text(pp);
    let path_words = t.add(Node::column(0).flex(1), [path]);
    let up_icon = t.icon(Icon::ChevronUp, 28, INK_2);
    let up_label = t.label(tr(Msg::JianguoyunParentDir), s.font_xs().bold());
    let up_row = t.add(Node::row(6).cross(Cross::Center), [up_icon, up_label]);
    let up_pad = t.pad(Insets::vh(0, 12), up_row);
    let up = if view.current_dir.trim().is_empty() {
        // At the root there is no parent to go to.
        let up = t.add(Node::plain().border(2, RULE, 20), [up_pad]);
        (t.fixed(None, Some(72), Some(up)), None)
    } else {
        let up = t.button(Node::plain().border(2, INK_3, 20), up_pad, action::PARENT);
        (t.fixed(None, Some(72), Some(up)), Some(up))
    };
    let (up, parent) = up;
    let bottom = t.add(
        Node::row(12).cross(Cross::Center),
        [folder_icon, path_words, up],
    );

    let card = t.add(Node::column(12), [top, bottom]);
    let pad = t.pad(Insets::new(16, 24, 16, 24), card);
    (
        t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]),
        parent,
    )
}

/// The row of a folder: its name and the way in.
fn folder_row(t: &mut Tree, s: &Styles, entry: &JianguoyunEntry, row: usize) -> NodeId {
    let icon = t.icon(Icon::Folder, 44, INK);
    let icon_box = t.fixed(Some(60), Some(60), Some(icon));
    let mut tp = Paragraph::plain(
        &entry.name,
        s.font_base().bold().line_height(1.25),
        Align::Left,
    );
    tp.style.max_lines = Some(1);
    let title = t.text(tp);
    let badge = t.label(tr(Msg::JianguoyunFolderBadge), s.font_xs().gray(INK_MID));
    let words = t.add(Node::column(4).cross(Cross::Start).flex(1), [title, badge]);
    let chevron = t.icon(Icon::ChevronRight, 32, INK_3);
    let row_node = t.add(
        Node::row(20).cross(Cross::Center),
        [icon_box, words, chevron],
    );
    let pad = t.pad(Insets::new(12, 24, 12, 24), row_node);
    let card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]);
    let card = t.fixed(None, Some(FOLDER_H), Some(card));
    t.button(Node::plain(), card, action::ENTER + row as u32)
}

/// The status line of a book row: its format, its size and its state.
fn book_meta(entry: &JianguoyunEntry) -> String {
    let format = if entry.format.trim().is_empty() {
        "—".to_string()
    } else {
        entry.format.trim().to_ascii_uppercase()
    };
    let meta = tf!(
        Msg::JianguoyunFileMeta,
        format = format,
        size = fmt_bytes(entry.size)
    );
    match entry.download {
        JianguoyunDownloadState::NotDownloaded => {
            format!("{meta} · {}", tr(Msg::JianguoyunNotDownloaded))
        }
        JianguoyunDownloadState::Downloading { percent } => {
            format!(
                "{meta} · {}",
                tf!(Msg::JianguoyunDownloading, percent = percent)
            )
        }
        JianguoyunDownloadState::Downloaded => {
            format!("{meta} · {}", tr(Msg::JianguoyunDownloaded))
        }
        JianguoyunDownloadState::Failed => {
            format!("{meta} · {}", tr(Msg::JianguoyunDownloadFailed))
        }
    }
}

/// The book's right-hand control: download, cancel, retry or open.
fn download_control(t: &mut Tree, s: &Styles, entry: &JianguoyunEntry, row: usize) -> NodeId {
    if matches!(entry.download, JianguoyunDownloadState::NotDownloaded) {
        let icon = t.icon(Icon::Download, 32, INK_2);
        let centered = t.align(0.5, 0.5, icon);
        let button = t.button(
            Node::new(Kind::MinSize(Size::new(72, 72))).border(2, INK_3, 20),
            centered,
            action::DOWNLOAD + row as u32,
        );
        return t.fixed(Some(72), Some(72), Some(button));
    }
    let (label, action) = match entry.download {
        JianguoyunDownloadState::NotDownloaded => unreachable!("handled above"),
        JianguoyunDownloadState::Downloading { .. } => (
            tr(Msg::JianguoyunDownloadCancel),
            action::CANCEL + row as u32,
        ),
        JianguoyunDownloadState::Downloaded => (tr(Msg::JianguoyunOpen), action::OPEN + row as u32),
        JianguoyunDownloadState::Failed => (
            tr(Msg::JianguoyunDownloadRetry),
            action::DOWNLOAD + row as u32,
        ),
    };
    let label = t.label(label, s.font_xs().bold().gray(INK_2));
    let pad = t.pad(Insets::vh(0, 18), label);
    let button = t.button(
        Node::new(Kind::MinSize(Size::new(88, 72))).border(2, INK_3, 20),
        pad,
        action,
    );
    t.fixed(None, Some(72), Some(button))
}

/// The row of a book: its cover, its name and its download control.
fn book_row(t: &mut Tree, s: &Styles, entry: &JianguoyunEntry, row: usize) -> (NodeId, NodeId) {
    let cover = Cover::new(&entry.name, &entry.format, s.family)
        .with_source_tag(super::library::BookSource::Jianguoyun.label());
    let cover_node = t.custom(cover);
    let cover_box = t.fixed(Some(108), Some(144), Some(cover_node));

    let mut tp = Paragraph::plain(
        &entry.name,
        s.font_base().bold().line_height(1.25),
        Align::Left,
    );
    tp.style.max_lines = Some(1);
    let title = t.text(tp);

    let mut mp = Paragraph::plain(
        book_meta(entry),
        s.font_xs().gray(INK_2).line_height(1.3),
        Align::Left,
    );
    mp.style.max_lines = Some(1);
    let meta = t.text(mp);

    let info = t.add(Node::column(8).cross(Cross::Start).flex(1), [title, meta]);
    let pill = download_control(t, s, entry, row);

    let row_node = t.add(Node::row(24).cross(Cross::Center), [cover_box, info, pill]);
    let pad = t.pad(Insets::new(12, 24, 12, 24), row_node);
    let card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]);
    let card = t.fixed(None, Some(BOOK_H), Some(card));
    // The row itself carries no action: the control on its right is the one
    // the reader taps, and two actions on the same pixels would overlap.
    (card, meta)
}

/// The guide shown when the account is not configured, and the empty-folder
/// notice.
fn guide(t: &mut Tree, s: &Styles, view: &JianguoyunShelfView) -> NodeId {
    let (title, hint) = if !view.configured {
        (
            tr(Msg::JianguoyunNotConfiguredTitle),
            Some(tr(Msg::JianguoyunNotConfiguredHint)),
        )
    } else if view.loading {
        (tr(Msg::JianguoyunLoading), None)
    } else if let Some(message) = view.message {
        (message, None)
    } else {
        (tr(Msg::JianguoyunEmpty), None)
    };
    let mut tp = Paragraph::plain(title, s.ui().bold().gray(INK), Align::Center);
    tp.style.max_lines = Some(3);
    let title = t.text(tp);
    let mut children = vec![title];
    if let Some(hint) = hint {
        let mut hp = Paragraph::plain(hint, s.small().gray(INK_2), Align::Center);
        hp.style.max_lines = Some(4);
        let hint = t.text(hp);
        children.push(t.gap(1, 16));
        children.push(hint);
    }
    if !view.configured {
        let label = t.label(tr(Msg::JianguoyunConfigure), s.ui_bold().gray(PAPER));
        let pad = t.pad(Insets::vh(0, 28), label);
        let button = t.add(
            Node::new(Kind::MinSize(Size::new(420, 80)))
                .bg(INK)
                .border(0, 0, 20)
                .action(action::LOGIN)
                .feedback(Feedback::Invert),
            [pad],
        );
        children.push(t.gap(1, 32));
        children.push(button);
    }
    let column = t.add(Node::column(16).cross(Cross::Center), children);
    let pad = t.pad(Insets::vh(crate::geom::mm(8.0), 24), column);
    t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad])
}

/// The entries shown for `filter`, in the order the shelf draws them.
fn shown_entries(entries: &[JianguoyunEntry], filter: JianguoyunFilter) -> Vec<&JianguoyunEntry> {
    entries.iter().filter(|e| filter.accepts(e)).collect()
}

impl<'a> JianguoyunShelfView<'a> {
    pub fn build(
        text: &mut crate::text::TextEngine,
        s: &Styles,
        view: &Self,
    ) -> (Tree, JianguoyunShelfUi) {
        Self::build_for_width(text, s, view, crate::geom::SCREEN_WIDTH)
    }

    /// Builds the shelf for a screen `width` px wide.
    pub fn build_for_width(
        text: &mut crate::text::TextEngine,
        s: &Styles,
        view: &Self,
        width: i32,
    ) -> (Tree, JianguoyunShelfUi) {
        Self::build_for_width_with_channels(
            text,
            s,
            view,
            width,
            ChannelVisibility::readest_and_weread(true, true),
        )
    }

    /// Builds the shelf for a screen `width` px wide, showing the cloud
    /// channels Settings turns on: Readest, WeRead and Nutstore.
    pub fn build_for_width_with_channels(
        text: &mut crate::text::TextEngine,
        s: &Styles,
        view: &Self,
        _width: i32,
        channels: ChannelVisibility,
    ) -> (Tree, JianguoyunShelfUi) {
        let _ = text;
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &view.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let mut column = vec![status];

        let title_text = tr(Msg::JianguoyunTitle);
        let title_style = s.font_lg().bold().line_height(1.2).tracking(4.0);
        let title = t.text(Paragraph::plain(title_text, title_style, Align::Left));
        let header = t.add(Node::row(0).cross(Cross::Center), [title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
        column.push(header);

        // Local / Readest / WeRead / Nutstore, in the same place and size as on
        // the library. This screen is the Nutstore shelf, so its own cell is
        // always on and takes no action.
        let channels = ChannelVisibility {
            jianguoyun: true,
            ..channels
        };
        let seg = segment_control_channels(
            &mut t,
            s,
            view.local_books_count,
            Segment::Jianguoyun,
            Some(action::LOCAL),
            channels.readest.then_some(action::CLOUD),
            channels.weread.then_some(action::WEREAD),
            None,
            channels,
        );
        let seg = t.fixed(None, Some(76), Some(seg));
        let seg = t.pad(Insets::new(0, home_margin(), 28, home_margin()), seg);
        column.push(seg);

        let mut rows = Vec::new();
        let mut row_paths = Vec::new();
        let mut row_data = Vec::new();
        let mut row_status = Vec::new();
        let mut list = NodeId(0);
        let mut parent = None;

        if view.configured {
            let (card, parent_button) = account_card(&mut t, s, view);
            let card = t.pad(Insets::new(24, home_margin(), 0, home_margin()), card);
            column.push(card);
            parent = parent_button;

            let chips = filter_chips(&mut t, s, view.filter);
            let chips = t.pad(Insets::new(24, home_margin(), 0, home_margin()), chips);
            column.push(chips);

            // Only the rows of the current folder are built: a Nutstore
            // account can hold thousands of files, and the screen is a view
            // over the folder the app fetched.
            let shown = shown_entries(view.entries, view.filter);
            let shelf = if shown.is_empty() {
                let empty = guide(&mut t, s, view);
                let empty = t.pad(list_insets(), empty);
                t.add(Node::column(0).flex(1), [empty])
            } else {
                for (row, entry) in shown.iter().enumerate() {
                    if entry.is_dir {
                        rows.push(folder_row(&mut t, s, entry, row));
                        row_status.push(NodeId(0));
                    } else {
                        let (card, status) = book_row(&mut t, s, entry, row);
                        rows.push(card);
                        row_status.push(status);
                    }
                    row_paths.push(entry.path.clone());
                    row_data.push((*entry).clone());
                }
                let pages_node = t.add(pages(ROW_GAP).flex(1), rows.clone());
                list = pages_node;
                t.add(
                    Node::new(Kind::Padding(list_insets())).flex(1),
                    [pages_node],
                )
            };
            column.push(shelf);
        } else {
            // Without an account the screen is the guide and its button.
            let empty = guide(&mut t, s, view);
            let empty = t.pad(list_insets(), empty);
            let empty = t.add(Node::column(0).flex(1), [empty]);
            column.push(empty);
        }

        let prev = super::icon_button(&mut t, Icon::ChevronLeft, 44, action::PREV_PAGE);
        let next = super::icon_button(&mut t, Icon::ChevronRight, 44, action::NEXT_PAGE);
        let pager_label = t.text(Paragraph::plain(
            page_of(1, 1),
            s.font_sm().line_height(1.3),
            Align::Center,
        ));
        let hint = t.text(Paragraph::plain(
            tr(Msg::LibraryPagerHintScroll),
            s.font_xs().gray(INK_MID).line_height(1.3),
            Align::Center,
        ));
        let center = t.add(Node::column(0).cross(Cross::Center), [pager_label, hint]);
        let center = t.add(Node::column(0).cross(Cross::Center).flex(1), [center]);
        let pager_row = t.add(Node::row(0).cross(Cross::Center), [prev, center, next]);
        let pager = t.fixed(None, Some(104), Some(pager_row));
        let pager = t.pad(Insets::new(0, home_margin(), 0, home_margin()), pager);
        column.push(pager);

        let col = t.add(Node::column(0).flex(1), column);
        let nav = crate::nav::build(&mut t, s, crate::nav::Tab::Library);
        let content = t.add(Node::column(0), [col, nav.root]);
        t.set_root(content);

        let ui = JianguoyunShelfUi {
            list,
            pager_label,
            prev: t.node(prev).children[0],
            next: t.node(next).children[0],
            rows,
            row_paths,
            row_data,
            row_status,
            parent,
        };
        (t, ui)
    }
}

impl JianguoyunShelfUi {
    /// Updates the pager after layout: page label and arrow states.
    pub fn sync(&self, t: &mut Tree) {
        let (page, count) = if self.list == NodeId(0) {
            (0, 1)
        } else {
            t.page(self.list)
        };
        let count = count.max(1);
        t.set_text(self.pager_label, &page_of(page + 1, count));
        t.set_icon_gray(self.prev, if page == 0 { RULE } else { INK });
        t.set_icon_gray(self.next, if page + 1 >= count { RULE } else { INK });
    }

    /// The remote path a row action belongs to, if it belongs to this page.
    pub fn entry_at(&self, id: ActionId) -> Option<&str> {
        let s = action::ROW_STRIDE;
        let row = if (action::DOWNLOAD..action::DOWNLOAD + s).contains(&id) {
            id - action::DOWNLOAD
        } else if (action::CANCEL..action::CANCEL + s).contains(&id) {
            id - action::CANCEL
        } else if (action::OPEN..action::OPEN + s).contains(&id) {
            id - action::OPEN
        } else if (action::ENTER..action::ENTER + s).contains(&id) {
            id - action::ENTER
        } else {
            return None;
        };
        self.row_paths.get(row as usize).map(String::as_str)
    }

    /// Update one row's status line in place: a download tick does not
    /// rebuild the page or touch the disk.
    pub fn sync_download(&self, t: &mut Tree, path: &str, state: JianguoyunDownloadState) {
        let Some(row) = self.row_paths.iter().position(|p| p == path) else {
            return;
        };
        let Some(entry) = self.row_data.get(row) else {
            return;
        };
        let mut entry = entry.clone();
        entry.download = state;
        if let Some(node) = self.row_status.get(row) {
            if *node != NodeId(0) {
                t.set_text(*node, &book_meta(&entry));
            }
        }
    }

    /// Applies the pager actions. Returns true when handled.
    pub fn handle(&mut self, t: &mut Tree, id: ActionId) -> bool {
        if self.list == NodeId(0) {
            return false;
        }
        let (page, _) = t.page(self.list);
        match id {
            action::PREV_PAGE => {
                t.set_page(self.list, page.saturating_sub(1));
            }
            action::NEXT_PAGE => {
                t.set_page(self.list, page + 1);
            }
            _ => return false,
        }
        true
    }

    /// Swipe pages the list. Returns true when handled.
    pub fn handle_swipe(&mut self, t: &mut Tree, dir: Direction) -> bool {
        if self.list == NodeId(0) {
            return false;
        }
        let (page, _) = t.page(self.list);
        match dir {
            Direction::Up | Direction::Left => {
                t.set_page(self.list, page + 1);
            }
            Direction::Down | Direction::Right => {
                t.set_page(self.list, page.saturating_sub(1));
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
    use crate::text::{FamilyId, Fonts, TextEngine};
    use crate::ui::Ui;
    use aristotle_i18n::with_language;

    fn book(path: &str, name: &str) -> JianguoyunEntry {
        JianguoyunEntry::book(path, name, "EPUB", 1_048_576)
    }

    fn view<'a>(entries: &'a [JianguoyunEntry], configured: bool) -> JianguoyunShelfView<'a> {
        JianguoyunShelfView {
            status: StatusLine {
                clock: "12:00",
                battery: Some(0.5),
                charging: false,
            },
            configured,
            account: Some("anna@example.com"),
            current_dir: "Books",
            book_count: entries.len(),
            local_books_count: 3,
            last_sync: Some("2 min ago"),
            filter: JianguoyunFilter::All,
            loading: false,
            message: None,
            entries,
        }
    }

    fn text_of(tree: &Tree, node: NodeId) -> String {
        match &tree.node(node).kind {
            Kind::Text { para, .. } => para
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>(),
            _ => panic!("not a text node"),
        }
    }

    fn actions(tree: &Tree) -> Vec<ActionId> {
        (0..tree.len())
            .map(|n| NodeId(n as u32))
            .filter_map(|n| tree.node(n).action)
            .collect()
    }

    #[test]
    fn a_folder_and_a_book_carry_their_own_actions() {
        with_language("en", || {
            let entries = vec![
                JianguoyunEntry::folder("Books/Novels", "Novels"),
                book("Books/dune.epub", "Dune"),
            ];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, shelf) =
                JianguoyunShelfView::build(&mut text, &styles, &view(&entries, true));
            let acts = actions(&tree);
            assert!(acts.contains(&action::ENTER), "the folder opens");
            assert!(acts.contains(&(action::DOWNLOAD + 1)), "the book downloads");
            assert_eq!(shelf.entry_at(action::ENTER), Some("Books/Novels"));
            assert_eq!(
                shelf.entry_at(action::DOWNLOAD + 1),
                Some("Books/dune.epub")
            );
            assert_eq!(shelf.entry_at(action::PREV_PAGE), None);
        });
    }

    #[test]
    fn the_filter_keeps_the_folders_and_picks_the_books() {
        let mut downloaded = book("Books/dune.epub", "Dune");
        downloaded.download = JianguoyunDownloadState::Downloaded;
        let entries = vec![
            JianguoyunEntry::folder("Books/Novels", "Novels"),
            downloaded,
            book("Books/emma.epub", "Emma"),
        ];
        assert_eq!(
            shown_entries(&entries, JianguoyunFilter::All).len(),
            3,
            "every entry"
        );
        assert_eq!(
            shown_entries(&entries, JianguoyunFilter::Downloaded).len(),
            2,
            "the folder and the downloaded book"
        );
        assert_eq!(
            shown_entries(&entries, JianguoyunFilter::NotDownloaded).len(),
            2,
            "the folder and the book that is not on the device"
        );
    }

    #[test]
    fn a_downloading_row_offers_a_cancel_and_a_downloaded_one_opens() {
        with_language("en", || {
            let mut downloading = book("Books/a.epub", "A");
            downloading.download = JianguoyunDownloadState::Downloading { percent: 42 };
            let mut done = book("Books/b.epub", "B");
            done.download = JianguoyunDownloadState::Downloaded;
            let mut failed = book("Books/c.epub", "C");
            failed.download = JianguoyunDownloadState::Failed;
            let entries = vec![downloading, done, failed];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, shelf) =
                JianguoyunShelfView::build(&mut text, &styles, &view(&entries, true));
            let acts = actions(&tree);
            assert!(acts.contains(&action::CANCEL));
            assert!(acts.contains(&(action::OPEN + 1)));
            assert!(acts.contains(&(action::DOWNLOAD + 2)));
            assert_eq!(shelf.entry_at(action::CANCEL), Some("Books/a.epub"));
            assert!(text_of(&tree, shelf.row_status[0]).contains("Downloading 42%"));
            assert!(text_of(&tree, shelf.row_status[1]).contains("Downloaded"));
            assert!(text_of(&tree, shelf.row_status[2]).contains("Download failed"));
        });
    }

    #[test]
    fn a_progress_tick_updates_the_row_in_place() {
        with_language("en", || {
            let entries = vec![book("Books/a.epub", "A")];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (mut tree, shelf) =
                JianguoyunShelfView::build(&mut text, &styles, &view(&entries, true));
            let before = tree.len();
            shelf.sync_download(
                &mut tree,
                "Books/a.epub",
                JianguoyunDownloadState::Downloading { percent: 30 },
            );
            assert_eq!(tree.len(), before, "no node was added or removed");
            assert!(text_of(&tree, shelf.row_status[0]).contains("Downloading 30%"));
        });
    }

    #[test]
    fn an_unconfigured_shelf_offers_the_setup_guide() {
        with_language("zh", || {
            let entries: Vec<JianguoyunEntry> = Vec::new();
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, _) = JianguoyunShelfView::build(&mut text, &styles, &view(&entries, false));
            let acts = actions(&tree);
            assert!(acts.contains(&action::LOGIN), "the guide's button");
            assert!(acts.contains(&action::LOCAL), "the local cell");
            let found = (0..tree.len()).map(|n| NodeId(n as u32)).any(|n| {
                if let Kind::Text { para, .. } = &tree.node(n).kind {
                    para.spans
                        .iter()
                        .any(|s| s.text == tr(Msg::JianguoyunNotConfiguredTitle))
                } else {
                    false
                }
            });
            assert!(found, "the guide's title");
        });
    }

    #[test]
    fn the_account_card_offers_refresh_configure_and_sign_out() {
        with_language("en", || {
            let entries = vec![book("Books/a.epub", "A")];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, _) = JianguoyunShelfView::build(&mut text, &styles, &view(&entries, true));
            let acts = actions(&tree);
            assert!(acts.contains(&action::SYNC));
            assert!(acts.contains(&action::CONFIGURE));
            assert!(acts.contains(&action::LOGOUT));
        });
    }

    #[test]
    fn the_parent_button_appears_only_below_the_root() {
        with_language("en", || {
            let entries = vec![book("a.epub", "A")];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let mut v = view(&entries, true);
            v.current_dir = "";
            let (tree, _) = JianguoyunShelfView::build(&mut text, &styles, &v);
            assert!(
                !actions(&tree).contains(&action::PARENT),
                "the root has no parent"
            );
            v.current_dir = "Books/Novels";
            let (tree, _) = JianguoyunShelfView::build(&mut text, &styles, &v);
            assert!(actions(&tree).contains(&action::PARENT));
        });
    }

    #[test]
    fn the_list_paginates_by_height_and_the_pager_turns_the_page() {
        with_language("en", || {
            let entries: Vec<JianguoyunEntry> = (0..12)
                .map(|i| book(&format!("Books/{i}.epub"), &format!("Book {i}")))
                .collect();
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, mut shelf) =
                JianguoyunShelfView::build(&mut text, &styles, &view(&entries, true));
            let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
            ui.layout();
            let (page, count) = ui.tree.page(shelf.list);
            assert!(count > 1, "twelve books do not fit on one page");
            assert_eq!(page, 0);
            assert!(shelf.handle(&mut ui.tree, action::NEXT_PAGE));
            assert_eq!(ui.tree.page(shelf.list).0, 1);
            assert!(shelf.handle(&mut ui.tree, action::PREV_PAGE));
            assert_eq!(ui.tree.page(shelf.list).0, 0);
        });
    }
}
