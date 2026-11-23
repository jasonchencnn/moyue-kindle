// SPDX-License-Identifier: AGPL-3.0-or-later
//! The WeRead shelf: the books in the reader's WeRead account, read-only.
//!
//! It shares the library's frame: the status line, the title, the
//! "Local / Cloud / WeRead" segmented control, the account card with the
//! refresh button, the filter chips, a list of three cards a page, and the
//! pager. A card is a placeholder: v1 downloads nothing, so its right button
//! is a static "not downloaded" pill, not an action.
//!
//! The screen is a view over data the app owns. Paging only moves the page
//! of the already-built list: it never reads a file, dials the network or
//! takes a lock (the app fetches the shelf when the screen opens or the
//! reader asks for it).

use super::library::{
    list_insets, page_of, segment_control_channels, status_line, ChannelVisibility, Segment,
    StatusLine,
};
use crate::geom::{Insets, Size, MIN_TARGET};
use crate::icons::Icon;
use crate::input::Direction;
use crate::text::{Align, Paragraph};
use crate::theme::{home_margin, Styles, CARD, INK, INK_2, INK_3, INK_MID, PAPER, RULE};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::Cover;
use aristotle_i18n::{tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// The "local" cell of the segmented control.
    pub const LOCAL: ActionId = 1;
    /// The "cloud" cell of the segmented control.
    pub const CLOUD: ActionId = 2;
    /// Ask the app for the shelf again.
    pub const SYNC: ActionId = 3;
    /// The guide shown when nobody is signed in.
    pub const LOGIN: ActionId = 4;
    /// Sign out of the current WeRead account so the user can switch accounts.
    pub const LOGOUT: ActionId = 5;
    /// The "Nutstore" cell of the segmented control.
    pub const JIANGUOYUN: ActionId = 6;
    /// `FILTER + i` selects `WereadFilter::ALL[i]`.
    pub const FILTER: ActionId = 10;
    pub const PREV_PAGE: ActionId = 20;
    pub const NEXT_PAGE: ActionId = 21;
    /// `DOWNLOAD + i` downloads (or retries) the card at row `i` on this page.
    pub const DOWNLOAD: ActionId = 30;
    /// `CANCEL + i` stops the download of the card at row `i`.
    pub const CANCEL: ActionId = 40;
    /// `OPEN + i` opens the downloaded card at row `i`.
    pub const OPEN: ActionId = 50;
}

/// Which books the shelf shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WereadFilter {
    #[default]
    All,
    /// Only the books that belong to a WeRead book list (`archive`).
    Lists,
}

impl WereadFilter {
    pub const ALL: [WereadFilter; 2] = [WereadFilter::All, WereadFilter::Lists];

    pub fn label(self) -> &'static str {
        tr(match self {
            WereadFilter::All => Msg::WereadFilterAll,
            WereadFilter::Lists => Msg::WereadFilterArchive,
        })
    }

    pub fn accepts(self, book: &WereadBook) -> bool {
        match self {
            WereadFilter::All => true,
            WereadFilter::Lists => book.archive,
        }
    }
}

/// What the card's download control shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WereadDownloadState {
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

/// One book on the WeRead shelf.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WereadBook {
    /// The WeRead `bookId`; the row's identity.
    pub id: String,
    pub title: String,
    /// The format line under the title (`epub`); the shelf row has no author.
    pub format: String,
    /// Cloud reading progress, percent.
    pub progress: Option<i64>,
    pub finished: bool,
    /// The book belongs to a book list (WeRead's `archive`).
    pub archive: bool,
    /// The book is marked private in WeRead.
    pub secret: bool,
    /// Download state shown on the card and its right-hand control.
    pub download: WereadDownloadState,
}

/// Everything the WeRead shelf shows.
pub struct WereadShelfView<'a> {
    pub status: StatusLine<'a>,
    pub signed_in: bool,
    /// Total WeRead books, before the filter.
    pub book_count: usize,
    /// The local library's count, for the segmented control.
    pub local_books_count: usize,
    /// When the shelf was last fetched, in words.
    pub last_sync: Option<&'a str>,
    pub filter: WereadFilter,
    pub loading: bool,
    /// A failure or a hint shown above the list.
    pub message: Option<&'a str>,
    /// The page of the filtered list to draw; it is clamped to the count.
    pub page: usize,
    pub books: &'a [WereadBook],
}

/// Handles to the nodes the screen updates in place.
pub struct WereadShelfUi {
    /// The page on screen, zero-based.
    pub page: usize,
    /// How many pages the filtered list has.
    pub count: usize,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    /// Node of each list row drawn on this page, for tests and the preview.
    pub rows: Vec<NodeId>,
    /// WeRead book id of each row on this page, in action order.
    row_books: Vec<String>,
    /// The book behind each row, for in-place status updates.
    row_data: Vec<WereadBook>,
    /// The status text node of each row.
    row_status: Vec<NodeId>,
}

/// How many cards a page holds. Three keeps every card above the pager on
/// the PW4: the cloud shelf's fourth card is clipped by it.
pub const PER_PAGE: usize = 3;
/// The height of one card: a 108x144 cover and its two lines of air.
pub const CARD_H: i32 = 188;
/// The gap between two cards on a page.
pub const CARD_GAP: i32 = 16;
/// The height of a page of cards.
pub const PAGE_H: i32 = PER_PAGE as i32 * CARD_H + (PER_PAGE as i32 - 1) * CARD_GAP;

fn badge(t: &mut Tree) -> NodeId {
    let icon = t.icon(Icon::Language, 44, PAPER);
    let aligned = t.align(0.5, 0.5, icon);
    let badge = t.add(Node::column(0).bg(INK).border(0, 0, 20), [aligned]);
    t.fixed(Some(76), Some(76), Some(badge))
}

fn filter_chips(t: &mut Tree, s: &Styles, filter: WereadFilter) -> NodeId {
    let mut chips = Vec::new();
    for (i, f) in WereadFilter::ALL.iter().enumerate() {
        chips.push(crate::components::chip(
            t,
            s,
            f.label(),
            *f == filter,
            action::FILTER + i as ActionId,
        ));
    }
    chips.push(t.spacer(1));
    chips.push(crate::components::chip(
        t,
        s,
        tr(Msg::WereadSignOut),
        false,
        action::LOGOUT,
    ));
    t.add(Node::row(16).cross(Cross::Center), chips)
}

fn progress_label(book: &WereadBook) -> String {
    if book.finished {
        return tr(Msg::WereadProgressFinished).to_string();
    }
    match book.progress {
        Some(percent) if percent > 0 => {
            tf!(Msg::WereadProgress, percent = percent.clamp(0, 100))
        }
        _ => tr(Msg::WereadProgressUnread).to_string(),
    }
}

fn download_label(book: &WereadBook) -> String {
    match book.download {
        WereadDownloadState::NotDownloaded => tr(Msg::WereadNotDownloaded).to_string(),
        WereadDownloadState::Downloading { percent } => {
            tf!(Msg::WereadDownloading, percent = percent)
        }
        WereadDownloadState::Downloaded => tr(Msg::WereadDownloaded).to_string(),
        WereadDownloadState::Failed => tr(Msg::WereadDownloadFailed).to_string(),
    }
}

fn book_meta(book: &WereadBook) -> String {
    let mut parts = vec![progress_label(book), download_label(book)];
    let fmt = book.format.trim();
    if !fmt.is_empty() {
        parts.push(fmt.to_ascii_uppercase());
    }
    if book.archive {
        parts.push(tr(Msg::WereadArchiveBadge).to_string());
    }
    if book.secret {
        parts.push(tr(Msg::WereadSecretBadge).to_string());
    }
    parts.join(" · ")
}

/// The card's right-hand control: download, cancel, retry or open.
///
/// The download state is an icon: the words "Not downloaded" are already on
/// the card, and a "Download" label would leave too little room for a book
/// that also has a list/private badge.
fn download_control(t: &mut Tree, s: &Styles, book: &WereadBook, row: usize) -> NodeId {
    if matches!(book.download, WereadDownloadState::NotDownloaded) {
        let icon = t.icon(Icon::Download, 32, INK_2);
        let centered = t.align(0.5, 0.5, icon);
        let button = t.button(
            Node::new(Kind::MinSize(Size::new(72, 72))).border(2, INK_3, 20),
            centered,
            action::DOWNLOAD + row as u32,
        );
        return t.fixed(Some(72), Some(72), Some(button));
    }
    let (label, action) = match book.download {
        WereadDownloadState::NotDownloaded => unreachable!("handled above"),
        WereadDownloadState::Downloading { .. } => {
            (tr(Msg::WereadDownloadCancel), action::CANCEL + row as u32)
        }
        WereadDownloadState::Downloaded => (tr(Msg::WereadOpen), action::OPEN + row as u32),
        WereadDownloadState::Failed => {
            (tr(Msg::WereadDownloadRetry), action::DOWNLOAD + row as u32)
        }
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

fn book_row(t: &mut Tree, s: &Styles, book: &WereadBook, card_index: usize) -> (NodeId, NodeId) {
    let cover = Cover::new(&book.title, &book.format, s.family)
        .with_source_tag(super::library::BookSource::WeRead.label());
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
        &book.format,
        s.font_xs().gray(INK_MID).line_height(1.3),
        Align::Left,
    );
    mp.style.max_lines = Some(1);
    let meta = t.text(mp);

    let mut pp = Paragraph::plain(
        book_meta(book),
        s.font_xs().gray(INK_2).line_height(1.3),
        Align::Left,
    );
    pp.style.max_lines = Some(1);
    let progress = t.text(pp);

    let info = t.add(
        Node::column(8).cross(Cross::Start).flex(1),
        [title, meta, progress],
    );
    let pill = download_control(t, s, book, card_index);

    let row = t.add(Node::row(24).cross(Cross::Center), [cover_box, info, pill]);
    let pad = t.pad(Insets::new(12, 24, 12, 24), row);
    let card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]);
    let card = t.fixed(None, Some(CARD_H), Some(card));
    let action = match book.download {
        WereadDownloadState::Downloading { .. } => None,
        WereadDownloadState::Downloaded => Some(action::OPEN + card_index as u32),
        WereadDownloadState::NotDownloaded | WereadDownloadState::Failed => {
            Some(action::DOWNLOAD + card_index as u32)
        }
    };
    let card = if let Some(id) = action {
        t.button(Node::plain(), card, id)
    } else {
        card
    };
    let card = t.add(Node::new(Kind::MinSize(Size::new(0, MIN_TARGET))), [card]);
    (card, progress)
}

fn empty_message(t: &mut Tree, s: &Styles, view: &WereadShelfView) -> NodeId {
    let (title, hint) = if !view.signed_in {
        // A failed shelf request leaves its message here; the reader then
        // sees why the guide is back ("sign-in expired") and the button.
        (
            view.message
                .unwrap_or_else(|| tr(Msg::WereadLoginGuideTitle)),
            Some(tr(Msg::WereadLoginGuideHint)),
        )
    } else if view.loading {
        (tr(Msg::WereadLoading), None)
    } else if let Some(message) = view.message {
        // The failure is the title: "no books" next to "service down" says
        // two different things at once.
        (message, None)
    } else {
        (tr(Msg::WereadEmpty), None)
    };
    let mut tp = Paragraph::plain(title, s.ui().bold().gray(INK), Align::Center);
    tp.style.max_lines = Some(3);
    let title = t.text(tp);
    let mut children = vec![title];
    if let Some(hint) = hint {
        let mut hp = Paragraph::plain(hint, s.small().gray(INK_2), Align::Center);
        hp.style.max_lines = Some(3);
        let hint = t.text(hp);
        children.push(t.gap(1, 16));
        children.push(hint);
    }
    if !view.signed_in {
        let label = t.label(tr(Msg::WereadLoginButton), s.ui_bold().gray(PAPER));
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

/// The books shown for `filter`, in the order the shelf draws them.
fn shown_books(books: &[WereadBook], filter: WereadFilter) -> Vec<&WereadBook> {
    books.iter().filter(|b| filter.accepts(b)).collect()
}

impl<'a> WereadShelfView<'a> {
    pub fn build(
        text: &mut crate::text::TextEngine,
        s: &Styles,
        view: &Self,
    ) -> (Tree, WereadShelfUi) {
        Self::build_for_width(text, s, view, crate::geom::SCREEN_WIDTH)
    }

    /// Builds the shelf for a screen `width` px wide.
    pub fn build_for_width(
        text: &mut crate::text::TextEngine,
        s: &Styles,
        view: &Self,
        width: i32,
    ) -> (Tree, WereadShelfUi) {
        Self::build_for_width_with_sources(text, s, view, width, true)
    }

    /// Builds the shelf for a screen `width` px wide, optionally showing the
    /// Readest cloud segment tab when `show_cloud` is enabled in Settings.
    pub fn build_for_width_with_sources(
        text: &mut crate::text::TextEngine,
        s: &Styles,
        view: &Self,
        width: i32,
        show_cloud: bool,
    ) -> (Tree, WereadShelfUi) {
        Self::build_for_width_with_channels(
            text,
            s,
            view,
            width,
            ChannelVisibility::readest_and_weread(show_cloud, true),
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
    ) -> (Tree, WereadShelfUi) {
        let _ = text;
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &view.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let mut column = vec![status];

        let title_text = tr(Msg::WereadTitle);
        let title_style = s.font_lg().bold().line_height(1.2).tracking(4.0);
        let title = t.text(Paragraph::plain(title_text, title_style, Align::Left));
        let header = t.add(Node::row(0).cross(Cross::Center), [title]);
        let header = t.fixed(None, Some(112), Some(header));
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
        column.push(header);

        // Local / optional Cloud / WeRead / optional Nutstore, in the same
        // place and size as on the library. This screen is the WeRead shelf,
        // so its own cell is always on. The chosen WeRead cell takes no action.
        let channels = ChannelVisibility {
            weread: true,
            ..channels
        };
        let seg = segment_control_channels(
            &mut t,
            s,
            view.local_books_count,
            Segment::Weread,
            Some(action::LOCAL),
            channels.readest.then_some(action::CLOUD),
            None,
            channels.jianguoyun.then_some(action::JIANGUOYUN),
            channels,
        );
        let seg = t.fixed(None, Some(76), Some(seg));
        let seg = t.pad(Insets::new(0, home_margin(), 28, home_margin()), seg);
        column.push(seg);

        if view.signed_in {
            let badge = badge(&mut t);
            let account = t.label(
                &tf!(Msg::WereadAccount, count = view.book_count),
                s.font_base().bold().line_height(1.3),
            );
            let line = if let Some(message) = view.message {
                message.to_string()
            } else if let Some(time) = view.last_sync {
                tf!(Msg::WereadSyncLine, time = time)
            } else {
                tr(Msg::WereadSyncNever).to_string()
            };
            let mut lp =
                Paragraph::plain(&line, s.font_xs().gray(INK_2).line_height(1.3), Align::Left);
            lp.style.max_lines = Some(1);
            let line = t.text(lp);
            let words = t.add(Node::column(4).flex(1), [account, line]);
            let sync_icon = t.icon(Icon::Refresh, 30, PAPER);
            let sync_label = t.label(tr(Msg::WereadRefresh), s.font_sm().bold().gray(PAPER));
            let sync_row = t.add(Node::row(10).cross(Cross::Center), [sync_icon, sync_label]);
            let sync_pad = t.pad(Insets::vh(0, 24), sync_row);
            let sync = t.button(
                Node::plain().bg(INK).border(0, 0, 20),
                sync_pad,
                action::SYNC,
            );
            let sync = t.fixed(Some(232), Some(80), Some(sync));
            let card = t.add(Node::row(24).cross(Cross::Center), [badge, words, sync]);
            let pad = t.pad(Insets::new(22, 24, 22, 24), card);
            let card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]);
            let card = t.pad(Insets::new(24, home_margin(), 0, home_margin()), card);
            column.push(card);

            let chips = filter_chips(&mut t, s, view.filter);
            let chips = t.pad(Insets::new(24, home_margin(), 0, home_margin()), chips);
            column.push(chips);
        }

        // Only the page on screen is built. A WeRead shelf can hold tens of
        // thousands of books (MiuRead #17): building a card for each of them
        // would allocate the whole shelf's node tree on every visit and page
        // turn. The page count is arithmetic; the pager moves the number.
        let filtered = shown_books(view.books, view.filter);
        let count = filtered.len().div_ceil(PER_PAGE).max(1);
        let page = view.page.min(count - 1);
        let mut rows = Vec::new();
        let mut row_books = Vec::new();
        let mut row_data = Vec::new();
        let mut row_status = Vec::new();
        let shelf = if filtered.is_empty() {
            let empty = empty_message(&mut t, s, view);
            let empty = t.pad(list_insets(), empty);
            t.add(Node::column(0).flex(1), [empty])
        } else {
            let start = page * PER_PAGE;
            let end = (start + PER_PAGE).min(filtered.len());
            for (row, book) in filtered[start..end].iter().enumerate() {
                let (card, status) = book_row(&mut t, s, book, row);
                rows.push(card);
                row_books.push(book.id.clone());
                row_data.push((*book).clone());
                row_status.push(status);
            }
            let mut page_items = Vec::new();
            for (ci, &card) in rows.iter().enumerate() {
                if ci > 0 {
                    page_items.push(t.gap(1, CARD_GAP));
                }
                page_items.push(card);
            }
            let page_col = t.add(Node::column(0), page_items);
            let page_box = t.fixed(None, Some(PAGE_H), Some(page_col));
            t.add(Node::new(Kind::Padding(list_insets())).flex(1), [page_box])
        };
        column.push(shelf);

        let prev = super::icon_button(&mut t, Icon::ChevronLeft, 44, action::PREV_PAGE);
        let next = super::icon_button(&mut t, Icon::ChevronRight, 44, action::NEXT_PAGE);
        let pager_label = t.text(Paragraph::plain(
            page_of(page + 1, count),
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

        let ui = WereadShelfUi {
            page,
            count,
            pager_label,
            prev: t.node(prev).children[0],
            next: t.node(next).children[0],
            rows,
            row_books,
            row_data,
            row_status,
        };
        (t, ui)
    }
}

impl WereadShelfUi {
    /// Updates the pager after layout: page label and arrow states.
    pub fn sync(&self, t: &mut Tree) {
        let count = self.count.max(1);
        t.set_text(self.pager_label, &page_of(self.page + 1, count));
        t.set_icon_gray(self.prev, if self.page == 0 { RULE } else { INK });
        t.set_icon_gray(self.next, if self.page + 1 >= count { RULE } else { INK });
    }

    /// The WeRead book id a card action belongs to, if it belongs to this
    /// page.
    pub fn book_at(&self, id: ActionId) -> Option<&str> {
        let row = if (action::DOWNLOAD..action::DOWNLOAD + PER_PAGE as u32).contains(&id) {
            id - action::DOWNLOAD
        } else if (action::CANCEL..action::CANCEL + PER_PAGE as u32).contains(&id) {
            id - action::CANCEL
        } else if (action::OPEN..action::OPEN + PER_PAGE as u32).contains(&id) {
            id - action::OPEN
        } else {
            return None;
        };
        self.row_books.get(row as usize).map(String::as_str)
    }

    /// Update one card's status line in place: a download tick does not
    /// rebuild the page or touch the disk.
    pub fn sync_download(&self, t: &mut Tree, book_id: &str, state: WereadDownloadState) {
        let Some(row) = self.row_books.iter().position(|id| id == book_id) else {
            return;
        };
        let Some(book) = self.row_data.get(row) else {
            return;
        };
        let mut book = book.clone();
        book.download = state;
        if let Some(node) = self.row_status.get(row) {
            t.set_text(*node, &book_meta(&book));
        }
    }

    /// Applies the pager actions. Returns true when the page changed; the
    /// app then builds the new page (three cards, no I/O).
    pub fn handle(&mut self, _t: &mut Tree, id: ActionId) -> bool {
        match id {
            action::PREV_PAGE if self.page > 0 => {
                self.page -= 1;
                true
            }
            action::NEXT_PAGE if self.page + 1 < self.count => {
                self.page += 1;
                true
            }
            _ => false,
        }
    }

    /// Swipe pages the list. Returns true when the page changed.
    pub fn handle_swipe(&mut self, _t: &mut Tree, dir: Direction) -> bool {
        match dir {
            Direction::Up | Direction::Left if self.page + 1 < self.count => {
                self.page += 1;
                true
            }
            Direction::Down | Direction::Right if self.page > 0 => {
                self.page -= 1;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
    use crate::text::{FamilyId, Fonts, TextEngine};
    use crate::tree::Refresh;
    use crate::ui::Ui;
    use aristotle_i18n::with_language;

    fn book(id: &str, archive: bool) -> WereadBook {
        WereadBook {
            id: id.to_string(),
            title: format!("Book {id}"),
            format: "epub".to_string(),
            progress: Some(42),
            finished: false,
            archive,
            secret: false,
            download: WereadDownloadState::NotDownloaded,
        }
    }

    fn view<'a>(books: &'a [WereadBook], signed_in: bool) -> WereadShelfView<'a> {
        WereadShelfView {
            status: StatusLine {
                clock: "12:00",
                battery: Some(0.5),
                charging: false,
            },
            signed_in,
            book_count: books.len(),
            local_books_count: 3,
            last_sync: Some("2 min ago"),
            filter: WereadFilter::All,
            loading: false,
            message: None,
            page: 0,
            books,
        }
    }

    #[test]
    fn filter_accepts_book_lists() {
        let all = book("a", false);
        let listed = book("b", true);
        assert!(WereadFilter::All.accepts(&all));
        assert!(WereadFilter::All.accepts(&listed));
        assert!(!WereadFilter::Lists.accepts(&all));
        assert!(WereadFilter::Lists.accepts(&listed));
        assert_eq!(
            shown_books(&[all, listed], WereadFilter::Lists)
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
    }

    #[test]
    fn each_page_holds_three_cards() {
        let books: Vec<WereadBook> = (0..10).map(|i| book(&i.to_string(), false)).collect();
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0));
        let (tree, shelf) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        assert_eq!(shelf.page, 0);
        assert_eq!(shelf.count, 4, "ten books, three a page");
        assert_eq!(shelf.rows.len(), 3, "only the page on screen is built");
    }

    #[test]
    fn paging_only_moves_the_page_of_the_built_list() {
        let books: Vec<WereadBook> = (0..7).map(|i| book(&i.to_string(), false)).collect();
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0));
        let (tree, mut shelf) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        assert!(shelf.handle(&mut ui.tree, action::NEXT_PAGE));
        assert_eq!(shelf.page, 1);
        assert!(shelf.handle(&mut ui.tree, action::PREV_PAGE));
        assert_eq!(shelf.page, 0);
        // Only one page of cards was ever built; the page number is an
        // index, not a tree of every book.
        assert_eq!(shelf.rows.len(), 3);
        assert_eq!(shelf.count, 3);
    }

    #[test]
    fn every_card_carries_its_download_action() {
        let books = vec![book("a", false), book("b", false), book("c", false)];
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0));
        let (tree, shelf) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
        let actions: Vec<ActionId> = (0..tree.len())
            .map(|n| NodeId(n as u32))
            .filter_map(|n| tree.node(n).action)
            .collect();
        for row in 0..3 {
            assert!(actions.contains(&(action::DOWNLOAD + row as u32)));
        }
        assert_eq!(shelf.book_at(action::DOWNLOAD), Some("a"));
        assert_eq!(shelf.book_at(action::DOWNLOAD + 2), Some("c"));
        assert_eq!(shelf.book_at(action::PREV_PAGE), None);
    }

    #[test]
    fn a_downloading_card_shows_progress_and_a_cancel_control() {
        with_language("en", || {
            let mut downloading = book("a", false);
            downloading.download = WereadDownloadState::Downloading { percent: 47 };
            let books = vec![downloading];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, shelf) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
            let actions: Vec<ActionId> = (0..tree.len())
                .map(|n| NodeId(n as u32))
                .filter_map(|n| tree.node(n).action)
                .collect();
            assert!(actions.contains(&action::CANCEL));
            assert!(!actions.contains(&action::DOWNLOAD));
            assert_eq!(shelf.book_at(action::CANCEL), Some("a"));
            let text = text_of(&tree, shelf.row_status[0]);
            assert!(text.contains("Downloading 47%"), "{text}");
        });
    }

    #[test]
    fn a_downloaded_card_opens_and_a_failed_one_retries() {
        with_language("en", || {
            let mut downloaded = book("a", false);
            downloaded.download = WereadDownloadState::Downloaded;
            let mut failed = book("b", false);
            failed.download = WereadDownloadState::Failed;
            let books = vec![downloaded, failed];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, shelf) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
            let actions: Vec<ActionId> = (0..tree.len())
                .map(|n| NodeId(n as u32))
                .filter_map(|n| tree.node(n).action)
                .collect();
            assert!(actions.contains(&action::OPEN));
            assert!(actions.contains(&(action::DOWNLOAD + 1)));
            assert_eq!(shelf.book_at(action::OPEN), Some("a"));
            assert!(text_of(&tree, shelf.row_status[0]).contains("Downloaded"));
            assert!(text_of(&tree, shelf.row_status[1]).contains("Download failed"));
        });
    }

    #[test]
    fn a_progress_tick_updates_the_status_line_in_place() {
        with_language("en", || {
            let books = vec![book("a", false)];
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (mut tree, shelf) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
            let before = tree.len();
            shelf.sync_download(
                &mut tree,
                "a",
                WereadDownloadState::Downloading { percent: 33 },
            );
            assert_eq!(tree.len(), before, "no node was added or removed");
            let label = text_of(&tree, shelf.row_status[0]);
            assert!(label.contains("Downloading 33%"), "{label}");
        });
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

    #[test]
    fn a_signed_out_shelf_offers_the_scan_guide() {
        with_language("zh", || {
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, _) = WereadShelfView::build(&mut text, &styles, &view(&[], false));
            let actions: Vec<ActionId> = (0..tree.len())
                .map(|n| NodeId(n as u32))
                .filter_map(|n| tree.node(n).action)
                .collect();
            assert!(actions.contains(&action::LOGIN));
            assert!(actions.contains(&action::LOCAL));
            assert!(actions.contains(&action::CLOUD));
        });
    }

    #[test]
    fn the_three_cells_are_all_on_the_screen() {
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0));
        let books = vec![book("a", false)];
        let (tree, _) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
        let actions: Vec<ActionId> = (0..tree.len())
            .map(|n| NodeId(n as u32))
            .filter_map(|n| tree.node(n).action)
            .collect();
        assert!(actions.contains(&action::LOCAL));
        assert!(actions.contains(&action::CLOUD));
        assert!(actions.contains(&action::SYNC));
    }

    #[test]
    fn progress_and_badges_are_written_out() {
        with_language("en", || {
            let mut listed = book("b", true);
            listed.progress = Some(37);
            let label = book_meta(&listed);
            assert!(label.contains("37%"));
            assert!(label.contains("List"));
            listed.progress = Some(100);
            listed.finished = true;
            assert_eq!(progress_label(&listed), tr(Msg::WereadProgressFinished));
            listed.finished = false;
            listed.progress = None;
            assert_eq!(progress_label(&listed), tr(Msg::WereadProgressUnread));
        });
    }

    /// AC-05's measurement: paging a 500-book shelf is one in-memory page
    /// change, with no file, network or lock. Run with
    /// `cargo test -p aristotle-ui --release paging_a_large_shelf -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn paging_a_large_shelf() {
        use std::time::Instant;
        let books: Vec<WereadBook> = (0..500).map(|i| book(&i.to_string(), false)).collect();
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0));
        let t0 = Instant::now();
        let (tree, _) = WereadShelfView::build(&mut text, &styles, &view(&books, true));
        let build = t0.elapsed();
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        let t1 = Instant::now();
        for i in 0..100 {
            let page = if i % 2 == 0 { 0 } else { 1 };
            let mut page_view = view(&books, true);
            page_view.page = page;
            let (tree, _) = WereadShelfView::build(&mut ui.text, &styles, &page_view);
            ui.set_tree_with(tree, Refresh::Auto);
            ui.layout();
        }
        let turns = t1.elapsed();
        println!(
            "500 books: first page {:?}, 100 page builds {:?} ({:?} each)",
            build,
            turns,
            turns / 100
        );
        assert!(turns.as_secs_f64() < 10.0);
    }

    #[test]
    fn an_empty_shelf_says_so() {
        with_language("en", || {
            let mut text = TextEngine::new(Fonts::embedded());
            let styles = Styles::new(FamilyId(0));
            let (tree, _) = WereadShelfView::build(&mut text, &styles, &view(&[], true));
            let found = (0..tree.len()).map(|n| NodeId(n as u32)).any(|n| {
                if let Kind::Text { para, .. } = &tree.node(n).kind {
                    para.spans.iter().any(|s| s.text == tr(Msg::WereadEmpty))
                } else {
                    false
                }
            });
            assert!(found);
        });
    }
}
