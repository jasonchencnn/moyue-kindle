// SPDX-License-Identifier: AGPL-3.0-or-later
//! The cloud shelf: the books of a Readest account, and their download
//! state.
//!
//! It shares the library's frame: the status line, the title with the
//! "Local / Cloud" segmented control, the list of four cards a page, and
//! the pager. A card shows the typographic cover, title, author, format and
//! the cloud reading progress; the button on its right downloads the book
//! or opens the copy already on the device.
//!
//! The screen is a view over data the app owns: the books, the account and
//! the download in flight. It only keeps the nodes it updates in place
//! (the pager).

use super::icon_button;
use super::library::{
    list_insets, page_of, segment_control_channels, status_line, ChannelVisibility, Segment,
    StatusLine,
};
use crate::geom::{mm, Insets, Size, MIN_TARGET};
use crate::icons::Icon;
use crate::input::Direction;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_MID, PAPER, RULE};
use crate::tree::{pages, ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::{Cover, CoverMark, GrayImage};
use aristotle_i18n::{tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;

    /// The "local" cell of the segmented control.
    pub const LOCAL: ActionId = 1;
    /// Ask the account for the list again.
    pub const SYNC: ActionId = 2;
    /// The guide shown when nobody is signed in.
    pub const LOGIN: ActionId = 3;
    /// The "WeRead" cell of the segmented control.
    pub const WEREAD: ActionId = 4;
    /// The "Nutstore" cell of the segmented control.
    pub const JIANGUOYUN: ActionId = 5;
    /// `FILTER + i` selects `CloudFilter::ALL[i]`.
    pub const FILTER: ActionId = 10;
    pub const PREV_PAGE: ActionId = 20;
    pub const NEXT_PAGE: ActionId = 21;
    /// `ITEM + i` downloads `books[i]`, or opens it once downloaded.
    pub const ITEM: ActionId = 100;
    /// `CANCEL + i` stops the download of `books[i]`. Kept far above the
    /// item range: a shelf may hold many more than a hundred books.
    pub const CANCEL: ActionId = 10_000;
}

/// What the cloud list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CloudFilter {
    #[default]
    All,
    NotDownloaded,
    Downloaded,
}

impl CloudFilter {
    pub const ALL: [CloudFilter; 3] = [
        CloudFilter::All,
        CloudFilter::NotDownloaded,
        CloudFilter::Downloaded,
    ];

    pub fn label(self) -> &'static str {
        tr(match self {
            CloudFilter::All => Msg::CloudFilterAll,
            CloudFilter::NotDownloaded => Msg::CloudFilterNotDownloaded,
            CloudFilter::Downloaded => Msg::CloudFilterDownloaded,
        })
    }

    pub fn accepts(self, book: &CloudBook) -> bool {
        match self {
            CloudFilter::All => true,
            CloudFilter::NotDownloaded => !book.downloaded,
            CloudFilter::Downloaded => book.downloaded,
        }
    }
}

/// Download state of one cloud book.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CloudBookState {
    #[default]
    NotDownloaded,
    Downloading,
    Downloaded,
    Failed,
}

/// One book on the cloud shelf.
#[derive(Clone, Debug)]
pub struct CloudBook {
    /// The book's partial MD5, the key the app downloads and opens by.
    pub hash: String,
    pub title: String,
    pub author: String,
    pub format: String,
    /// Cloud reading progress as `[current, total]` pages, when known.
    pub progress: Option<(u32, u32)>,
    pub state: CloudBookState,
    /// Download progress, 0.0 to 1.0, while `state` is `Downloading`.
    pub progress_fraction: f32,
    /// The server gave a total size, so `progress_fraction` means something.
    pub progress_known: bool,
    pub downloaded: bool,
    /// The book's cover, when one is available.
    pub cover: Option<GrayImage>,
}

impl CloudBook {
    pub fn new(title: &str, author: &str, format: &str) -> Self {
        Self {
            hash: String::new(),
            title: title.into(),
            author: author.into(),
            format: format.into(),
            progress: None,
            state: CloudBookState::NotDownloaded,
            progress_fraction: 0.0,
            progress_known: true,
            downloaded: false,
            cover: None,
        }
    }

    pub fn mark(&self) -> CoverMark {
        CoverMark::default()
    }
}

/// Everything the cloud shelf shows.
pub struct CloudView<'a> {
    pub status: StatusLine<'a>,
    pub signed_in: bool,
    /// Total cloud books, before the filter.
    pub book_count: usize,
    /// The local library's count, for the "Local" segment.
    pub local_books_count: usize,
    /// When the list was last fetched, in words.
    pub last_sync: Option<&'a str>,
    /// The list came from the cache on disk.
    pub offline: bool,
    pub filter: CloudFilter,
    pub loading: bool,
    /// A failure or a hint shown above the list.
    pub message: Option<&'a str>,
    pub books: &'a [CloudBook],
}

/// Handles to the nodes the screen updates in place.
pub struct CloudUi {
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    /// Node of each list row, for tests and the preview.
    pub rows: Vec<NodeId>,
    /// The books the rows were built from, in the order they were drawn.
    pub row_books: CloudRows,
}

/// The books the current screen drew, in the order it drew them.
///
/// Row actions carry a position in this list, and the app keeps the same
/// list beside the screen. A tap is then resolved against what the reader
/// sees, never against a freshly filtered list where the same position could
/// be another book after a download or a scan changed the library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloudRows {
    order: Vec<String>,
}

impl CloudRows {
    pub fn new(order: Vec<String>) -> Self {
        Self { order }
    }

    /// The books' hashes, in drawing order.
    pub fn order(&self) -> &[String] {
        &self.order
    }

    /// The book `id` points at, when it is a row action from `base`.
    pub fn target(&self, base: ActionId, id: ActionId) -> Option<&str> {
        let index = id.checked_sub(base)? as usize;
        self.order.get(index).map(String::as_str)
    }
}

fn cloud_badge(t: &mut Tree) -> NodeId {
    let icon = t.icon(Icon::Storage, 44, PAPER);
    let aligned = t.align(0.5, 0.5, icon);
    let badge = t.add(Node::column(0).bg(INK).border(0, 0, 20), [aligned]);
    t.fixed(Some(76), Some(76), Some(badge))
}

fn filter_chips(t: &mut Tree, s: &Styles, filter: CloudFilter) -> NodeId {
    let mut chips = Vec::new();
    for (i, f) in CloudFilter::ALL.iter().enumerate() {
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

fn state_label(book: &CloudBook) -> String {
    match book.state {
        CloudBookState::NotDownloaded => tr(Msg::CloudNotDownloaded).to_string(),
        CloudBookState::Downloaded => tr(Msg::CloudDownloaded).to_string(),
        CloudBookState::Downloading => downloading_label(book),
        CloudBookState::Failed => tr(Msg::CloudRetry).to_string(),
    }
}

fn downloading_label(book: &CloudBook) -> String {
    if book.progress_known {
        tf!(
            Msg::CloudDownloading,
            percent = format!("{:.0}", (book.progress_fraction * 100.0).floor())
        )
    } else {
        tr(Msg::CloudDownloadingUnknown).to_string()
    }
}

fn progress_label(book: &CloudBook) -> String {
    match book.progress {
        Some((current, total)) if total > 0 => {
            let percent = (current as f32 * 100.0 / total as f32).floor();
            if percent >= 100.0 {
                tr(Msg::CloudProgressFinished).to_string()
            } else if current == 0 {
                tr(Msg::CloudProgressUnread).to_string()
            } else {
                tf!(Msg::CloudProgress, percent = format!("{percent:.0}"))
            }
        }
        _ => tr(Msg::CloudProgressUnread).to_string(),
    }
}

/// The button at the right of a card: download, progress, retry or open.
fn cloud_action(t: &mut Tree, s: &Styles, book: &CloudBook, index: usize) -> NodeId {
    let id = action::ITEM + index as ActionId;
    match book.state {
        CloudBookState::Downloading => {
            // The prototype has no cancel control here; this text button sits
            // in the same place as the other row buttons, at the 64 px
            // minimum touch target QA-101 asks for (kept by the Leader's
            // COVER-003 rework).
            let label = t.label(&downloading_label(book), s.font_xs().line_height(1.2));
            let mut items = vec![label];
            if book.progress_known {
                let bar = t.custom(super::library::RoundedBar {
                    value: book.progress_fraction,
                    height: 10,
                    track_gray: PAPER,
                });
                let bar = t.fixed(Some(248), Some(10), Some(bar));
                items.push(bar);
            }
            let cancel_label = t.label(tr(Msg::CommonCancel), s.font_xs().bold());
            let cancel_pad = t.pad(Insets::vh(0, 20), cancel_label);
            let cancel = t.button(
                Node::plain().border(2, INK, 18),
                cancel_pad,
                action::CANCEL + index as ActionId,
            );
            let cancel = t.fixed(Some(160), Some(64), Some(cancel));
            items.push(cancel);
            t.add(Node::column(10).cross(Cross::End), items)
        }
        CloudBookState::Downloaded => {
            let check = t.icon(Icon::Check, 28, PAPER);
            let label = t.label(tr(Msg::CloudOpen), s.font_sm().bold().gray(PAPER));
            let row = t.add(Node::row(10).cross(Cross::Center), [check, label]);
            let pad = t.pad(Insets::vh(0, 24), row);
            let button = t.button(Node::plain().bg(INK).border(0, 0, 20), pad, id);
            t.fixed(Some(248), Some(80), Some(button))
        }
        CloudBookState::NotDownloaded | CloudBookState::Failed => {
            let label_text = if book.state == CloudBookState::Failed {
                tr(Msg::CloudRetry)
            } else {
                tr(Msg::CloudDownload)
            };
            let icon = if book.state == CloudBookState::Failed {
                Icon::Refresh
            } else {
                Icon::Download
            };
            let icon = t.icon(icon, 28, INK);
            let label = t.label(label_text, s.font_sm());
            let row = t.add(Node::row(10).cross(Cross::Center), [icon, label]);
            let pad = t.pad(Insets::vh(0, 24), row);
            let button = t.button(Node::plain().border(2, INK, 20), pad, id);
            t.fixed(Some(248), Some(80), Some(button))
        }
    }
}

/// The books shown for `filter`, in the order the shelf draws them.
fn shown_books(books: &[CloudBook], filter: CloudFilter) -> Vec<&CloudBook> {
    books.iter().filter(|b| filter.accepts(b)).collect()
}

fn book_row(t: &mut Tree, s: &Styles, book: &CloudBook, index: usize) -> NodeId {
    let cover = Cover::new(&book.title, &book.author, s.family)
        .with_mark(book.mark())
        .with_source_tag(super::library::BookSource::Readest.label())
        .with_image(book.cover.clone());
    let cover_node = t.custom(cover);
    let cover_box = t.fixed(Some(108), Some(144), Some(cover_node));

    let mut tp = Paragraph::plain(
        &book.title,
        s.font_base().bold().line_height(1.25),
        Align::Left,
    );
    tp.style.max_lines = Some(1);
    let title = t.text(tp);

    let mut mp = Paragraph::plain(
        format!("{} · {}", book.author, book.format),
        s.font_xs().gray(INK_MID).line_height(1.3),
        Align::Left,
    );
    mp.style.max_lines = Some(1);
    let meta = t.text(mp);

    let progress = t.label(
        &format!("{} · {}", progress_label(book), state_label(book)),
        s.font_xs().gray(INK_2).line_height(1.3),
    );

    let info = t.add(
        Node::column(8).cross(Cross::Start).flex(1),
        [title, meta, progress],
    );
    let button = cloud_action(t, s, book, index);

    let row = t.add(
        Node::row(24).cross(Cross::Center),
        [cover_box, info, button],
    );
    let pad = t.pad(Insets::new(18, 24, 18, 24), row);
    let card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]);
    let card = t.fixed(None, Some(215), Some(card));
    t.add(Node::new(Kind::MinSize(Size::new(0, MIN_TARGET))), [card])
}

fn empty_message(t: &mut Tree, s: &Styles, view: &CloudView) -> NodeId {
    let (title, hint) = if !view.signed_in {
        (tr(Msg::CloudLoginTitle), Some(tr(Msg::CloudLoginHint)))
    } else if view.loading {
        (tr(Msg::CloudLoading), None)
    } else if let Some(message) = view.message {
        // The failure is the title: "no books" next to "network down" says
        // two different things at once.
        (message, None)
    } else {
        (tr(Msg::CloudEmpty), None)
    };
    let mut tp = Paragraph::plain(title, s.ui().bold().gray(INK), Align::Center);
    tp.style.max_lines = Some(3);
    let title = t.text(tp);
    let mut children = vec![title];
    if let Some(hint) = hint {
        let mut hp = Paragraph::plain(hint, s.small().gray(INK_2), Align::Center);
        hp.style.max_lines = Some(3);
        let hint = t.text(hp);
        let g = t.gap(1, 16);
        children.extend([g, hint]);
    }
    if !view.signed_in {
        let label = t.label(tr(Msg::CloudLoginButton), s.ui_bold().gray(PAPER));
        let pad = t.pad(Insets::vh(0, 28), label);
        let button = t.add(
            Node::new(Kind::MinSize(Size::new(420, 80)))
                .bg(INK)
                .border(0, 0, 20)
                .action(action::LOGIN)
                .feedback(Feedback::Invert),
            [pad],
        );
        let g = t.gap(1, 32);
        children.extend([g, button]);
    }
    let column = t.add(Node::column(16).cross(Cross::Center), children);
    let pad = t.pad(Insets::vh(mm(10.0), 24), column);
    t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad])
}

impl<'a> CloudView<'a> {
    pub fn build(text: &mut TextEngine, s: &Styles, view: &CloudView) -> (Tree, CloudUi) {
        Self::build_for_width(text, s, view, crate::geom::SCREEN_WIDTH)
    }

    /// Builds the shelf for a screen `width` px wide.
    pub fn build_for_width(
        text: &mut TextEngine,
        s: &Styles,
        view: &CloudView,
        width: i32,
    ) -> (Tree, CloudUi) {
        Self::build_for_width_with_sources(text, s, view, width, false)
    }

    /// Builds the cloud shelf for `width`, optionally showing the WeRead tab
    /// when enabled in Settings.
    pub fn build_for_width_with_sources(
        text: &mut TextEngine,
        s: &Styles,
        view: &CloudView,
        width: i32,
        show_weread: bool,
    ) -> (Tree, CloudUi) {
        Self::build_for_width_with_channels(
            text,
            s,
            view,
            width,
            ChannelVisibility::readest_and_weread(true, show_weread),
        )
    }

    /// Builds the cloud shelf for `width`, showing the cloud channels
    /// Settings turns on: Readest, WeRead and Nutstore.
    pub fn build_for_width_with_channels(
        text: &mut TextEngine,
        s: &Styles,
        view: &CloudView,
        _width: i32,
        channels: ChannelVisibility,
    ) -> (Tree, CloudUi) {
        let _ = text;
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &view.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let mut column = vec![status];

        // Title, in the same place as on the local library.
        let title_text = tr(Msg::LibraryTitle);
        let title_style = s.font_lg().bold().line_height(1.2).tracking(4.0);
        let title = t.text(Paragraph::plain(title_text, title_style, Align::Left));
        let header = t.add(Node::row(0).cross(Cross::Center), [title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
        column.push(header);

        // Local / Cloud / optional WeRead and Nutstore, in the same place and
        // size as on the library. This screen is the Readest shelf, so its
        // own cell is always on.
        let channels = ChannelVisibility {
            readest: true,
            ..channels
        };
        let seg = segment_control_channels(
            &mut t,
            s,
            view.local_books_count,
            Segment::Cloud,
            Some(action::LOCAL),
            None,
            channels.weread.then_some(action::WEREAD),
            channels.jianguoyun.then_some(action::JIANGUOYUN),
            channels,
        );
        let seg = t.fixed(None, Some(76), Some(seg));
        let seg = t.pad(Insets::new(0, home_margin(), 28, home_margin()), seg);
        column.push(seg);

        // Account card: whose books these are, and the way to refresh.
        if view.signed_in {
            let badge = cloud_badge(&mut t);
            let account = t.label(
                &tf!(Msg::CloudAccount, count = view.book_count),
                s.font_base().bold().line_height(1.3),
            );
            let line = if let Some(message) = view.message {
                message.to_string()
            } else if let Some(time) = view.last_sync {
                if view.offline {
                    tf!(Msg::CloudSyncCached, time = time)
                } else {
                    tf!(Msg::CloudSyncLine, time = time)
                }
            } else {
                tr(Msg::CloudSyncNever).to_string()
            };
            let mut lp =
                Paragraph::plain(&line, s.font_xs().gray(INK_2).line_height(1.3), Align::Left);
            lp.style.max_lines = Some(1);
            let line = t.text(lp);
            let words = t.add(Node::column(4).flex(1), [account, line]);
            let sync_icon = t.icon(Icon::Refresh, 30, PAPER);
            let sync_label = t.label(tr(Msg::CloudSync), s.font_sm().bold().gray(PAPER));
            let sync_row = t.add(Node::row(10).cross(Cross::Center), [sync_icon, sync_label]);
            let sync_pad = t.pad(Insets::vh(0, 24), sync_row);
            let sync = t.button(
                Node::plain().bg(INK).border(0, 0, 20),
                sync_pad,
                action::SYNC,
            );
            let sync = t.fixed(Some(196), Some(80), Some(sync));
            let card = t.add(Node::row(24).cross(Cross::Center), [badge, words, sync]);
            let pad = t.pad(Insets::new(22, 24, 22, 24), card);
            let card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]);
            let card = t.pad(Insets::new(24, home_margin(), 0, home_margin()), card);
            column.push(card);

            let chips = filter_chips(&mut t, s, view.filter);
            let chips = t.pad(Insets::new(24, home_margin(), 0, home_margin()), chips);
            column.push(chips);
        }

        // The list: paginated by available height so six-inch, seven-inch and
        // larger panels each show as many complete cards as fit above the pager.
        let filtered = shown_books(view.books, view.filter);
        let mut rows = Vec::new();
        let mut row_books = Vec::new();
        let mut list_pages = None;
        let shelf = if filtered.is_empty() {
            let empty = empty_message(&mut t, s, view);
            let empty = t.pad(list_insets(), empty);
            t.add(Node::column(0).flex(1), [empty])
        } else {
            for (i, book) in filtered.iter().enumerate() {
                rows.push(book_row(&mut t, s, book, i));
                row_books.push(book.hash.clone());
            }
            let pages_node = t.add(pages(24).flex(1), rows.clone());
            list_pages = Some(pages_node);
            t.add(
                Node::new(Kind::Padding(list_insets())).flex(1),
                [pages_node],
            )
        };
        column.push(shelf);
        let list = list_pages.unwrap_or(shelf);

        // Pager, the same as the local list's.
        let prev = icon_button(&mut t, Icon::ChevronLeft, 44, action::PREV_PAGE);
        let next = icon_button(&mut t, Icon::ChevronRight, 44, action::NEXT_PAGE);
        let initial = page_of(1, 1);
        let pager_label = t.text(Paragraph::plain(
            initial,
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

        let ui = CloudUi {
            list,
            pager_label,
            prev: t.node(prev).children[0],
            next: t.node(next).children[0],
            rows,
            row_books: CloudRows::new(row_books),
        };
        (t, ui)
    }
}

impl CloudUi {
    /// Updates the pager after layout: page label and arrow states.
    pub fn sync(&self, t: &mut Tree) {
        let (page, count) = t.page(self.list);
        let count = count.max(1);
        t.set_text(self.pager_label, &page_of(page + 1, count));
        t.set_icon_gray(self.prev, if page == 0 { RULE } else { INK });
        t.set_icon_gray(self.next, if page + 1 >= count { RULE } else { INK });
    }

    /// Applies the pager actions. Returns true when handled.
    pub fn handle(&mut self, t: &mut Tree, id: ActionId) -> bool {
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
        let (page, _) = t.page(self.list);
        match dir {
            Direction::Up | Direction::Left => {
                t.set_page(self.list, page + 1);
                true
            }
            Direction::Down | Direction::Right => {
                t.set_page(self.list, page.saturating_sub(1));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_labels_are_translated() {
        aristotle_i18n::with_language("en", || {
            assert_eq!(CloudFilter::All.label(), "All");
            assert_eq!(CloudFilter::Downloaded.label(), "Downloaded");
        });
    }

    #[test]
    fn progress_label_reads_pages() {
        let mut b = CloudBook::new("Book", "Author", "EPUB");
        assert_eq!(progress_label(&b), tr(Msg::CloudProgressUnread));
        b.progress = Some((43, 100));
        assert!(progress_label(&b).contains("43"));
        b.progress = Some((100, 100));
        assert_eq!(progress_label(&b), tr(Msg::CloudProgressFinished));
    }

    #[test]
    fn filter_accepts_download_state() {
        let mut b = CloudBook::new("Book", "Author", "EPUB");
        assert!(CloudFilter::NotDownloaded.accepts(&b));
        assert!(!CloudFilter::Downloaded.accepts(&b));
        b.downloaded = true;
        b.state = CloudBookState::Downloaded;
        assert!(CloudFilter::Downloaded.accepts(&b));
    }

    #[test]
    fn row_actions_keep_the_books_the_screen_drew() {
        let mut done = CloudBook::new("Done", "Author", "EPUB");
        done.hash = "aaa".into();
        done.downloaded = true;
        let mut other = CloudBook::new("Other", "Author", "EPUB");
        other.hash = "bbb".into();
        let mut books = [done, other];

        let drawn: Vec<String> = shown_books(&books, CloudFilter::NotDownloaded)
            .iter()
            .map(|b| b.hash.clone())
            .collect();
        assert_eq!(drawn, ["bbb"]);
        let rows = CloudRows::new(drawn);

        // A download elsewhere changes the library: the same position in a
        // fresh list is now another book, but the id still means the book
        // that was drawn there.
        books[0].downloaded = false;
        let fresh: Vec<String> = shown_books(&books, CloudFilter::NotDownloaded)
            .iter()
            .map(|b| b.hash.clone())
            .collect();
        assert_eq!(fresh, ["aaa", "bbb"]);
        assert_eq!(rows.target(action::ITEM, action::ITEM), Some("bbb"));
        assert_eq!(rows.target(action::CANCEL, action::CANCEL), Some("bbb"));
        assert_eq!(rows.target(action::ITEM, action::ITEM + 1), None);
    }

    use crate::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
    use crate::screens::library::{
        self as local, CoverMode, Filter, Library, LibraryLayout, Place, Shelf, View,
    };
    use crate::text::{FamilyId, Fonts};
    use crate::ui::Ui;

    fn find_action(tree: &Tree, id: ActionId) -> NodeId {
        (0..tree.len())
            .map(|n| NodeId(n as u32))
            .find(|n| tree.node(*n).action == Some(id))
            .unwrap_or_else(|| panic!("action {id} not found"))
    }

    /// FIX-105: the cloud shelf shares the local list's frame, so its cards
    /// line up with the page header and keep the same gap below the chips.
    #[test]
    fn cards_line_up_with_the_local_list() {
        let books: Vec<CloudBook> = (0..4)
            .map(|i| CloudBook::new(&format!("Cloud {i}"), "Author", "EPUB"))
            .collect();
        let status = || StatusLine {
            clock: "12:00",
            battery: Some(0.5),
            charging: false,
        };
        let view = CloudView {
            status: status(),
            signed_in: true,
            book_count: books.len(),
            local_books_count: 4,
            last_sync: Some("2 min ago"),
            offline: false,
            filter: CloudFilter::All,
            loading: false,
            message: None,
            books: &books,
        };
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0));
        let (tree, cloud) = CloudView::build(&mut text, &styles, &view);
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();

        let local_books = local::sample_books();
        let items = local::sample_items(&local_books, View::Books);
        let shelf = Shelf {
            place: Place::Home {
                view: View::Books,
                filter: Filter::All,
            },
            hero: None,
            items: &items,
            sort: "Recent",
            status: status(),
            covers: CoverMode::Cards,
            layout: LibraryLayout::List,
            local_books_count: local_books.len(),
        };
        let mut text = TextEngine::new(Fonts::embedded());
        let (tree, library) = Library::build_shelf(&mut text, &styles, &shelf);
        let mut local_ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        local_ui.layout();

        let card = ui.tree.rect(cloud.rows[0]);
        let local_card = local_ui.tree.rect(library.rows[0]);
        assert_eq!(card.x, local_card.x);
        assert_eq!(card.w, local_card.w);
        assert_eq!(card.x, home_margin());

        let chip = find_action(&ui.tree, action::FILTER);
        let local_chip = find_action(&local_ui.tree, local::action::FILTER);
        let gap = card.y - ui.tree.rect(chip).bottom();
        let local_gap = local_card.y - local_ui.tree.rect(local_chip).bottom();
        assert_eq!(gap, local_gap);
        assert_eq!(gap, list_insets().top);
    }
}
