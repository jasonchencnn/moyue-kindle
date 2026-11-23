// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scan screen: empty library guide, active scan progress, and scan report summary.
//!
//! Visuals follow `prototype-v2/mcode/LibraryScan.dc.html` and `DESIGN-scan.md`.
//! Chinese text uses sans-serif (Noto Sans SC fallback).

use super::library::{
    segment_control_channels, status_line, ChannelVisibility, Segment, StatusLine,
};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::nav::{self, Tab};
use crate::screens::home::pet::CAT_MASK;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER};
use crate::tree::{
    pages, ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use crate::widgets::{Cover, GrayImage};
use aristotle_i18n::{tf, tr, trn, Msg, Plural};
use zeno::Transform;

pub mod action {
    use crate::tree::ActionId;
    /// Return to library (or the previous screen).
    pub const BACK: ActionId = 1;
    /// Start scanning from empty state.
    pub const START_SCAN: ActionId = 2;
    /// Open Wi-Fi transfer coming-soon page.
    pub const WIFI_TRANSFER: ActionId = 3;
    /// Cancel an in-progress scan or import.
    pub const CANCEL_SCAN: ActionId = 4;
    /// Interactive tap to complete scan (preview / device shortcut).
    pub const FINISH_SCAN: ActionId = 5;
    /// View library after a scan or import completes.
    pub const VIEW_LIBRARY: ActionId = 6;
    /// Run the scan of the chosen sources again.
    pub const RESCAN: ActionId = 7;
    /// Switch to the cloud shelf from the empty-library guide.
    pub const CLOUD_LIBRARY: ActionId = 8;
    /// Switch to the WeRead shelf from the empty-library guide.
    pub const WEREAD_LIBRARY: ActionId = 12;
    /// Switch to the Nutstore shelf from the empty-library guide.
    pub const JIANGUOYUN_LIBRARY: ActionId = 27;
    /// Open the source picker, where the folders to scan are chosen.
    pub const CHOOSE_SCOPE: ActionId = 9;
    /// Scan the chosen sources (the source picker's primary button).
    pub const SCAN_SCOPE: ActionId = 10;
    /// Step up to the parent folder.
    pub const SCOPE_PARENT: ActionId = 11;
    /// Toggle the whole user store as the source.
    pub const SOURCE_WHOLE: ActionId = 15;
    /// Toggle the folder the picker is showing as a source.
    pub const SOURCE_CURRENT: ActionId = 16;
    /// Clear every chosen source.
    pub const SOURCE_CLEAR: ActionId = 17;
    /// Open the destination picker (single-destination mode).
    pub const DEST_CHOOSE: ActionId = 18;
    /// Use the folder the destination picker is showing.
    pub const DEST_USE: ActionId = 19;
    /// Leave the destination picker without changing the destination.
    pub const DEST_CANCEL: ActionId = 20;
    /// Select every reviewable book on the review list.
    pub const REVIEW_ALL: ActionId = 21;
    /// Clear every review checkbox.
    pub const REVIEW_CLEAR: ActionId = 22;
    /// Import and move the checked books.
    pub const REVIEW_CONFIRM: ActionId = 23;
    /// Leave the review without importing anything.
    pub const REVIEW_CANCEL: ActionId = 24;
    /// The previous page of the review list.
    pub const REVIEW_PREV: ActionId = 25;
    /// The next page of the review list.
    pub const REVIEW_NEXT: ActionId = 26;
    /// The previous page of a folder list.
    pub const PREV_PAGE: ActionId = 13;
    /// The next page of a folder list.
    pub const NEXT_PAGE: ActionId = 14;
    /// The folder rows of a picker; `SCOPE_ITEM + i` opens the `i`th folder.
    ///
    /// The row bases are tagged high-bit namespaces with no row limit: a
    /// store with tens of thousands of candidates keeps every row usable,
    /// and no base can grow into another range.
    pub const SCOPE_ITEM: ActionId = 1 << 24;
    /// The source checkboxes; `SOURCE_ITEM + i` toggles the `i`th folder.
    pub const SOURCE_ITEM: ActionId = 2 << 24;
    /// The review rows; `REVIEW_ITEM + i` toggles the `i`th book.
    pub const REVIEW_ITEM: ActionId = 3 << 24;
}

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 104;
/// The size of the back button, in reference pixels.
const BACK_SIZE: i32 = 72;
/// The height of the card that names the folder to scan.
const SCOPE_CARD_H: i32 = 128;
/// The height of one folder row.
const SCOPE_ROW_H: i32 = 128;
/// The gap between two folder rows.
const SCOPE_ROW_GAP: i32 = 16;
/// The height of one review row.
const REVIEW_ROW_H: i32 = 136;
/// The gap between two review rows.
const REVIEW_ROW_GAP: i32 = 12;

/// A book found during scanning and offered in the review list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedBook {
    pub title: String,
    pub format: String,
}

/// One row of the combined review list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewBook {
    pub title: String,
    /// The path relative to the scan root, shown under the title.
    pub path: String,
    pub format: String,
    /// The index record, when the library already knows the file.
    pub id: Option<String>,
    /// The book already sits inside the physical library folder.
    pub in_destination: bool,
}

impl ReviewBook {
    /// Whether the row can be checked. A book already in the destination is
    /// listed for information only; a new or indexed book elsewhere can be
    /// imported or moved.
    pub fn selectable(&self) -> bool {
        // A new file already in the folder is still worth importing (it gets
        // an index entry); an indexed book there has nothing left to do.
        !(self.id.is_some() && self.in_destination)
    }
}

/// Which folder choice the picker is making.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickerMode {
    /// The folders to scan from; several may be checked.
    #[default]
    Sources,
    /// The physical library folder; one choice only.
    Destination,
}

/// A newly added book previewed in the done summary.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedBook {
    pub title: String,
    pub author: String,
    pub format: String,
    /// The book's own cover, when the scan made a thumbnail of it.
    pub cover: Option<GrayImage>,
}

/// One metric tile in the scan summary (3x2 grid).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanTile {
    pub label: String,
    pub value: String,
}

/// An error record in the scan report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    pub count: usize,
    pub filename: String,
    pub reason: String,
}

/// The report's "added" card, which also carries the restored books.
///
/// The prototype's cards are a 3×2 grid of six and this screen already fills
/// all six (added, updated, removed, ignored, imported progress, time), so
/// there is no slot for a fifth book card. A book moved away and back is
/// therefore counted with the added ones, and the label says so when some
/// came back: without that, a scan that only restored books looked like
/// nothing had changed.
pub fn added_tile(added: usize, restored: usize) -> ScanTile {
    let label = if restored > 0 {
        tr(Msg::LibraryScanStatAddedRestored)
    } else {
        tr(Msg::LibraryScanStatAdded)
    };
    ScanTile {
        label: label.to_string(),
        value: tr(Msg::LibraryScanValBooks).replace("{count}", &(added + restored).to_string()),
    }
}

/// The same grid with zeroes for a scan that could not run: the error card
/// says what happened, and "Scan again" retries (LIB-006).
pub fn failed_tiles() -> [ScanTile; 6] {
    let books = |label: Msg| ScanTile {
        label: tr(label).to_string(),
        value: tr(Msg::LibraryScanValBooks).replace("{count}", "0"),
    };
    [
        books(Msg::LibraryScanStatAdded),
        books(Msg::LibraryScanStatUpdated),
        books(Msg::LibraryScanStatRemoved),
        ScanTile {
            label: tr(Msg::LibraryScanStatIgnored).to_string(),
            value: tr(Msg::LibraryScanValItems).replace("{count}", "0"),
        },
        books(Msg::LibraryScanStatSidecars),
        ScanTile {
            label: tr(Msg::LibraryScanStatTime).to_string(),
            value: tr(Msg::LibraryScanValSecs).replace("{count}", "0"),
        },
    ]
}

/// The three states of the scan screen.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ScanState {
    /// Empty library onboarding guide with illustration and transfer cards.
    Empty,
    /// Background scanning in progress with progress bar and scanned books list.
    Scanning {
        path: String,
        subdirs: bool,
        found_count: usize,
        current_file: String,
        progress: f32,
        status: String,
        scanned_books: Vec<ScannedBook>,
    },
    /// The chosen sources and the library destination, before scanning.
    Sources {
        mode: PickerMode,
        /// The folder shown, relative to the scan root; empty for the root.
        path: String,
        /// The folders below the shown folder.
        folders: Vec<ScopeFolder>,
        /// Whether the shown folder can step up to its parent.
        can_go_up: bool,
        /// The chosen sources, relative to the scan root.
        selected: Vec<String>,
        /// The whole user store is the source instead of folders.
        whole: bool,
        /// The physical library folder, as a path to show.
        destination: String,
    },
    /// The combined list of found books. Nothing is imported until the
    /// reader checks rows and confirms.
    Review {
        /// A line naming the chosen sources.
        sources: String,
        /// The physical library folder the checked books move into.
        destination: String,
        books: Vec<ReviewBook>,
        /// One flag per book, in the same order.
        selected: Vec<bool>,
    },
    /// Selected books are being copied, verified and indexed on a worker.
    Importing {
        destination: String,
        done: usize,
        total: usize,
        status: String,
    },
    /// The import finished: what moved, what stayed and why.
    Imported {
        destination: String,
        migrated: usize,
        failed: usize,
        cancelled: bool,
        errors: Vec<String>,
    },
    /// Scan finished report with 6 stat tiles, errors, and preview of added books.
    Done {
        tiles: [ScanTile; 6],
        error: Option<ScanError>,
        added_books: Vec<AddedBook>,
    },
}

/// One folder of the scope picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeFolder {
    /// The name shown in the list.
    pub name: String,
    /// The path relative to the scan root, for the source checkmarks.
    pub relative: String,
    /// The books the index holds in the folder.
    pub books: usize,
}

impl ScanState {
    /// Whether this is the empty-library guide and the library has since
    /// gained `books`: the first scan filled it while the guide was on the
    /// screen, so the guide has to give way to the books.
    pub fn empty_guide_is_stale(&self, books: usize) -> bool {
        matches!(self, ScanState::Empty) && books > 0
    }
}

/// The complete model for the scan screen.
pub struct ScanScreen<'a> {
    pub status: StatusLine<'a>,
    pub state: ScanState,
    /// The local library's count, for the empty guide's "Local" segment.
    pub local_books_count: usize,
}

/// Handles to the nodes of the scan screen.
pub struct ScanUi {
    pub root: NodeId,
    pub loading: LoadingNodes,
    pub progress_node: Option<NodeId>,
    /// The pages of a folder list, in the source or destination picker.
    pub folder_list: Option<NodeId>,
    /// The page number of the folder list.
    pub folder_pager: Option<NodeId>,
    /// The pages of the review list.
    pub review_list: Option<NodeId>,
    /// The page number of the review list.
    pub review_pager: Option<NodeId>,
}

/// The fishing cat illustration for empty library:
/// 322 px wide by 280 px tall, centered horizontally.
pub struct EmptyLibraryIllustration;

impl Widget for EmptyLibraryIllustration {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(322, 280))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        let x = r.x;
        let y = r.y;
        // Fishing line from tip of rod to hook
        p.fill_rect(Rect::new(x + 284, y + 11, 2, 189), INK);
        // Desk cat mask
        p.blend_mask(x, y, 287, 280, CAT_MASK, INK);
        // Empty hook
        p.stroke_path(
            "M 13 8 L 13 19 C 13 26 5 26 5 20 L 2 16",
            2.0,
            Some(Transform::translation(x as f32 + 272.0, y as f32 + 192.0)),
            INK,
        );
        // Water ripples (cubic beziers matching quadratic prototype curves)
        p.stroke_path(
            "M 2 10 C 8 5.33 14 5.33 20 10 C 26 14.67 32 14.67 38 10 C 44 5.33 50 5.33 56 10 C 62 14.67 67 14.67 72 10 \
             M 12 32 C 18 27.33 24 27.33 30 32 C 36 36.67 42 36.67 48 32 C 54 27.33 60 27.33 66 32",
            2.0,
            Some(Transform::translation(x as f32 + 248.0, y as f32 + 228.0)),
            INK,
        );
    }
}

/// A square checkbox: an outline when off, a filled box with a tick when on,
/// gray when it cannot be changed.
pub struct CheckBox {
    pub checked: bool,
    pub enabled: bool,
}

impl Widget for CheckBox {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(44, 44))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        let ink = if self.enabled { INK } else { INK_MID };
        if self.checked {
            p.fill_round_rect(r, 8.0, ink);
            p.stroke_path(
                "M 11 23 L 19 31 L 33 15",
                5.0,
                Some(Transform::translation(r.x as f32, r.y as f32)),
                PAPER,
            );
        } else {
            p.stroke_round_rect(r, 8.0, 3.0, ink);
        }
    }
}

/// A 10 px high rounded progress bar for scanning.
pub struct ScanProgressBar {
    pub progress: f32,
}

impl Widget for ScanProgressBar {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(c.max.w, 10))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        p.fill_round_rect(r, 5.0, PAPER);
        let fill_w = ((r.w as f32) * self.progress.clamp(0.0, 1.0)).round() as i32;
        if fill_w > 0 {
            let fill_r = Rect::new(r.x, r.y, fill_w, r.h);
            p.fill_round_rect(fill_r, 5.0, INK);
        }
    }
}

impl ScanUi {
    /// Builds the scan screen with both Readest and WeRead library tabs shown
    /// on the empty-library guide.
    pub fn build(text: &mut TextEngine, s: &Styles, screen: &ScanScreen) -> (Tree, Self) {
        Self::build_with_sources(text, s, screen, true, true)
    }

    /// Builds the scan screen for a panel of `size` native pixels.
    pub fn build_for_size(
        text: &mut TextEngine,
        s: &Styles,
        screen: &ScanScreen,
        size: Size,
    ) -> (Tree, Self) {
        Self::build_with_sources_for_size(text, s, screen, true, true, size)
    }

    /// Builds the scan screen, showing Local plus whichever cloud libraries
    /// are enabled in Settings (`show_cloud`, `show_weread`) when the library
    /// is empty.
    pub fn build_with_sources(
        text: &mut TextEngine,
        s: &Styles,
        screen: &ScanScreen,
        show_cloud: bool,
        show_weread: bool,
    ) -> (Tree, Self) {
        Self::build_with_channels(
            text,
            s,
            screen,
            ChannelVisibility::readest_and_weread(show_cloud, show_weread),
        )
    }

    /// Builds the scan screen, showing Local plus whichever cloud libraries
    /// Settings turns on: Readest, WeRead and Nutstore.
    pub fn build_with_channels(
        text: &mut TextEngine,
        s: &Styles,
        screen: &ScanScreen,
        channels: ChannelVisibility,
    ) -> (Tree, Self) {
        Self::build_with_channels_for_size(
            text,
            s,
            screen,
            channels,
            Size::new(s.density.px(1264), s.density.px(1680)),
        )
    }

    /// Builds the scan screen for a panel of `size` native pixels, showing
    /// Local plus whichever cloud libraries are enabled in Settings.
    pub fn build_with_sources_for_size(
        _text: &mut TextEngine,
        s: &Styles,
        screen: &ScanScreen,
        show_cloud: bool,
        show_weread: bool,
        size: Size,
    ) -> (Tree, Self) {
        Self::build_with_channels_for_size(
            _text,
            s,
            screen,
            ChannelVisibility::readest_and_weread(show_cloud, show_weread),
            size,
        )
    }

    /// Builds the scan screen for a panel of `size` native pixels, showing
    /// Local plus whichever cloud libraries Settings turns on.
    pub fn build_with_channels_for_size(
        _text: &mut TextEngine,
        s: &Styles,
        screen: &ScanScreen,
        channels: ChannelVisibility,
        size: Size,
    ) -> (Tree, Self) {
        let compact = crate::theme::PanelLayout::for_size(size, s.density)
            == crate::theme::PanelLayout::Compact;
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &screen.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);

        // Header: Back button + Title "扫描本地书籍"
        let back_glyph = t.icon(Icon::ChevronLeft, 40, INK);
        let back = t.add(
            Node::new(Kind::MinSize(Size::new(BACK_SIZE, BACK_SIZE)))
                .action(action::BACK)
                .feedback(Feedback::Invert),
            [back_glyph],
        );
        let title_label = t.label(
            match screen.state {
                ScanState::Sources {
                    mode: PickerMode::Destination,
                    ..
                } => tr(Msg::LibraryScanDestinationTitle),
                ScanState::Sources { .. } => tr(Msg::LibraryScanSourceTitle),
                ScanState::Review { .. } => tr(Msg::LibraryScanReviewTitle),
                ScanState::Importing { .. } => tr(Msg::LibraryScanImportingTitle),
                ScanState::Imported { .. } => tr(Msg::LibraryScanImportedTitle),
                _ => tr(Msg::LibraryScanTitle),
            },
            s.font_lg().bold().line_height(1.2).tracking(4.0),
        );
        let header = t.add(Node::row(20).cross(Cross::Center), [back, title_label]);
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
        let header = t.fixed(None, Some(HEADER_H), Some(header));

        // The empty guide keeps the same Local / enabled cloud sources control
        // as the populated library shelf so a new or wiped device can directly
        // enter Readest, WeRead or Nutstore without first scanning local files.
        let mut parts = vec![status, header];
        if matches!(screen.state, ScanState::Empty) && channels.count() > 0 {
            let seg_box = segment_control_channels(
                &mut t,
                s,
                screen.local_books_count,
                Segment::Local,
                None,
                channels.readest.then_some(action::CLOUD_LIBRARY),
                channels.weread.then_some(action::WEREAD_LIBRARY),
                channels.jianguoyun.then_some(action::JIANGUOYUN_LIBRARY),
                channels,
            );
            let seg_box = t.fixed(None, Some(76), Some(seg_box));
            let seg_control = t.pad(Insets::new(0, home_margin(), 28, home_margin()), seg_box);
            parts.push(seg_control);
        }

        let mut progress_node = None;
        let mut folder_list = None;
        let mut folder_pager = None;
        let mut review_list = None;
        let mut review_pager = None;
        let content_node = match &screen.state {
            ScanState::Empty => Self::build_empty(&mut t, s, compact),
            ScanState::Scanning {
                path,
                subdirs,
                found_count,
                current_file,
                progress,
                status,
                scanned_books,
            } => {
                let (node, pb) = Self::build_scanning(
                    &mut t,
                    s,
                    path,
                    *subdirs,
                    *found_count,
                    current_file,
                    *progress,
                    status,
                    scanned_books,
                    compact,
                );
                progress_node = Some(pb);
                node
            }
            ScanState::Sources {
                mode,
                path,
                folders,
                can_go_up,
                selected,
                whole,
                destination,
            } => {
                let (node, list, pager) = Self::build_sources(
                    &mut t,
                    s,
                    *mode,
                    path,
                    folders,
                    *can_go_up,
                    selected,
                    *whole,
                    destination,
                );
                folder_list = Some(list);
                folder_pager = Some(pager);
                node
            }
            ScanState::Review {
                sources,
                destination,
                books,
                selected,
            } => {
                let (node, list, pager) =
                    Self::build_review(&mut t, s, sources, destination, books, selected);
                review_list = Some(list);
                review_pager = Some(pager);
                node
            }
            ScanState::Importing {
                destination,
                done,
                total,
                status,
            } => Self::build_importing(&mut t, s, destination, *done, *total, status),
            ScanState::Imported {
                destination,
                migrated,
                failed,
                cancelled,
                errors,
            } => Self::build_imported(
                &mut t,
                s,
                destination,
                *migrated,
                *failed,
                *cancelled,
                errors,
            ),
            ScanState::Done {
                tiles,
                error,
                added_books,
            } => Self::build_done(&mut t, s, tiles, error.as_ref(), added_books, compact),
        };

        let content_area = t.pad(
            Insets::new(
                0,
                home_margin(),
                if compact { 14 } else { 28 },
                home_margin(),
            ),
            content_node,
        );
        let content_area = t.add(Node::column(0).flex(1), [content_area]);

        // Bottom nav bar with Tab::Library selected
        let nav_bar = nav::build(&mut t, s, Tab::Library);

        parts.extend([content_area, nav_bar.root]);
        let root = t.add(Node::column(0), parts);
        t.set_root(root);
        (
            t,
            ScanUi {
                root,
                loading: bar.loading,
                progress_node,
                folder_list,
                folder_pager,
                review_list,
                review_pager,
            },
        )
    }

    /// Build State 1: Empty library onboarding guide.
    fn build_empty(t: &mut Tree, s: &Styles, compact: bool) -> NodeId {
        let ill = t.custom(EmptyLibraryIllustration);
        let ill = if compact {
            t.fixed(Some(200), Some(174), Some(ill))
        } else {
            t.fixed(Some(322), Some(280), Some(ill))
        };

        // The body is a centered column, so the labels centre themselves:
        // an Align child here would take the column's remaining height and
        // squeeze its siblings to zero (QA-101 P1-1).
        let title = t.label(
            tr(Msg::LibraryScanEmptyTitle),
            s.font_lg().bold().line_height(1.3),
        );

        let desc = t.label(
            tr(Msg::LibraryScanEmptyDesc),
            s.font_sm()
                .gray(INK_MID)
                .line_height(if compact { 1.35 } else { 1.5 }),
        );

        // Black primary button "扫描本地书籍"
        let btn_text = t.label(
            tr(Msg::LibraryScanButtonStart),
            s.font_base().bold().gray(PAPER),
        );
        let btn_align = t.align(0.5, 0.5, btn_text);
        let btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(if compact { 80 } else { 96 }),
            })
            .bg(INK)
            .border(0, INK, 20)
            .action(action::START_SCAN)
            .feedback(Feedback::Invert),
            [btn_align],
        );

        // Two side-by-side cards: Wi-Fi Transfer & USB Connection
        let wifi_icon = t.icon(Icon::Upload, 36, INK);
        let wifi_icon_align = t.align(0.5, 0.5, wifi_icon);
        let wifi_icon_box = t.add(
            Node::new(Kind::Fixed {
                w: Some(64),
                h: Some(64),
            })
            .bg(PAPER)
            .border(0, PAPER, 20),
            [wifi_icon_align],
        );
        let wifi_title = t.label(tr(Msg::LibraryWifiTransfer), s.font_sm().bold());
        let wifi_spacer = t.spacer(1);
        let wifi_arrow = t.icon(Icon::ChevronRight, 28, INK);
        let wifi_top = t.add(
            Node::row(16).cross(Cross::Center),
            [wifi_icon_box, wifi_title, wifi_spacer, wifi_arrow],
        );
        let wifi_sub = t.label(
            tr(Msg::LibraryScanWifiDesc),
            s.font_xs().gray(INK_MID).line_height(1.3),
        );
        let wifi_card_content = t.add(
            Node::column(if compact { 8 } else { 12 }),
            [wifi_top, wifi_sub],
        );
        let card_pad = if compact {
            Insets::new(14, 20, 14, 20)
        } else {
            Insets::new(24, 28, 24, 28)
        };
        let wifi_card_pad = t.pad(card_pad, wifi_card_content);
        let wifi_card = t.add(
            Node::column(0)
                .bg(CARD)
                .border(0, 0, 24)
                .action(action::WIFI_TRANSFER)
                .feedback(Feedback::Invert)
                .flex(1),
            [wifi_card_pad],
        );

        let usb_icon = t.icon(Icon::Usb, 36, INK);
        let usb_icon_align = t.align(0.5, 0.5, usb_icon);
        let usb_icon_box = t.add(
            Node::new(Kind::Fixed {
                w: Some(64),
                h: Some(64),
            })
            .bg(PAPER)
            .border(0, PAPER, 20),
            [usb_icon_align],
        );
        let usb_title = t.label(tr(Msg::LibraryScanUsbTitle), s.font_sm().bold());
        let usb_top = t.add(
            Node::row(16).cross(Cross::Center),
            [usb_icon_box, usb_title],
        );
        let usb_sub = t.label(
            tr(Msg::LibraryScanUsbDesc),
            s.font_xs().gray(INK_MID).line_height(1.3),
        );
        let usb_card_content = t.add(
            Node::column(if compact { 8 } else { 12 }),
            [usb_top, usb_sub],
        );
        let usb_card_pad = t.pad(card_pad, usb_card_content);
        let usb_card = t.add(
            Node::column(0).bg(CARD).border(0, 0, 24).flex(1),
            [usb_card_pad],
        );

        let cards_row = t.add(
            Node::row(if compact { 16 } else { 24 }),
            [wifi_card, usb_card],
        );

        // Supported formats section
        let formats_label = t.label(
            tr(Msg::LibraryScanSupportedFormats),
            s.font_xs().gray(INK_MID),
        );
        let format_names = ["EPUB", "MOBI / AZW3", "PDF", "TXT", "CBZ"];
        let mut chips = Vec::new();
        for f in format_names {
            let label = t.label(f, s.font_xs().bold().line_height(1.0));
            let pad = t.pad(
                Insets::new(
                    0,
                    if compact { 14 } else { 22 },
                    0,
                    if compact { 14 } else { 22 },
                ),
                label,
            );
            // MinSize shrink-wraps the label and gives it the 64 px height;
            // a Fixed box would stretch the first chip across the row.
            let chip = t.add(
                Node::new(Kind::MinSize(Size::new(0, if compact { 52 } else { 64 })))
                    .bg(PAPER)
                    .border(2, INK, 16),
                [pad],
            );
            chips.push(chip);
        }
        let chips_row = t.add(Node::row(if compact { 10 } else { 14 }), chips);
        let formats_col = t.add(
            Node::column(if compact { 10 } else { 16 }),
            [formats_label, chips_row],
        );

        let g1 = t.gap(1, if compact { 16 } else { 40 });
        let g2 = t.gap(1, if compact { 10 } else { 16 });
        let g3 = t.gap(1, if compact { 18 } else { 40 });
        let g4 = t.gap(1, if compact { 14 } else { 24 });
        let g5 = t.gap(1, if compact { 14 } else { 28 });

        // Group into centered column
        let body = t.add(
            Node::column(0).cross(Cross::Center),
            [
                ill,
                g1,
                title,
                g2,
                desc,
                g3,
                btn,
                g4,
                cards_row,
                g5,
                formats_col,
            ],
        );
        t.align(0.5, 0.5, body)
    }

    /// Build the source picker (several folders, or the whole store) or the
    /// destination picker (one folder), with the folders below the shown one.
    ///
    /// The list pages like the library's: an e-ink list that scrolled would
    /// cost a full update per step and leave ghosting.
    #[allow(clippy::too_many_arguments)]
    fn build_sources(
        t: &mut Tree,
        s: &Styles,
        mode: PickerMode,
        path: &str,
        folders: &[ScopeFolder],
        can_go_up: bool,
        selected: &[String],
        whole: bool,
        destination: &str,
    ) -> (NodeId, NodeId, NodeId) {
        let destination_mode = mode == PickerMode::Destination;
        // The folder shown, which may be chosen as a source or destination.
        let current_value = if path.is_empty() {
            tr(Msg::LibraryScanScopeWhole).to_string()
        } else {
            path.to_string()
        };
        let card_label = if destination_mode {
            tr(Msg::LibraryScanCurrentFolder)
        } else {
            tr(Msg::LibraryScanSourceLabel)
        };
        let label = t.label(card_label, s.font_xs().gray(INK_MID));
        let mut value_par = Paragraph::plain(
            current_value,
            s.font_base().bold().line_height(1.3),
            Align::Left,
        );
        value_par.style.max_lines = Some(1);
        let value = t.text(value_par);
        let col = t.add(Node::column(8).cross(Cross::Start), [label, value]);
        let pad = t.pad(Insets::new(24, 28, 24, 28), col);
        let card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [pad]);
        let card = t.fixed(None, Some(SCOPE_CARD_H), Some(card));

        let mut parts = vec![card];

        // The whole-store choice sits above the folders: it is the usual
        // scan for a reader who copied books anywhere on the device.
        if !destination_mode {
            // Where a confirmed import will move the books, and the way to
            // choose another folder. Choosing it is not import consent.
            let dest_label = t.label(
                tr(Msg::LibraryScanDestinationLabel),
                s.font_xs().gray(INK_MID),
            );
            let mut dest_par = Paragraph::plain(
                destination.to_string(),
                s.font_xs().line_height(1.2),
                Align::Left,
            );
            dest_par.style.max_lines = Some(1);
            let dest_text = t.text(dest_par);
            let dest_words = t.add(Node::column(2).flex(1), [dest_label, dest_text]);
            let change_label = t.label(tr(Msg::LibraryScanDestinationChange), s.font_xs());
            let change_pad = t.pad(Insets::vh(24, 20), change_label);
            let change = t.add(
                Node::new(Kind::MinSize(Size::new(64, 64)))
                    .action(action::DEST_CHOOSE)
                    .feedback(Feedback::Invert),
                [change_pad],
            );
            let dest_row = t.add(Node::row(16).cross(Cross::Center), [dest_words, change]);
            let dest_pad = t.pad(Insets::new(10, 24, 10, 24), dest_row);
            let dest_card = t.add(Node::column(0).bg(CARD).border(0, 0, 20), [dest_pad]);
            let dest_card = t.fixed(None, Some(88), Some(dest_card));
            parts.push(t.gap(1, 8));
            parts.push(dest_card);
        }

        if !destination_mode {
            let check = t.custom(CheckBox {
                checked: whole,
                enabled: true,
            });
            let check_box = t.fixed(Some(64), Some(64), Some(check));
            let title = t.label(tr(Msg::LibraryScanWhole), s.font_base().bold());
            let desc = t.label(
                tr(Msg::LibraryScanWholeDesc),
                s.font_xs().gray(INK_MID).line_height(1.3),
            );
            let words = t.add(Node::column(4).cross(Cross::Start).flex(1), [title, desc]);
            let row = t.add(Node::row(20).cross(Cross::Center), [check_box, words]);
            let row_pad = t.pad(Insets::new(18, 24, 18, 24), row);
            let card = t.add(Node::column(0).bg(CARD).border(0, 0, 20), [row_pad]);
            let card = t.fixed(None, Some(120), Some(card));
            parts.push(t.gap(1, 12));
            parts.push(
                t.add(
                    Node::new(Kind::MinSize(Size::new(0, 120)))
                        .action(action::SOURCE_WHOLE)
                        .feedback(Feedback::Invert)
                        .full_width_invert(true),
                    [card],
                ),
            );
        }

        // The rows: the parent folder, then the folders below the shown one.
        let mut rows: Vec<NodeId> = Vec::new();
        if can_go_up {
            rows.push(Self::scope_row(
                t,
                s,
                tr(Msg::LibraryScanScopeParent),
                None,
                Some(action::SCOPE_PARENT),
                None,
                None,
            ));
        }
        for (i, folder) in folders.iter().enumerate() {
            let books = trn(Plural::LibraryBooks, folder.books);
            let check = (!destination_mode).then(|| {
                (
                    selected.iter().any(|p| p == &folder.relative),
                    action::SOURCE_ITEM + i as ActionId,
                )
            });
            rows.push(Self::scope_row(
                t,
                s,
                &folder.name,
                Some(&books),
                Some(action::SCOPE_ITEM + i as ActionId),
                check,
                None,
            ));
        }
        if rows.is_empty() {
            let empty = t.label(tr(Msg::LibraryScanScopeEmpty), s.font_sm().gray(INK_MID));
            rows.push(t.fixed(None, Some(SCOPE_ROW_H), Some(empty)));
        }

        // The pages node paginates the rows itself, by the height the
        // layout gives it: a fixed row count can overflow the panel or draw
        // rows from two pages together.
        let mut items = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                items.push(t.gap(1, SCOPE_ROW_GAP));
            }
            items.push(*row);
        }
        let list = t.add(pages(0).flex(1), items);
        let page_count = 1;
        parts.push(t.gap(1, 12));
        parts.push(list);

        // The pager: the page number, and the two buttons.
        let prev = super::icon_button(t, Icon::ChevronLeft, 44, action::PREV_PAGE);
        let next = super::icon_button(t, Icon::ChevronRight, 44, action::NEXT_PAGE);
        let page_of = tf!(Msg::LibraryPagerPageOf, page = 1, count = page_count);
        let pager_label = t.text(Paragraph::plain(
            page_of,
            s.font_sm().line_height(1.3),
            Align::Center,
        ));
        let pager_center = t.add(Node::column(0).cross(Cross::Center).flex(1), [pager_label]);
        let pager_row = t.add(
            Node::row(0).cross(Cross::Center),
            [prev, pager_center, next],
        );
        let pager_fixed = t.fixed(None, Some(88), Some(pager_row));
        parts.push(pager_fixed);

        if destination_mode {
            // One explicit choice, and a way back without choosing.
            let use_text = t.label(
                tr(Msg::LibraryScanDestinationUse),
                s.font_base().bold().gray(PAPER),
            );
            let use_align = t.align(0.5, 0.5, use_text);
            let use_btn = t.add(
                Node::new(Kind::Fixed {
                    w: None,
                    h: Some(96),
                })
                .bg(INK)
                .border(0, INK, 20)
                .action(action::DEST_USE)
                .feedback(Feedback::Invert)
                .flex(1),
                [use_align],
            );
            let cancel_text = t.label(tr(Msg::LibraryScanDestinationCancel), s.font_base());
            let cancel_align = t.align(0.5, 0.5, cancel_text);
            let cancel_btn = t.add(
                Node::new(Kind::Fixed {
                    w: None,
                    h: Some(96),
                })
                .bg(PAPER)
                .border(2, INK, 20)
                .action(action::DEST_CANCEL)
                .feedback(Feedback::Invert)
                .flex(1),
                [cancel_align],
            );
            parts.push(t.gap(1, 12));
            parts.push(t.add(Node::row(20), [use_btn, cancel_btn]));
        } else {
            // The current folder itself can be a source, then the summary
            // with its count and the clear control, then the scan button.
            let current_checked = selected.iter().any(|p| p == path);
            let current_text = t.label(
                if current_checked {
                    tr(Msg::LibraryScanSourceRemoveCurrent)
                } else {
                    tr(Msg::LibraryScanSourceCurrent)
                },
                s.font_base(),
            );
            let current_align = t.align(0.5, 0.5, current_text);
            let current_btn = t.add(
                Node::new(Kind::Fixed {
                    w: None,
                    h: Some(72),
                })
                .bg(PAPER)
                .border(2, INK, 20)
                .action(action::SOURCE_CURRENT)
                .feedback(Feedback::Invert),
                [current_align],
            );
            parts.push(t.gap(1, 8));
            parts.push(current_btn);

            let count = if whole { 1 } else { selected.len() };
            let summary = if count == 0 {
                tr(Msg::LibraryScanSourceNone).to_string()
            } else if whole {
                tr(Msg::LibraryScanWhole).to_string()
            } else {
                tf!(Msg::LibraryScanSourceSelected, count = count)
            };
            let summary_text = t.label(&summary, s.font_xs().gray(INK_MID));
            let clear_label = t.label(tr(Msg::LibraryScanSourceClear), s.font_xs());
            let clear_pad = t.pad(Insets::vh(24, 20), clear_label);
            let clear = t.add(
                Node::new(Kind::MinSize(Size::new(64, 64)))
                    .action(action::SOURCE_CLEAR)
                    .feedback(Feedback::Invert),
                [clear_pad],
            );
            let summary_spacer = t.spacer(1);
            let summary_row = t.add(
                Node::row(16).cross(Cross::Center),
                [summary_text, summary_spacer, clear],
            );

            let start_text = t.label(
                tr(Msg::LibraryScanSourceStart),
                s.font_base().bold().gray(PAPER),
            );
            let start_align = t.align(0.5, 0.5, start_text);
            let mut start = Node::new(Kind::Fixed {
                w: None,
                h: Some(96),
            })
            .bg(INK)
            .border(0, INK, 20);
            if count > 0 {
                start = start.action(action::SCAN_SCOPE).feedback(Feedback::Invert);
            }
            let start = t.add(start, [start_align]);
            parts.push(t.gap(1, 4));
            parts.push(summary_row);
            parts.push(t.gap(1, 8));
            parts.push(start);
        }

        (t.add(Node::column(0), parts), list, pager_label)
    }

    /// One row of a folder list: a folder icon, the name, the number of
    /// books, an optional checkbox and a chevron.
    fn scope_row(
        t: &mut Tree,
        s: &Styles,
        name: &str,
        books: Option<&str>,
        open: Option<ActionId>,
        check: Option<(bool, ActionId)>,
        badge: Option<&str>,
    ) -> NodeId {
        let mut columns = Vec::new();
        if let Some((checked, id)) = check {
            let box_node = t.custom(CheckBox {
                checked,
                enabled: true,
            });
            let box_node = t.fixed(Some(64), Some(64), Some(box_node));
            columns.push(
                t.add(
                    Node::new(Kind::MinSize(Size::new(64, SCOPE_ROW_H)))
                        .action(id)
                        .feedback(Feedback::Invert),
                    [box_node],
                ),
            );
        } else {
            let icon = t.icon(Icon::Folder, 48, INK);
            let icon_align = t.align(0.5, 0.5, icon);
            columns.push(t.fixed(Some(64), Some(64), Some(icon_align)));
        }

        let mut tp = Paragraph::plain(name, s.font_base().bold().line_height(1.25), Align::Left);
        tp.style.max_lines = Some(1);
        let title = t.text(tp);
        let mut texts = vec![title];
        if let Some(books) = books {
            let mut sp = Paragraph::plain(
                books,
                s.font_xs().gray(INK_MID).line_height(1.3),
                Align::Left,
            );
            sp.style.max_lines = Some(1);
            texts.push(t.text(sp));
        }
        if let Some(badge) = badge {
            texts.push(t.label(badge, s.font_xs().gray(INK_MID)));
        }
        let words = t.add(Node::column(6).cross(Cross::Start).flex(1), texts);
        let words = if let Some(open) = open {
            t.add(
                Node::new(Kind::MinSize(Size::new(0, SCOPE_ROW_H)))
                    .action(open)
                    .feedback(Feedback::Invert)
                    .flex(1),
                [words],
            )
        } else {
            words
        };
        columns.push(words);

        if open.is_some() {
            let chevron = t.icon(Icon::ChevronRight, 40, INK_MID);
            let chevron_align = t.align(0.5, 0.5, chevron);
            columns.push(t.fixed(Some(64), Some(64), Some(chevron_align)));
        }

        let row = t.add(Node::row(16).cross(Cross::Center), columns);
        let row_pad = t.pad(Insets::new(16, 24, 16, 24), row);
        let card = t.add(Node::column(0).bg(CARD).border(0, 0, 20), [row_pad]);
        t.fixed(None, Some(SCOPE_ROW_H), Some(card))
    }

    /// Build the combined review: what was found, which rows are checked,
    /// and the explicit confirmation that moves them into the library folder.
    fn build_review(
        t: &mut Tree,
        s: &Styles,
        sources: &str,
        destination: &str,
        books: &[ReviewBook],
        selected: &[bool],
    ) -> (NodeId, NodeId, NodeId) {
        let checked = selected.iter().filter(|&&on| on).count();

        // What confirmation will do, in one card: the sources that were
        // searched and the folder the checked files move into.
        let source_label = t.label(tr(Msg::LibraryScanReviewSources), s.font_xs().gray(INK_MID));
        let mut source_par = Paragraph::plain(
            sources.to_string(),
            s.font_sm().bold().line_height(1.25),
            Align::Left,
        );
        source_par.style.max_lines = Some(1);
        let source_text = t.text(source_par);
        let dest_label = t.label(
            tr(Msg::LibraryScanReviewDestination),
            s.font_xs().gray(INK_MID),
        );
        let mut dest_par = Paragraph::plain(
            destination.to_string(),
            s.font_xs().line_height(1.25),
            Align::Left,
        );
        dest_par.style.max_lines = Some(1);
        let dest_text = t.text(dest_par);
        let info = t.add(
            Node::column(4).cross(Cross::Start),
            [source_label, source_text, dest_label, dest_text],
        );
        let info_pad = t.pad(Insets::new(18, 24, 18, 24), info);
        let info_card = t.add(Node::column(0).bg(CARD).border(0, 0, 20), [info_pad]);
        let info_card = t.fixed(None, Some(210), Some(info_card));

        // Selection toolbar: select all, clear, and the count.
        let all_text = t.label(tr(Msg::LibraryScanReviewSelectAll), s.font_xs());
        let all_pad = t.pad(Insets::vh(24, 20), all_text);
        let all = t.add(
            Node::new(Kind::MinSize(Size::new(64, 64)))
                .action(action::REVIEW_ALL)
                .feedback(Feedback::Invert),
            [all_pad],
        );
        let clear_text = t.label(tr(Msg::LibraryScanReviewClear), s.font_xs());
        let clear_pad = t.pad(Insets::vh(24, 20), clear_text);
        let clear = t.add(
            Node::new(Kind::MinSize(Size::new(64, 64)))
                .action(action::REVIEW_CLEAR)
                .feedback(Feedback::Invert),
            [clear_pad],
        );
        let count_str = tf!(Msg::LibraryScanReviewSelected, count = checked);
        let count = t.label(&count_str, s.font_xs().gray(INK_MID));
        let spacer = t.spacer(1);
        let toolbar = t.add(
            Node::row(20).cross(Cross::Center),
            [all, clear, spacer, count],
        );

        // The rows, paged like the library.
        let mut rows: Vec<NodeId> = Vec::new();
        for (i, book) in books.iter().enumerate() {
            let on = selected.get(i).copied().unwrap_or(false);
            rows.push(Self::review_row(
                t,
                s,
                book,
                on,
                action::REVIEW_ITEM + i as ActionId,
            ));
        }
        let mut items = Vec::new();
        if rows.is_empty() {
            let empty = t.label(tr(Msg::LibraryScanReviewNone), s.font_sm().gray(INK_MID));
            items.push(t.pad(Insets::all(24), empty));
        } else {
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    items.push(t.gap(1, REVIEW_ROW_GAP));
                }
                items.push(*row);
            }
        }
        let list = t.add(pages(0).flex(1), items);
        let page_count = 1;

        // The pager.
        let prev = super::icon_button(t, Icon::ChevronLeft, 44, action::REVIEW_PREV);
        let next = super::icon_button(t, Icon::ChevronRight, 44, action::REVIEW_NEXT);
        let page_of = tf!(Msg::LibraryPagerPageOf, page = 1, count = page_count);
        let pager_label = t.text(Paragraph::plain(
            page_of,
            s.font_sm().line_height(1.3),
            Align::Center,
        ));
        let pager_center = t.add(Node::column(0).cross(Cross::Center).flex(1), [pager_label]);
        let pager_row = t.add(
            Node::row(0).cross(Cross::Center),
            [prev, pager_center, next],
        );
        let pager_fixed = t.fixed(None, Some(88), Some(pager_row));

        // Confirmation, and a way back that imports nothing.
        let confirm_str = if checked == 0 {
            tr(Msg::LibraryScanReviewConfirmNone).to_string()
        } else {
            tf!(Msg::LibraryScanReviewConfirm, count = checked)
        };
        let confirm_text = t.label(
            &confirm_str,
            if checked == 0 {
                s.font_sm().gray(INK_MID)
            } else {
                s.font_sm().bold().gray(PAPER)
            },
        );
        let confirm_align = t.align(0.5, 0.5, confirm_text);
        let mut confirm = Node::new(Kind::Fixed {
            w: None,
            h: Some(96),
        })
        .bg(if checked == 0 { PAPER } else { INK })
        .border(if checked == 0 { 2 } else { 0 }, INK, 20)
        .flex(1);
        if checked > 0 {
            confirm = confirm
                .action(action::REVIEW_CONFIRM)
                .feedback(Feedback::Invert);
        }
        let confirm = t.add(confirm, [confirm_align]);

        let cancel_text = t.label(tr(Msg::LibraryScanReviewCancel), s.font_base());
        let cancel_align = t.align(0.5, 0.5, cancel_text);
        let cancel = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(96),
            })
            .bg(PAPER)
            .border(2, INK, 20)
            .action(action::REVIEW_CANCEL)
            .feedback(Feedback::Invert)
            .flex(1),
            [cancel_align],
        );
        let buttons = t.add(Node::row(20), [cancel, confirm]);

        let body = t.add(
            Node::column(12),
            [info_card, toolbar, list, pager_fixed, buttons],
        );
        (body, list, pager_label)
    }

    /// One row of the review list: a checkbox, the title, the relative path,
    /// the format and, for books already in the library, a badge.
    fn review_row(
        t: &mut Tree,
        s: &Styles,
        book: &ReviewBook,
        checked: bool,
        id: ActionId,
    ) -> NodeId {
        let selectable = book.selectable();
        let check = t.custom(CheckBox {
            checked,
            enabled: selectable,
        });
        let check = t.fixed(Some(64), Some(64), Some(check));

        let mut title_par = Paragraph::plain(
            &book.title,
            s.font_sm().bold().line_height(1.2),
            Align::Left,
        );
        title_par.style.max_lines = Some(1);
        let title = t.text(title_par);
        let mut path_par = Paragraph::plain(
            &book.path,
            s.font_xs().gray(INK_MID).line_height(1.2),
            Align::Left,
        );
        path_par.style.max_lines = Some(1);
        let path = t.text(path_par);
        let mut texts = vec![title, path];
        if !selectable {
            texts.push(t.label(
                tr(Msg::LibraryScanReviewInDestination),
                s.font_xs().gray(INK_MID),
            ));
        } else if book.id.is_some() {
            texts.push(t.label(tr(Msg::LibraryScanReviewIndexed), s.font_xs().gray(INK_MID)));
        }
        let words = t.add(Node::column(4).cross(Cross::Start).flex(1), texts);

        let format = t.label(&book.format, s.font_xs().gray(INK_MID));
        let format_pad = t.pad(Insets::new(8, 16, 8, 16), format);
        let format_box = t.add(
            Node::new(Kind::MinSize(Size::new(0, 48)))
                .bg(PAPER)
                .border(2, INK, 12),
            [format_pad],
        );

        let row = t.add(
            Node::row(16).cross(Cross::Center),
            [check, words, format_box],
        );
        let row_pad = t.pad(Insets::new(10, 20, 10, 20), row);
        let card = t.add(Node::column(0).bg(CARD).border(0, 0, 18), [row_pad]);
        let card = t.fixed(None, Some(REVIEW_ROW_H), Some(card));
        if selectable {
            t.add(
                Node::new(Kind::MinSize(Size::new(0, REVIEW_ROW_H)))
                    .action(id)
                    .feedback(Feedback::Invert)
                    .full_width_invert(true),
                [card],
            )
        } else {
            card
        }
    }

    /// Build the import in progress: the selected files are copied and
    /// verified on a worker, so reading and taps stay responsive.
    fn build_importing(
        t: &mut Tree,
        s: &Styles,
        destination: &str,
        done: usize,
        total: usize,
        status: &str,
    ) -> NodeId {
        let refresh_icon = t.icon(Icon::Refresh, 36, INK);
        let title = t.label(tr(Msg::LibraryScanImportingTitle), s.font_sm().bold());
        let top = t.add(Node::row(14).cross(Cross::Center), [refresh_icon, title]);
        let dest = t.label(
            &tf!(Msg::LibraryScanImportingDest, path = destination),
            s.font_xs().gray(INK_MID),
        );
        let progress = if total > 0 {
            done as f32 / total as f32
        } else {
            0.0
        };
        let bar = t.custom(ScanProgressBar { progress });
        let bar = t.fixed(None, Some(10), Some(bar));
        let count = t.label(
            &tf!(
                Msg::LibraryScanImportingProgress,
                done = done,
                total = total
            ),
            s.font_md().bold(),
        );
        let status = t.label(status, s.font_xs().gray(INK_MID));

        let g = t.gap(1, 20);
        let g_dest = t.gap(1, 24);
        let g_count = t.gap(1, 12);
        let g_bar = t.gap(1, 12);
        let inner = t.add(
            Node::column(0),
            [top, g, dest, g_dest, count, g_count, bar, g_bar, status],
        );
        let pad = t.pad(Insets::all(28), inner);
        let card = t.add(Node::column(0).bg(CARD).border(0, 0, 28), [pad]);

        let cancel_text = t.label(tr(Msg::LibraryScanCancel), s.font_base());
        let cancel_align = t.align(0.5, 0.5, cancel_text);
        let cancel = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(96),
            })
            .bg(PAPER)
            .border(2, INK, 20)
            .action(action::CANCEL_SCAN)
            .feedback(Feedback::Invert),
            [cancel_align],
        );
        let gap = t.gap(1, 24);
        let spacer = t.spacer(1);
        t.add(Node::column(0).flex(1), [card, gap, spacer, cancel])
    }

    /// Build the import report: how many books moved, and what stayed behind
    /// with the reason.
    fn build_imported(
        t: &mut Tree,
        s: &Styles,
        destination: &str,
        migrated: usize,
        failed: usize,
        cancelled: bool,
        errors: &[String],
    ) -> NodeId {
        let moved = t.label(
            &tf!(Msg::LibraryScanImportedMoved, count = migrated),
            s.font_md().bold(),
        );
        let dest = t.label(
            &tf!(Msg::LibraryScanImportedDest, path = destination),
            s.font_xs().gray(INK_MID),
        );
        let mut parts = vec![moved, t.gap(1, 8), dest];
        if cancelled {
            parts.push(t.gap(1, 12));
            parts.push(t.label(
                tr(Msg::LibraryScanImportCancelled),
                s.font_sm().gray(INK_MID),
            ));
        }
        if failed > 0 {
            parts.push(t.gap(1, 12));
            parts.push(t.label(
                &tf!(Msg::LibraryScanImportedFailed, count = failed),
                s.font_sm(),
            ));
            parts.push(t.label(
                tr(Msg::LibraryScanImportedRetained),
                s.font_xs().gray(INK_MID),
            ));
            for error in errors.iter().take(3) {
                let mut line = Paragraph::plain(
                    error,
                    s.font_xs().gray(INK_MID).line_height(1.2),
                    Align::Left,
                );
                line.style.max_lines = Some(2);
                parts.push(t.text(line));
            }
        }
        let card_inner = t.add(Node::column(0), parts);
        let card_pad = t.pad(Insets::all(28), card_inner);
        let card = t.add(Node::column(0).bg(CARD).border(0, 0, 28), [card_pad]);

        let view_text = t.label(
            tr(Msg::LibraryScanViewLibrary),
            s.font_base().bold().gray(PAPER),
        );
        let view_align = t.align(0.5, 0.5, view_text);
        let view = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(96),
            })
            .bg(INK)
            .border(0, INK, 20)
            .action(action::VIEW_LIBRARY)
            .feedback(Feedback::Invert),
            [view_align],
        );
        let gap = t.gap(1, 24);
        let spacer = t.spacer(1);
        t.add(Node::column(0).flex(1), [card, gap, spacer, view])
    }

    /// Build State 2: Background scanning in progress.
    #[allow(clippy::too_many_arguments)]
    fn build_scanning(
        t: &mut Tree,
        s: &Styles,
        path: &str,
        subdirs: bool,
        found_count: usize,
        current_file: &str,
        progress: f32,
        status: &str,
        scanned_books: &[ScannedBook],
        compact: bool,
    ) -> (NodeId, NodeId) {
        // Progress Card
        let refresh_icon = t.icon(Icon::Refresh, 36, INK);
        let in_progress_label = t.label(tr(Msg::LibraryScanInProgress), s.font_sm().bold());
        let top_row = t.add(
            Node::row(14).cross(Cross::Center),
            [refresh_icon, in_progress_label],
        );

        let path_label = t.label(path, s.font_base());
        let subdirs_text = if subdirs {
            tr(Msg::LibraryScanSubdirs)
        } else {
            ""
        };
        let subdirs_label = t.label(subdirs_text, s.font_xs().gray(INK_MID));

        let found_str = tr(Msg::LibraryScanFoundCount).replace("{count}", &found_count.to_string());
        let found_label = t.label(&found_str, s.font_md().bold());

        let current_str = tr(Msg::LibraryScanCurrentFile).replace("{file}", current_file);
        let current_label = t.label(&current_str, s.font_xs().gray(INK_MID));

        let pb = t.custom(ScanProgressBar { progress });
        let pb = t.fixed(None, Some(10), Some(pb));

        let status_label = t.label(status, s.font_xs().gray(INK_MID));
        let status_spacer = t.spacer(1);
        let pct_text = format!("{:.0}%", (progress * 100.0).clamp(0.0, 100.0));
        let pct_label = t.label(&pct_text, s.font_sm().bold());
        let status_row = t.add(
            Node::row(20).cross(Cross::Center),
            [status_label, status_spacer, pct_label],
        );

        let g1 = t.gap(1, if compact { 10 } else { 16 });
        let g2 = t.gap(1, if compact { 4 } else { 8 });
        let g3 = t.gap(1, if compact { 14 } else { 32 });
        let g4 = t.gap(1, if compact { 6 } else { 10 });
        let g5 = t.gap(1, if compact { 12 } else { 24 });
        let g6 = t.gap(1, if compact { 8 } else { 14 });

        let card_inner = t.add(
            Node::column(0),
            [
                top_row,
                g1,
                path_label,
                g2,
                subdirs_label,
                g3,
                found_label,
                g4,
                current_label,
                g5,
                pb,
                g6,
                status_row,
            ],
        );
        let card_pad = t.pad(Insets::all(if compact { 20 } else { 32 }), card_inner);
        let card = t.add(Node::column(0).bg(CARD).border(0, 0, 32), [card_pad]);

        // Found books section
        let limit = if compact { 3 } else { 5 };
        let found_books_title = t.label(tr(Msg::LibraryScanFoundBooks), s.font_sm().bold());
        let first_n = scanned_books.len().min(limit);
        let first_str = tr(Msg::LibraryScanFirstBooks).replace("{count}", &first_n.to_string());
        let first_label = t.label(&first_str, s.font_xs().gray(INK_MID));
        let header_spacer = t.spacer(1);
        let section_header = t.add(
            Node::row(20).cross(Cross::Center),
            [found_books_title, header_spacer, first_label],
        );

        let mut book_rows = Vec::new();
        for b in scanned_books.iter().take(limit) {
            let title = t.label(&b.title, s.font_sm().bold());
            let fmt = t.label(&b.format, s.font_xs().gray(INK_MID));
            let sp = t.spacer(1);
            let row = t.add(Node::row(20).cross(Cross::Center), [title, sp, fmt]);
            let row_pad = t.pad(
                if compact {
                    Insets::new(10, 20, 10, 20)
                } else {
                    Insets::new(16, 24, 16, 24)
                },
                row,
            );
            let row_card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [row_pad]);
            book_rows.push(row_card);
        }
        let books_col = t.add(Node::column(if compact { 8 } else { 12 }), book_rows);
        let list_section = t.add(
            Node::column(if compact { 10 } else { 16 }),
            [section_header, books_col],
        );

        // Cancel button at bottom
        let cancel_text = t.label(tr(Msg::LibraryScanCancel), s.font_base());
        let cancel_align = t.align(0.5, 0.5, cancel_text);
        let cancel_btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(if compact { 80 } else { 96 }),
            })
            .bg(PAPER)
            .border(2, INK, 20)
            .action(action::CANCEL_SCAN)
            .feedback(Feedback::Invert),
            [cancel_align],
        );

        let gap_card = t.gap(1, if compact { 14 } else { 28 });
        let bottom_spacer = t.spacer(1);
        let content = t.add(
            Node::column(0).flex(1),
            [card, gap_card, list_section, bottom_spacer, cancel_btn],
        );
        (content, pb)
    }

    /// Build State 3: Scan complete summary report.
    fn build_done(
        t: &mut Tree,
        s: &Styles,
        tiles: &[ScanTile; 6],
        error: Option<&ScanError>,
        added_books: &[AddedBook],
        compact: bool,
    ) -> NodeId {
        // 6 Statistic Tiles (3 columns x 2 rows). Use MinSize rather than Fixed
        // so wrapped labels in Chinese/English never crush the value line.
        let tile_min_h = if compact { 104 } else { 124 };
        let tile_pad = if compact {
            Insets::new(12, 16, 12, 16)
        } else {
            Insets::new(20, 24, 20, 24)
        };
        let mut tile_nodes = Vec::new();
        for tile in tiles {
            let label = t.label(&tile.label, s.font_xs().gray(INK_MID));
            let val = t.label(&tile.value, s.font_md().bold());
            let gap = t.gap(1, if compact { 4 } else { 8 });
            let col = t.add(Node::column(0).cross(Cross::Start), [label, gap, val]);
            let pad = t.pad(tile_pad, col);
            let tile_node = t.add(
                Node::new(Kind::MinSize(Size::new(0, tile_min_h)))
                    .bg(CARD)
                    .border(0, CARD, 24)
                    .flex(1),
                [pad],
            );
            tile_nodes.push(tile_node);
        }
        let grid_gap = if compact { 12 } else { 16 };
        let row1 = t.add(
            Node::row(grid_gap),
            vec![tile_nodes[0], tile_nodes[1], tile_nodes[2]],
        );
        let row2 = t.add(
            Node::row(grid_gap),
            vec![tile_nodes[3], tile_nodes[4], tile_nodes[5]],
        );
        let tiles_grid = t.add(Node::column(grid_gap), [row1, row2]);

        let mut parts = vec![tiles_grid];

        // Error card if any
        if let Some(err) = error {
            let mark = t.label("!", s.font_xs().bold().gray(PAPER));
            let mark_align = t.align(0.5, 0.5, mark);
            let mark_box = t.add(
                Node::new(Kind::Fixed {
                    w: Some(56),
                    h: Some(56),
                })
                .bg(INK)
                .border(0, INK, 28),
                [mark_align],
            );
            let err_title_str =
                tr(Msg::LibraryScanErrorCount).replace("{count}", &err.count.to_string());
            let err_title = t.label(&err_title_str, s.font_xs().gray(INK_MID));
            let err_msg_str = format!("{} {}", err.filename, err.reason);
            let err_msg = t.label(&err_msg_str, s.font_sm().bold());
            let err_text_col = t.add(Node::column(4), [err_title, err_msg]);
            let err_row = t.add(Node::row(20).cross(Cross::Center), [mark_box, err_text_col]);
            let err_pad = t.pad(
                if compact {
                    Insets::new(14, 22, 14, 22)
                } else {
                    Insets::new(22, 28, 22, 28)
                },
                err_row,
            );
            let err_card = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [err_pad]);
            let g = t.gap(1, if compact { 12 } else { 20 });
            parts.push(g);
            parts.push(err_card);
        }

        // Added books preview
        if !added_books.is_empty() {
            let count_str = added_books.len().to_string();
            let preview_title_str =
                tr(Msg::LibraryScanAddedPreviewTitle).replace("{count}", &count_str);
            let preview_title = t.label(&preview_title_str, s.font_sm().bold());

            let preview_limit = if compact { 2 } else { 4 };
            let (cw, ch) = if compact { (72, 96) } else { (96, 128) };
            let mut book_rows = Vec::new();
            for b in added_books.iter().take(preview_limit) {
                let cover = Cover::new(&b.title, &b.author, s.family).with_image(b.cover.clone());
                let cover_node = t.custom(cover);
                let cover_box = t.fixed(Some(cw), Some(ch), Some(cover_node));

                let title = t.label(&b.title, s.font_sm().bold());
                let meta_str = format!("{} · {}", b.author, b.format);
                let meta = t.label(&meta_str, s.font_xs().gray(INK_MID));
                let info_col = t.add(Node::column(6), [title, meta]);

                let row = t.add(Node::row(20).cross(Cross::Center), [cover_box, info_col]);
                book_rows.push(row);
            }
            let books_col = t.add(Node::column(if compact { 8 } else { 12 }), book_rows);
            let preview_section = t.add(
                Node::column(if compact { 10 } else { 16 }),
                [preview_title, books_col],
            );
            let g = t.gap(1, if compact { 12 } else { 24 });
            parts.push(g);
            parts.push(preview_section);
        }

        // Bottom two buttons: View Library (black) & Rescan (outline)
        let btn_h = if compact { 80 } else { 96 };
        let view_text = t.label(
            tr(Msg::LibraryScanViewLibrary),
            s.font_base().bold().gray(PAPER),
        );
        let view_align = t.align(0.5, 0.5, view_text);
        let view_btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(btn_h),
            })
            .bg(INK)
            .border(0, INK, 20)
            .action(action::VIEW_LIBRARY)
            .feedback(Feedback::Invert)
            .flex(1),
            [view_align],
        );

        let rescan_text = t.label(tr(Msg::LibraryScanRescan), s.font_base());
        let rescan_align = t.align(0.5, 0.5, rescan_text);
        let rescan_btn = t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(btn_h),
            })
            .bg(PAPER)
            .border(2, INK, 20)
            .action(action::RESCAN)
            .feedback(Feedback::Invert)
            .flex(1),
            [rescan_align],
        );

        let buttons_row = t.add(Node::row(20), [view_btn, rescan_btn]);
        let sp = t.spacer(1);
        parts.push(sp);
        parts.push(buttons_row);

        t.add(Node::column(0).flex(1), parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_and_review_action_ranges_are_disjoint() {
        // Enough folders and books to catch a range that grows into its
        // neighbour: the picker rows, the source checkboxes, the review
        // rows, and every fixed action.
        const FIXED: &[crate::tree::ActionId] = &[
            action::BACK,
            action::START_SCAN,
            action::WIFI_TRANSFER,
            action::CANCEL_SCAN,
            action::FINISH_SCAN,
            action::VIEW_LIBRARY,
            action::RESCAN,
            action::CLOUD_LIBRARY,
            action::WEREAD_LIBRARY,
            action::CHOOSE_SCOPE,
            action::SCAN_SCOPE,
            action::SCOPE_PARENT,
            action::SOURCE_WHOLE,
            action::SOURCE_CURRENT,
            action::SOURCE_CLEAR,
            action::DEST_CHOOSE,
            action::DEST_USE,
            action::DEST_CANCEL,
            action::REVIEW_ALL,
            action::REVIEW_CLEAR,
            action::REVIEW_CONFIRM,
            action::REVIEW_CANCEL,
            action::REVIEW_PREV,
            action::REVIEW_NEXT,
            action::PREV_PAGE,
            action::NEXT_PAGE,
        ];
        for index in 0..20_000 {
            let folder = action::SCOPE_ITEM + index;
            let source = action::SOURCE_ITEM + index;
            let review = action::REVIEW_ITEM + index;
            for id in [folder, source, review] {
                assert!(!FIXED.contains(&id), "row id {id} collides");
            }
            assert_ne!(folder, source);
            assert_ne!(folder, review);
            assert_ne!(source, review);
        }
        // The tagged bases cannot overlap, however many rows there are;
        // the loop above proves the decoded ids stay disjoint in practice.
        assert_eq!(
            (
                action::SCOPE_ITEM >> 24,
                action::SOURCE_ITEM >> 24,
                action::REVIEW_ITEM >> 24
            ),
            (1, 2, 3)
        );
    }
    use crate::text::{FamilyId, Fonts, TextEngine};

    fn test_tree(state: ScanState) -> (Tree, ScanUi) {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(FamilyId(0));
        let screen = ScanScreen {
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            state,
            local_books_count: 0,
        };
        ScanUi::build(&mut text, &s, &screen)
    }

    fn has_action(tree: &Tree, id: NodeId, action: crate::tree::ActionId) -> bool {
        if tree.node(id).action == Some(action) {
            return true;
        }
        tree.node(id)
            .children
            .iter()
            .any(|c| has_action(tree, *c, action))
    }

    /// FIX-106: a scan that only restored a book (moved away, put back) used
    /// to show zero added and no card of its own, so the report read as if
    /// nothing had happened. The restored book is counted with the added ones.
    #[test]
    fn a_restored_book_shows_in_the_report() {
        aristotle_i18n::with_language("zh", || {
            let quiet = added_tile(0, 0);
            assert_eq!(quiet.label, "新增");
            assert_eq!(quiet.value, "0 本");

            let back = added_tile(0, 1);
            assert_eq!(back.label, "新增/恢复");
            assert_eq!(back.value, "1 本");

            assert_eq!(added_tile(2, 1).value, "3 本");
        });
        aristotle_i18n::with_language("en", || {
            let back = added_tile(0, 2);
            assert_eq!(back.label, "Added/returned");
            assert_eq!(back.value, "2 books");
        });
    }

    /// FIX-106: the merged label must still fit one line of a report card
    /// (a third of the 1264 px page, minus the grid gaps and the card padding)
    /// in every language, or the fixed-height card would overflow.
    #[test]
    fn the_restored_label_fits_one_card_line() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(FamilyId(0));
        let card = (1264 - 2 * home_margin() - 2 * 16) / 3 - 2 * 24;
        for lang in ["zh", "en", "pl"] {
            aristotle_i18n::with_language(lang, || {
                let tile = added_tile(0, 1);
                let width = text.measure(&tile.label, &s.font_xs()).ceil() as i32;
                assert!(
                    width <= card,
                    "{lang}: {:?} is {width} px, the card holds {card}",
                    tile.label
                );
            });
        }
    }

    #[test]
    fn the_empty_guide_gives_way_once_a_scan_finds_books() {
        assert!(ScanState::Empty.empty_guide_is_stale(1));
        assert!(!ScanState::Empty.empty_guide_is_stale(0));
        // A scan in progress and its report have their own way forward.
        assert!(!ScanState::Scanning {
            path: "/mnt/us/documents".into(),
            subdirs: true,
            found_count: 3,
            current_file: "test.epub".into(),
            progress: 0.5,
            status: "Reading...".into(),
            scanned_books: Vec::new(),
        }
        .empty_guide_is_stale(3));
    }

    #[test]
    fn empty_state_actions() {
        let (tree, _ui) = test_tree(ScanState::Empty);
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::BACK));
        assert!(has_action(&tree, root, action::START_SCAN));
        assert!(has_action(&tree, root, action::WIFI_TRANSFER));
    }

    /// FIX-105: a reader whose library is empty must still reach the cloud
    /// shelf, or a new device with nothing local has no way to download a book.
    #[test]
    fn the_empty_guide_keeps_the_cloud_segment() {
        let (tree, _ui) = test_tree(ScanState::Empty);
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::CLOUD_LIBRARY));
    }

    #[test]
    fn scanning_state_actions() {
        let (tree, _ui) = test_tree(ScanState::Scanning {
            path: "/mnt/us/documents".into(),
            subdirs: true,
            found_count: 5,
            current_file: "test.epub".into(),
            progress: 0.58,
            status: "Reading...".into(),
            scanned_books: vec![ScannedBook {
                title: "Book 1".into(),
                format: "EPUB".into(),
            }],
        });
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::BACK));
        assert!(has_action(&tree, root, action::CANCEL_SCAN));
        assert!(!has_action(&tree, root, action::FINISH_SCAN));
        assert!(!has_action(&tree, root, action::CLOUD_LIBRARY));
    }

    #[test]
    fn done_state_actions() {
        let tiles = [
            ScanTile {
                label: "Added".into(),
                value: "12".into(),
            },
            ScanTile {
                label: "Updated".into(),
                value: "2".into(),
            },
            ScanTile {
                label: "Removed".into(),
                value: "1".into(),
            },
            ScanTile {
                label: "Ignored".into(),
                value: "5".into(),
            },
            ScanTile {
                label: "Imported".into(),
                value: "3".into(),
            },
            ScanTile {
                label: "Time".into(),
                value: "8 s".into(),
            },
        ];
        let (tree, _ui) = test_tree(ScanState::Done {
            tiles,
            error: Some(ScanError {
                count: 1,
                filename: "bad.cbz".into(),
                reason: "corrupted".into(),
            }),
            added_books: vec![AddedBook {
                title: "Book 1".into(),
                author: "Author 1".into(),
                format: "EPUB".into(),
                cover: None,
            }],
        });
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::BACK));
        assert!(has_action(&tree, root, action::VIEW_LIBRARY));
        assert!(has_action(&tree, root, action::RESCAN));
        assert!(!has_action(&tree, root, action::CLOUD_LIBRARY));
    }

    fn sources_state(selected: Vec<String>, whole: bool) -> ScanState {
        ScanState::Sources {
            mode: PickerMode::Sources,
            path: "documents".into(),
            folders: vec![
                ScopeFolder {
                    name: "Novels".into(),
                    relative: "documents/Novels".into(),
                    books: 2,
                },
                ScopeFolder {
                    name: "Poetry".into(),
                    relative: "documents/Poetry".into(),
                    books: 0,
                },
            ],
            can_go_up: true,
            selected,
            whole,
            destination: "/mnt/us/documents/墨阅书库".into(),
        }
    }

    fn review_state(selected: Vec<bool>) -> ScanState {
        ScanState::Review {
            sources: "documents/Novels".into(),
            destination: "/mnt/us/documents/墨阅书库".into(),
            books: vec![
                ReviewBook {
                    title: "Lalka".into(),
                    path: "documents/Novels/Lalka.txt".into(),
                    format: "TXT".into(),
                    id: None,
                    in_destination: false,
                },
                ReviewBook {
                    title: "Uczta".into(),
                    path: "documents/Novels/Uczta.epub".into(),
                    format: "EPUB".into(),
                    id: Some("uczta".into()),
                    in_destination: false,
                },
                ReviewBook {
                    title: "Dziady".into(),
                    path: "documents/墨阅书库/Dziady.txt".into(),
                    format: "TXT".into(),
                    id: Some("dziady".into()),
                    in_destination: true,
                },
            ],
            selected,
        }
    }

    /// The source screen cannot start a scan with nothing chosen, and the
    /// checkboxes are individual targets beside the folder-opening targets.
    #[test]
    fn the_source_screen_requires_a_source_and_keeps_row_targets() {
        let (tree, _ui) = test_tree(sources_state(Vec::new(), false));
        let root = tree.root().unwrap();
        assert!(!has_action(&tree, root, action::SCAN_SCOPE));
        assert!(has_action(&tree, root, action::SOURCE_WHOLE));
        assert!(has_action(&tree, root, action::SOURCE_CURRENT));
        assert!(has_action(&tree, root, action::SOURCE_ITEM));
        assert!(has_action(&tree, root, action::SCOPE_ITEM));
        assert!(has_action(&tree, root, action::DEST_CHOOSE));

        let (tree, _ui) = test_tree(sources_state(vec!["documents/Novels".into()], false));
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::SCAN_SCOPE));
        assert!(has_action(&tree, root, action::SOURCE_ITEM + 1));

        let (tree, _ui) = test_tree(sources_state(Vec::new(), true));
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::SCAN_SCOPE));
    }

    /// The review defaults to nothing checked, so the confirmation cannot
    /// import a file the reader did not choose. A book already inside the
    /// destination is listed but cannot be checked or imported again.
    #[test]
    fn the_review_defaults_to_unchecked_and_confirms_only_a_selection() {
        let (tree, _ui) = test_tree(review_state(vec![false, false, false]));
        let root = tree.root().unwrap();
        assert!(!has_action(&tree, root, action::REVIEW_CONFIRM));
        assert!(has_action(&tree, root, action::REVIEW_ALL));
        assert!(has_action(&tree, root, action::REVIEW_CLEAR));
        assert!(has_action(&tree, root, action::REVIEW_ITEM));
        assert!(has_action(&tree, root, action::REVIEW_ITEM + 1));
        assert!(
            !has_action(&tree, root, action::REVIEW_ITEM + 2),
            "a book already in the destination is not selectable"
        );

        let (tree, _ui) = test_tree(review_state(vec![true, false, false]));
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::REVIEW_CONFIRM));
        assert!(has_action(&tree, root, action::REVIEW_CANCEL));
    }

    /// A new file dropped straight into the library folder still needs its
    /// own confirmation to get an index entry. Only a book the index already
    /// knows and that already sits there has nothing to do.
    #[test]
    fn an_unindexed_file_inside_the_destination_stays_selectable() {
        let state = ScanState::Review {
            sources: "documents".into(),
            destination: "/mnt/us/documents/墨阅书库".into(),
            books: vec![
                ReviewBook {
                    title: "Dropped in".into(),
                    path: "documents/墨阅书库/dropped.txt".into(),
                    format: "TXT".into(),
                    id: None,
                    in_destination: true,
                },
                ReviewBook {
                    title: "Known".into(),
                    path: "documents/墨阅书库/known.txt".into(),
                    format: "TXT".into(),
                    id: Some("known".into()),
                    in_destination: true,
                },
            ],
            selected: vec![false, false],
        };
        let (tree, _ui) = test_tree(state);
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::REVIEW_ITEM));
        assert!(!has_action(&tree, root, action::REVIEW_ITEM + 1));
    }

    /// With no books found the review says so and offers no confirmation.
    #[test]
    fn an_empty_review_has_no_confirmation() {
        let (tree, _ui) = test_tree(ScanState::Review {
            sources: "documents".into(),
            destination: "/mnt/us/documents/墨阅书库".into(),
            books: Vec::new(),
            selected: Vec::new(),
        });
        let root = tree.root().unwrap();
        assert!(!has_action(&tree, root, action::REVIEW_CONFIRM));
        assert!(has_action(&tree, root, action::REVIEW_CANCEL));
    }

    /// The destination picker is a single choice: no source checkboxes, one
    /// "use this folder" and one way back.
    #[test]
    fn the_destination_picker_uses_the_current_folder_once() {
        let (tree, _ui) = test_tree(ScanState::Sources {
            mode: PickerMode::Destination,
            path: "documents".into(),
            folders: vec![ScopeFolder {
                name: "Novels".into(),
                relative: "documents/Novels".into(),
                books: 2,
            }],
            can_go_up: true,
            selected: Vec::new(),
            whole: false,
            destination: "/mnt/us/documents/墨阅书库".into(),
        });
        let root = tree.root().unwrap();
        assert!(has_action(&tree, root, action::DEST_USE));
        assert!(has_action(&tree, root, action::DEST_CANCEL));
        assert!(has_action(&tree, root, action::SCOPE_ITEM));
        assert!(!has_action(&tree, root, action::SOURCE_ITEM));
        assert!(!has_action(&tree, root, action::SCAN_SCOPE));
    }

    #[test]
    fn destination_picker_shows_the_folder_its_button_will_choose() {
        aristotle_i18n::with_language("en", || {
            let mut state = sources_state(Vec::new(), false);
            if let ScanState::Sources { mode, path, .. } = &mut state {
                *mode = PickerMode::Destination;
                *path = "documents/Poetry".into();
            }
            let (tree, _) = test_tree(state);
            let words: Vec<_> = (0..tree.len())
                .filter_map(|n| match &tree.node(NodeId(n as u32)).kind {
                    Kind::Text { para, .. } => {
                        Some(para.spans.iter().map(|span| span.text.as_str()))
                    }
                    _ => None,
                })
                .flatten()
                .collect();
            assert!(words.contains(&"Current folder"));
            assert!(words.contains(&"documents/Poetry"));
            assert!(!words.contains(&"/mnt/us/documents/墨阅书库"));
        });
    }
}
