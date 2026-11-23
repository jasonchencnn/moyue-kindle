// SPDX-License-Identifier: AGPL-3.0-or-later
//! Library home, and the folder and series shelves under it.
//!
//! One job: get back into a book, or find the next one. So the home screen
//! holds the book being read at the top, then the library in three views:
//!
//! - **Books**: every book, narrowed by a shelf chip: All, Reading (being
//!   read), New (never opened), Finished.
//! - **Folders**: the folders of the scanned storage as they are on the device
//!   (author folders such as "Mickiewicz, Adam", topics such as
//!   "Filozofia"), then the books lying loose at the top. A folder opens as
//!   its own shelf, with subfolders first.
//! - **Series**: one shelf per series, its books in series order.
//!
//! Lists turn pages, never scroll, and say where you are ("Page 1 of 3").
//! There is no store, no recommendations, no reading lists and no "more
//! for you": nothing on the screen exists to sell something.
//!
//! Covers are optional ([`CoverMode`]): none by default, a clean list of
//! titles; typographic cards (`widgets::Cover`); or the books' own covers.
//!
//! Labels come from `aristotle-i18n`, in the UI language.

use super::icon_button;
use crate::geom::{mm, Insets, Rect, Size, MIN_TARGET};
use crate::icons::Icon;
use crate::input::Direction;
use crate::status::{self, LoadingNodes, StatusBar};
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{
    home_margin, margin, Space, Styles, CARD, INK, INK_2, INK_3, INK_MID, PAPER, RULE,
};
use crate::tree::{
    pages, ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Refresh, Tree,
    Widget,
};
use crate::widgets::{series_number, Cover, CoverMark, GrayImage};
use aristotle_i18n::{tf, tr, trn, Msg, Plural};

/// Reading state of a book, as the library index keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Unread,
    Reading,
    Finished,
}

/// Origin channel of a book in the library.
///
/// Extensible for future cloud channels (e.g. WebDAV / Jianguoyun). Used both
/// for the cover badge and for channel-isolated cloud sync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BookSource {
    /// Side-loaded over USB or Wi-Fi transfer.
    #[default]
    Local,
    /// Downloaded from Readest cloud.
    Readest,
    /// Downloaded from WeRead.
    WeRead,
    /// Downloaded from Nutstore (Jianguoyun) over WebDAV.
    Jianguoyun,
}

impl BookSource {
    /// Short localized badge shown on the book cover and list metadata.
    pub fn label(self) -> &'static str {
        tr(match self {
            BookSource::Local => Msg::LibrarySourceLocal,
            BookSource::Readest => Msg::LibrarySourceReadest,
            BookSource::WeRead => Msg::LibrarySourceWeread,
            BookSource::Jianguoyun => Msg::LibrarySourceJianguoyun,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Book {
    pub title: String,
    pub author: String,
    pub format: String,
    /// 0.0 is the start, 1.0 the end.
    pub progress: f32,
    pub status: Status,
    /// Where the reader is, in words: a chapter title or a printed page
    /// ("Page 4 of 180"). Shown on the "continue reading" card; may be empty.
    pub chapter: String,
    /// Reading time left, when known.
    pub minutes_left: Option<u32>,
    /// The cover thumbnail, if the book has one and photo covers are on.
    pub cover: Option<GrayImage>,
    /// Series and shelf, for the typographic card and the list.
    pub series: Option<(String, f32)>,
    /// The folder the book lies in (relative to `documents/`), if any.
    pub folder: Option<String>,
    /// Origin channel of this book (Local / Readest / WeRead).
    pub source: BookSource,
}

impl Book {
    pub fn new(title: &str, author: &str, progress: f32) -> Self {
        let status = if progress >= 1.0 {
            Status::Finished
        } else if progress > 0.0 {
            Status::Reading
        } else {
            Status::Unread
        };
        Self {
            title: title.into(),
            author: author.into(),
            format: "EPUB".into(),
            progress,
            status,
            chapter: String::new(),
            minutes_left: None,
            cover: None,
            series: None,
            folder: None,
            source: BookSource::Local,
        }
    }

    pub fn with_source(mut self, source: BookSource) -> Self {
        self.source = source;
        self
    }

    pub fn with_format(mut self, format: &str) -> Self {
        self.format = format.into();
        self
    }

    pub fn in_folder(mut self, folder: &str) -> Self {
        self.folder = Some(folder.into());
        self
    }

    pub fn in_series(mut self, name: &str, index: f32) -> Self {
        self.series = Some((name.into(), index));
        self
    }

    /// What the typographic card shows: the series, else the folder,
    /// decides the binding.
    pub fn mark(&self) -> CoverMark {
        CoverMark {
            group: self
                .series
                .as_ref()
                .map(|(s, _)| s.clone())
                .or_else(|| self.folder.clone()),
            series_index: self.series.as_ref().map(|(_, i)| *i),
        }
    }
}

/// A plausible library of public-domain books, most recently opened first:
/// author folders and topic folders, one series.
pub fn sample_books() -> Vec<Book> {
    let mut lalka = Book::new("Lalka", "Bolesław Prus", 0.43).in_folder("Prus, Bolesław");
    lalka.chapter = "Rozdział VII · Idylla".into();
    lalka.minutes_left = Some(742);
    vec![
        lalka,
        Book::new("紅樓夢", "曹雪芹", 0.42).in_folder("中國小說"),
        Book::new("Pan Tadeusz", "Adam Mickiewicz", 0.02).in_folder("Mickiewicz, Adam"),
        Book::new("Duma i uprzedzenie", "Jane Austen", 0.01).in_folder("Powieści"),
        Book::new("Państwo", "Platon", 0.61).in_folder("Platon"),
        Book::new("Dziady", "Adam Mickiewicz", 0.0).in_folder("Mickiewicz, Adam"),
        Book::new("Obrona Sokratesa", "Platon", 1.0).in_folder("Platon"),
        Book::new("Ogniem i mieczem", "Henryk Sienkiewicz", 0.88)
            .in_series("Trylogia", 1.0)
            .in_folder("Sienkiewicz, Henryk"),
        Book::new("Potop", "Henryk Sienkiewicz", 0.0)
            .in_series("Trylogia", 2.0)
            .in_folder("Sienkiewicz, Henryk"),
        Book::new("Rozmyślania", "Marek Aureliusz", 0.0).in_folder("Filozofia"),
        Book::new("Fedon", "Platon", 0.18).in_folder("Platon"),
        Book::new("Moby Dick", "Herman Melville", 0.0),
    ]
}

/// The home screen's views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Books,
    Folders,
    Series,
}

impl View {
    pub const ALL: [View; 3] = [View::Books, View::Folders, View::Series];

    pub fn label(self) -> &'static str {
        tr(match self {
            View::Books => Msg::LibraryViewBooks,
            View::Folders => Msg::LibraryViewFolders,
            View::Series => Msg::LibraryViewSeries,
        })
    }
}

/// The shelves of the books view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Reading,
    New,
    Finished,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Reading, Filter::New, Filter::Finished];

    pub fn label(self) -> &'static str {
        tr(match self {
            Filter::All => Msg::LibraryFilterAll,
            Filter::Reading => Msg::LibraryFilterReading,
            Filter::New => Msg::LibraryFilterNew,
            Filter::Finished => Msg::LibraryFilterFinished,
        })
    }

    pub fn accepts(self, b: &Book) -> bool {
        match self {
            Filter::All => true,
            Filter::Reading => b.status == Status::Reading,
            Filter::New => b.status == Status::Unread,
            Filter::Finished => b.status == Status::Finished,
        }
    }
}

/// How books are pictured in lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoverMode {
    /// Text only: the calmest list, and the default.
    #[default]
    None,
    /// Typographic cards.
    Cards,
    /// The books' own covers where they have one, cards otherwise.
    Photos,
}

impl CoverMode {
    pub const ALL: [CoverMode; 3] = [CoverMode::None, CoverMode::Cards, CoverMode::Photos];

    pub fn label(self) -> &'static str {
        tr(match self {
            CoverMode::None => Msg::CoversNone,
            CoverMode::Cards => Msg::CoversCards,
            CoverMode::Photos => Msg::CoversPhotos,
        })
    }
}

/// The display layout of the library screen: list or 3x2 grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryLayout {
    /// Vertical list with one row per book/shelf.
    #[default]
    List,
    /// 3 columns x 2 rows cover grid.
    Grid,
}

impl LibraryLayout {
    pub fn toggle(self) -> Self {
        match self {
            LibraryLayout::List => LibraryLayout::Grid,
            LibraryLayout::Grid => LibraryLayout::List,
        }
    }
}

/// One line of a list.
#[derive(Clone, Debug)]
pub enum Item {
    Book(Book),
    Folder {
        name: String,
        books: usize,
        reading: usize,
    },
    Series {
        name: String,
        books: usize,
        finished: usize,
    },
}

/// Where the list is: the home screen with its views, or one shelf.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place<'a> {
    Home { view: View, filter: Filter },
    Folder { name: &'a str },
    Series { name: &'a str },
}

pub mod action {
    use crate::tree::ActionId;
    pub const RESUME: ActionId = 1;
    pub const SEARCH: ActionId = 3;
    pub const SORT: ActionId = 4;
    pub const PREV_PAGE: ActionId = 5;
    pub const NEXT_PAGE: ActionId = 6;
    /// Open the jump-to-page dialog.
    pub const JUMP: ActionId = 7;
    /// Up from a folder or series shelf.
    pub const BACK: ActionId = 8;
    /// Switch between list and grid view.
    pub const TOGGLE_LAYOUT: ActionId = 9;
    /// `FILTER + i` selects `Filter::ALL[i]`.
    pub const FILTER: ActionId = 10;
    /// `VIEW + i` selects `View::ALL[i]`.
    pub const VIEW: ActionId = 20;

    /// Jump dialog: jump to first page.
    pub const JUMP_FIRST: ActionId = 30;
    /// Jump dialog: step back 5 pages.
    pub const JUMP_MINUS_5: ActionId = 31;
    /// Jump dialog: step back 1 page.
    pub const JUMP_MINUS_1: ActionId = 32;
    /// Jump dialog: step forward 1 page.
    pub const JUMP_PLUS_1: ActionId = 33;
    /// Jump dialog: step forward 5 pages.
    pub const JUMP_PLUS_5: ActionId = 34;
    /// Jump dialog: jump to last page.
    pub const JUMP_LAST: ActionId = 35;
    /// Jump dialog: confirm jump.
    pub const JUMP_GO: ActionId = 36;
    /// Jump dialog: cancel and close.
    pub const JUMP_CANCEL: ActionId = 37;

    /// Wi-Fi transfer button
    pub const WIFI_TRANSFER: ActionId = 40;
    /// Cloud library segment
    pub const CLOUD_LIBRARY: ActionId = 41;
    /// Scan button
    pub const SCAN: ActionId = 42;
    /// WeRead library segment
    pub const WEREAD_LIBRARY: ActionId = 43;
    /// Nutstore (Jianguoyun) library segment
    pub const JIANGUOYUN_LIBRARY: ActionId = 44;

    /// `ITEM + i` opens `Shelf::items[i]`.
    pub const ITEM: ActionId = 100;
    /// `ITEM_MORE + i` opens actions for `Shelf::items[i]`.
    pub const ITEM_MORE: ActionId = 50_000;
}

/// Which cell of the "Local / Cloud" segmented control is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    Local,
    Cloud,
    Weread,
    Jianguoyun,
}

/// Which cloud channels the library's segmented control shows.
///
/// The control always has the local cell; the three cloud cells (Readest,
/// WeRead, Nutstore) appear only when their channel is turned on in Settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelVisibility {
    /// The Readest cloud library.
    pub readest: bool,
    /// The WeRead library.
    pub weread: bool,
    /// The Nutstore (Jianguoyun) library.
    pub jianguoyun: bool,
}

impl ChannelVisibility {
    /// How many cloud channels are on.
    pub fn count(self) -> usize {
        usize::from(self.readest) + usize::from(self.weread) + usize::from(self.jianguoyun)
    }

    /// The visibility of the two channels the older call sites know about.
    pub fn readest_and_weread(readest: bool, weread: bool) -> Self {
        Self {
            readest,
            weread,
            jianguoyun: false,
        }
    }
}

/// The side margins of a list shelf, and the gap above its first page: the
/// local list and the cloud shelf share them, so their cards line up with the
/// page header and leave the same gap below the filter chips.
pub fn list_insets() -> Insets {
    Insets::new(28, home_margin(), 0, home_margin())
}

/// One cell of the "Local / Cloud" segmented control: the chosen cell is
/// black with white bold text and takes no action.
///
/// `compact` is the four-cell row: a smaller face and a single line, so that
/// four labels fit a six-inch panel without wrapping or truncation.
fn segment_cell(
    t: &mut Tree,
    s: &Styles,
    label: &str,
    chosen: bool,
    id: Option<ActionId>,
    compact: bool,
) -> NodeId {
    let style = if compact {
        if chosen {
            s.font_xs().bold().gray(PAPER)
        } else {
            s.font_xs().gray(INK)
        }
    } else if chosen {
        s.font_sm().bold().gray(PAPER)
    } else {
        s.font_sm().gray(INK)
    };
    let mut para = Paragraph::plain(label, style, Align::Left);
    para.style.max_lines = Some(1);
    let label = t.text(para);
    let aligned = t.align(0.5, 0.5, label);
    let mut cell = Node::column(0).border(0, 0, 18).flex(1);
    if chosen {
        cell = cell.bg(INK);
    }
    if let Some(id) = id {
        cell = cell.action(id).feedback(Feedback::Invert);
    }
    t.add(cell, [aligned])
}

/// The local cell's label. The four-cell row has no room for the word
/// "books", so it drops it and keeps the count.
pub fn segment_local_label(local_books_count: usize, compact: bool) -> String {
    if compact {
        tf!(Msg::LibrarySegmentLocalCompact, count = local_books_count)
    } else {
        tf!(Msg::LibrarySegmentLocal, count = local_books_count)
    }
}

/// The "Local / Cloud" segmented control: the same cells, of the same size,
/// in the same place on the local shelf, the cloud shelf and the empty-library
/// guide. `local` and `cloud` are the actions their cells carry; the chosen
/// cell carries none.
pub fn segment_control(
    t: &mut Tree,
    s: &Styles,
    local_books_count: usize,
    selected: Segment,
    local: Option<ActionId>,
    cloud: Option<ActionId>,
) -> NodeId {
    let local_str = tf!(Msg::LibrarySegmentLocal, count = local_books_count);
    let cloud_str = tr(Msg::LibrarySegmentCloud);
    let local = segment_cell(t, s, &local_str, selected == Segment::Local, local, false);
    let cloud = segment_cell(t, s, cloud_str, selected == Segment::Cloud, cloud, false);
    let row = t.add(Node::row(6).cross(Cross::Stretch), [local, cloud]);
    let inner = t.pad(Insets::all(6), row);
    t.add(Node::column(0).bg(CARD).border(0, 0, 24), [inner])
}

/// The three cells of the library's segmented control: Local, Cloud and
/// WeRead. `selected` takes no action; the others get the action their cell
/// carries.
pub fn segment_control_three(
    t: &mut Tree,
    s: &Styles,
    local_books_count: usize,
    selected: Segment,
    local: Option<ActionId>,
    cloud: Option<ActionId>,
    weread: Option<ActionId>,
) -> NodeId {
    segment_control_for(
        t,
        s,
        local_books_count,
        selected,
        local,
        cloud,
        weread,
        true,
        true,
    )
}

/// Dynamic segmented control showing Local plus whichever cloud libraries are
/// enabled in Settings (`show_cloud`, `show_weread`).
#[allow(clippy::too_many_arguments)]
pub fn segment_control_for(
    t: &mut Tree,
    s: &Styles,
    local_books_count: usize,
    selected: Segment,
    local: Option<ActionId>,
    cloud: Option<ActionId>,
    weread: Option<ActionId>,
    show_cloud: bool,
    show_weread: bool,
) -> NodeId {
    segment_control_channels(
        t,
        s,
        local_books_count,
        selected,
        local,
        cloud,
        weread,
        None,
        ChannelVisibility::readest_and_weread(show_cloud, show_weread),
    )
}

/// The library's segmented control with every channel Settings turns on:
/// Local, plus Readest, WeRead and Nutstore.
///
/// Four cells share the row of a six-inch panel, so the row is compact: a
/// smaller face, a shorter local label and one line per cell. The labels
/// never wrap, so the row keeps its height on every panel.
#[allow(clippy::too_many_arguments)]
pub fn segment_control_channels(
    t: &mut Tree,
    s: &Styles,
    local_books_count: usize,
    selected: Segment,
    local: Option<ActionId>,
    readest: Option<ActionId>,
    weread: Option<ActionId>,
    jianguoyun: Option<ActionId>,
    channels: ChannelVisibility,
) -> NodeId {
    let mut cells: Vec<(Segment, String, Option<ActionId>)> = Vec::new();
    cells.push((Segment::Local, String::new(), local));
    if channels.readest {
        cells.push((
            Segment::Cloud,
            tr(Msg::LibrarySegmentCloud).to_string(),
            readest,
        ));
    }
    if channels.weread {
        cells.push((
            Segment::Weread,
            tr(Msg::LibrarySegmentWeread).to_string(),
            weread,
        ));
    }
    if channels.jianguoyun {
        cells.push((
            Segment::Jianguoyun,
            tr(Msg::LibrarySegmentJianguoyun).to_string(),
            jianguoyun,
        ));
    }
    let compact = cells.len() > 3;
    let local_label = segment_local_label(local_books_count, compact);
    let nodes: Vec<NodeId> = cells
        .into_iter()
        .enumerate()
        .map(|(i, (segment, label, id))| {
            let label = if i == 0 {
                local_label.clone()
            } else if compact && segment == Segment::Cloud {
                BookSource::Readest.label().to_string()
            } else {
                label
            };
            segment_cell(t, s, &label, segment == selected, id, compact)
        })
        .collect();
    let gap = if compact { 2 } else { 6 };
    let row = t.add(Node::row(gap).cross(Cross::Stretch), nodes);
    let inner = t.pad(Insets::all(6), row);
    t.add(Node::column(0).bg(CARD).border(0, 0, 24), [inner])
}

/// The status line: the clock and the battery.
pub use crate::status::{StatusLine, STATUS_H};

/// The status line node, and the loading area of its middle (hidden; write
/// what loads into it with `status::set_loading`, see
/// `docs/ui-framework.md`, "Transitions").
pub fn status_line(t: &mut Tree, s: &Styles, line: &StatusLine) -> StatusBar {
    status::status_bar(t, &s.font_xs(), line, status::Left::TimeAndSlogan)
}

/// A horizontal rounded progress bar with a background track and solid black fill.
#[derive(Clone, Copy, Debug)]
pub struct RoundedBar {
    pub value: f32,
    pub height: i32,
    pub track_gray: u8,
}

impl Widget for RoundedBar {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(c.max.w, self.height))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let radius = (r.h as f32) / 2.0;
        cx.painter.fill_round_rect(r, radius, self.track_gray);
        let fill_w = ((r.w as f32) * self.value.clamp(0.0, 1.0)).round() as i32;
        if fill_w > 0 {
            let fill_radius = radius.min(fill_w as f32 / 2.0);
            let fill_r = Rect::new(r.x, r.y, fill_w, r.h);
            cx.painter.fill_round_rect(fill_r, fill_radius, INK);
        }
    }
}

/// Everything a library screen shows.
pub struct Shelf<'a> {
    pub place: Place<'a>,
    /// The book to continue, shown large on the home screen.
    pub hero: Option<&'a Book>,
    /// The list; `action::ITEM + i` opens `items[i]`. In the books view
    /// the screen applies the filter itself.
    pub items: &'a [Item],
    /// Name of the current sort order, shown on the sort button; empty
    /// hides the button (a series keeps its own order).
    pub sort: &'a str,
    pub status: StatusLine<'a>,
    pub covers: CoverMode,
    pub layout: LibraryLayout,
    /// Total count of local books (for the segmented control "本地 · 128 本")
    pub local_books_count: usize,
}

/// Handles to the nodes the library updates in place.
pub struct Library {
    pub list: NodeId,
    pub pager_label: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    /// The screen title. Holding it quits the app on the device.
    pub title: NodeId,
    /// The loading area in the middle of the status line, hidden.
    pub loading: LoadingNodes,
    /// Node of each list row, for tests and the preview.
    pub rows: Vec<NodeId>,
    /// Item indices and cover widgets, for loading only the visible page.
    pub cover_nodes: Vec<(usize, NodeId)>,
    /// Jump dialog overlay root.
    pub jump_overlay: NodeId,
    /// The page label inside the jump dialog.
    pub jump_label: NodeId,
    /// Currently selected target page in the jump dialog (1-indexed).
    pub jump_target: usize,
    /// Whether the jump dialog is open.
    pub jump_open: bool,
}

/// How long a book row must be held to open the book's actions: the same
/// hold as the reader's long press.
pub const LONG_PRESS_MS: u32 = 500;

/// Touch-down feedback for a book row or the book to continue. Inverting
/// is the clearest signal and suits text and typographic cards (a card
/// simply shows its inverse binding), but it would turn a cover photo into
/// a negative; with photos the row gets a heavy frame instead.
fn press_feedback(covers: CoverMode) -> Feedback {
    if covers == CoverMode::Photos {
        Feedback::Outline
    } else {
        Feedback::Invert
    }
}

/// "Page 2 of 5": the pager and reader position label.
pub fn page_of(page: usize, count: usize) -> String {
    tf!(Msg::LibraryPagerPageOf, page = page, count = count)
}

/// "3 books".
pub fn books_label(n: usize) -> String {
    trn(Plural::LibraryBooks, n)
}

impl Library {
    /// The prototype entry point: all books in the books view, with a sample clock and battery.
    pub fn build(
        text: &mut TextEngine,
        s: &Styles,
        books: &[Book],
        filter: Filter,
    ) -> (Tree, Self) {
        let items: Vec<Item> = books.iter().cloned().map(Item::Book).collect();
        let shelf = Shelf {
            place: Place::Home {
                view: View::Books,
                filter,
            },
            hero: None,
            items: &items,
            sort: tr(Msg::LibrarySortRecent),
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            covers: CoverMode::Cards,
            layout: LibraryLayout::List,
            local_books_count: books.len(),
        };
        Self::build_shelf(text, s, &shelf)
    }

    /// Builds the library tree for `shelf`.
    pub fn build_shelf(text: &mut TextEngine, s: &Styles, shelf: &Shelf) -> (Tree, Self) {
        Self::build_shelf_for_width(text, s, shelf, s.density.px(crate::geom::SCREEN_WIDTH))
    }

    /// Wraps shelf filters to keep their labels visible at the supplied width.
    pub fn build_shelf_for_width(
        text: &mut TextEngine,
        s: &Styles,
        shelf: &Shelf,
        width: i32,
    ) -> (Tree, Self) {
        Self::build_shelf_for_size(
            text,
            s,
            shelf,
            Size::new(width, s.density.px(crate::geom::SCREEN_HEIGHT)),
        )
    }

    /// Builds a shelf using the panel's native dimensions and density.
    pub fn build_shelf_for_size(
        text: &mut TextEngine,
        s: &Styles,
        shelf: &Shelf,
        size: Size,
    ) -> (Tree, Self) {
        Self::build_shelf_with_sources(text, s, shelf, size, true, true)
    }

    /// Builds a shelf using the panel's native dimensions, showing only the
    /// enabled cloud channels (`show_cloud` for Readest, `show_weread` for WeRead).
    pub fn build_shelf_with_sources(
        text: &mut TextEngine,
        s: &Styles,
        shelf: &Shelf,
        size: Size,
        show_cloud: bool,
        show_weread: bool,
    ) -> (Tree, Self) {
        Self::build_shelf_with_channels(
            text,
            s,
            shelf,
            size,
            ChannelVisibility::readest_and_weread(show_cloud, show_weread),
        )
    }

    /// Builds a shelf using the panel's native dimensions, showing the cloud
    /// channels Settings turns on: Readest, WeRead and Nutstore.
    pub fn build_shelf_with_channels(
        text: &mut TextEngine,
        s: &Styles,
        shelf: &Shelf,
        size: Size,
        channels: ChannelVisibility,
    ) -> (Tree, Self) {
        let width = (size.w as f32 * 300.0 / s.density.dpi() as f32).round() as i32;
        let panel = crate::theme::PanelLayout::for_size(size, s.density);
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &shelf.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let mut column = vec![status];

        // The sort order: next to the views at home, in the header of a shelf.
        let sort_label = t.label(shelf.sort, s.font_link());
        let chevron = t.icon(Icon::ChevronDown, 28, INK);
        let g = t.gap(8, 1);
        let sort_row = t.add(Node::row(0).cross(Cross::Center), [sort_label, g, chevron]);
        let sort = t.add(
            Node::row(0)
                .cross(Cross::Center)
                .action(action::SORT)
                .feedback(Feedback::Invert),
            [sort_row],
        );
        let sort = t.fixed(None, Some(64), Some(sort));

        // Header: the title with actions, or back and the shelf's name.
        let title;
        match shelf.place {
            Place::Home { .. } => {
                let title_text = tr(Msg::LibraryTitle);
                let title_style = s.font_lg().bold().line_height(1.2).tracking(4.0);
                title = t.label(title_text, title_style);
                let toggle_icon = match shelf.layout {
                    LibraryLayout::List => Icon::Grid,
                    LibraryLayout::Grid => Icon::List,
                };
                let search_btn = icon_button(&mut t, Icon::Search, 44, action::SEARCH);
                let wifi_btn = icon_button(&mut t, Icon::Upload, 44, action::WIFI_TRANSFER);
                let toggle = icon_button(&mut t, toggle_icon, 44, action::TOGGLE_LAYOUT);
                let buttons = t.add(
                    Node::row(8).cross(Cross::Center),
                    [search_btn, wifi_btn, toggle],
                );
                let fill = t.spacer(1);
                let header = t.add(Node::row(0).cross(Cross::Center), [title, fill, buttons]);
                let header_h = if shelf.layout == LibraryLayout::Grid {
                    104
                } else {
                    112
                };
                let header = t.fixed(None, Some(header_h), Some(header));
                let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
                column.push(header);

                // Segmented control: Local + enabled cloud sources (omitted when
                // every cloud channel is turned off in Settings).
                if channels.count() > 0 {
                    let seg_box = segment_control_channels(
                        &mut t,
                        s,
                        shelf.local_books_count,
                        Segment::Local,
                        None,
                        channels.readest.then_some(action::CLOUD_LIBRARY),
                        channels.weread.then_some(action::WEREAD_LIBRARY),
                        channels.jianguoyun.then_some(action::JIANGUOYUN_LIBRARY),
                        channels,
                    );
                    let seg_box = t.fixed(None, Some(76), Some(seg_box));
                    let bottom_margin = if shelf.layout == LibraryLayout::Grid {
                        24
                    } else {
                        28
                    };
                    let seg_control = t.pad(
                        Insets::new(0, home_margin(), bottom_margin, home_margin()),
                        seg_box,
                    );
                    column.push(seg_control);
                }
            }
            Place::Folder { name } | Place::Series { name } => {
                let back = icon_button(&mut t, Icon::ArrowLeft, 44, action::BACK);
                let mut p = Paragraph::plain(name, s.headline(), Align::Left);
                p.style.max_lines = Some(2);
                title = t.text(p);
                let kind = tr(if matches!(shelf.place, Place::Folder { .. }) {
                    Msg::LibraryFolder
                } else {
                    Msg::LibrarySeries
                });
                let count = shelf
                    .items
                    .iter()
                    .filter(|i| matches!(i, Item::Book(_)))
                    .count();
                let sub = t.label(&format!("{kind} · {}", books_label(count)), s.small());
                let words = t.add(Node::column(2).flex(1), [title, sub]);
                let g = t.gap(Space::XS, 1);
                let mut parts = vec![back, g, words];
                if !shelf.sort.is_empty() {
                    parts.push(sort);
                }
                let toggle_icon = match shelf.layout {
                    LibraryLayout::List => Icon::Grid,
                    LibraryLayout::Grid => Icon::List,
                };
                let toggle = icon_button(&mut t, toggle_icon, 44, action::TOGGLE_LAYOUT);
                let g_toggle = t.gap(Space::XS, 1);
                parts.extend([g_toggle, toggle]);
                let header = t.add(Node::row(0).cross(Cross::Center), parts);
                let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
                let g = t.gap(1, Space::S);
                let rule = t.rule(2, RULE);
                column.extend([header, g, rule]);
            }
        }

        // Tabs row: Books / Folders / Series + Scan + Sort
        if let Place::Home { view, .. } = shelf.place {
            let mut tab_nodes = Vec::new();
            for (i, v) in View::ALL.iter().enumerate() {
                let is_selected = *v == view;
                let action_id = action::VIEW + i as ActionId;
                if is_selected {
                    let label = t.label(v.label(), s.font_base().bold().line_height(1.2));
                    let line = t.add(Node::column(0).bg(INK).border(0, 0, 3), []);
                    let line = t.fixed(Some(56), Some(6), Some(line));
                    let col = t.add(Node::column(12).cross(Cross::Center), [label, line]);
                    let tab_btn = t.button(Node::plain(), col, action_id);
                    tab_nodes.push(tab_btn);
                } else {
                    let label = t.label(v.label(), s.font_base().gray(INK_MID).line_height(1.2));
                    let padded = t.pad(Insets::new(0, 0, 18, 0), label);
                    let tab_btn = t.button(Node::plain(), padded, action_id);
                    tab_nodes.push(tab_btn);
                }
            }
            let tabs_left = t.add(Node::row(44).cross(Cross::End), tab_nodes);

            // Scan button
            let scan_icon = t.icon(Icon::Refresh, 28, INK);
            let scan_label = t.label(tr(Msg::LibraryScan), s.font_xs());
            let scan_inner = t.add(Node::row(8).cross(Cross::Center), [scan_icon, scan_label]);
            let scan_inner = t.pad(Insets::vh(0, 20), scan_inner);
            let scan_btn = t.add(
                Node::row(0)
                    .bg(PAPER)
                    .border(2, INK, 20)
                    .cross(Cross::Center)
                    .action(action::SCAN)
                    .feedback(Feedback::Invert),
                [scan_inner],
            );
            let scan_btn = t.fixed(None, Some(64), Some(scan_btn));

            let tools_right = t.add(Node::row(16).cross(Cross::Center), [scan_btn, sort]);
            let fill = t.spacer(1);
            let tabs_h = if shelf.layout == LibraryLayout::Grid {
                64
            } else {
                68
            };
            let tabs_row = t.add(
                Node::row(0).cross(Cross::End),
                [tabs_left, fill, tools_right],
            );
            let tabs_row = t.fixed(None, Some(tabs_h), Some(tabs_row));
            let tabs_row = t.pad(Insets::new(0, home_margin(), 0, home_margin()), tabs_row);
            column.push(tabs_row);
        }

        // Shelf filter chips in the books view.
        let filter = match shelf.place {
            Place::Home {
                view: View::Books,
                filter,
            } => {
                let available = (width - 2 * home_margin()).max(1);
                let mut chip_rows = Vec::new();
                let mut current_chips = Vec::new();
                let mut used = 0;
                let chip_gap = 16;
                for (i, f) in Filter::ALL.iter().enumerate() {
                    let on = *f == filter;
                    let action_id = action::FILTER + i as ActionId;
                    let style = if on { s.ui_bold() } else { s.ui() };
                    let chip_w = (text.measure(f.label(), &style) * 300.0 / s.density.dpi() as f32)
                        .ceil() as i32
                        + 2 * crate::components::CHIP_PADDING_X;
                    if !current_chips.is_empty() && used + chip_gap + chip_w > available {
                        chip_rows.push(t.add(
                            Node::row(chip_gap).cross(Cross::Center),
                            std::mem::take(&mut current_chips),
                        ));
                        used = 0;
                    }
                    if !current_chips.is_empty() {
                        used += chip_gap;
                    }
                    used += chip_w;
                    current_chips.push(crate::components::chip(
                        &mut t,
                        s,
                        f.label(),
                        on,
                        action_id,
                    ));
                }
                if !current_chips.is_empty() {
                    chip_rows.push(t.add(Node::row(chip_gap).cross(Cross::Center), current_chips));
                }
                let chips_col = t.add(Node::column(12), chip_rows);
                let chips_pad = t.pad(Insets::new(28, home_margin(), 0, home_margin()), chips_col);
                let is_grid = shelf.layout == LibraryLayout::Grid;
                let chips_row = t.add(Node::column(0).hidden(is_grid), [chips_pad]);
                column.push(chips_row);
                Some(filter)
            }
            _ => None,
        };

        // The list or grid, one page at a time.
        let mut rows = Vec::new();
        let mut cover_nodes = Vec::new();
        let (list, pages_node) = match shelf.layout {
            LibraryLayout::List => {
                for (i, item) in shelf.items.iter().enumerate() {
                    let id = action::ITEM + i as ActionId;
                    let row = match item {
                        Item::Book(b) => {
                            if filter.is_some_and(|f| !f.accepts(b)) {
                                continue;
                            }
                            let in_series = matches!(shelf.place, Place::Series { .. });
                            let (row, cover) =
                                Self::book_row(&mut t, s, b, shelf.covers, in_series, i, id);
                            if shelf.covers == CoverMode::Photos {
                                if let Some(cover) = cover {
                                    cover_nodes.push((i, cover));
                                }
                            }
                            row
                        }
                        Item::Folder {
                            name,
                            books,
                            reading,
                        } => {
                            let mut sub = books_label(*books);
                            if *reading > 0 {
                                sub.push_str(" · ");
                                sub.push_str(&tf!(Msg::LibraryFolderReading, n = reading));
                            }
                            Self::shelf_row(&mut t, s, Icon::Folder, name, &sub, id)
                        }
                        Item::Series {
                            name,
                            books,
                            finished,
                        } => {
                            let mut sub = trn(Plural::LibraryVolumes, *books);
                            if *finished > 0 {
                                sub.push_str(" · ");
                                sub.push_str(&tf!(Msg::LibrarySeriesFinished, n = finished));
                            }
                            Self::shelf_row(&mut t, s, Icon::Series, name, &sub, id)
                        }
                    };
                    rows.push(row);
                }
                let mut children = Vec::new();
                if rows.is_empty() {
                    let msg = tr(match shelf.place {
                        Place::Home {
                            view: View::Folders,
                            ..
                        } => Msg::LibraryNoFolders,
                        Place::Home {
                            view: View::Series, ..
                        } => Msg::LibraryNoSeries,
                        _ if shelf.items.is_empty() && shelf.hero.is_none() => Msg::LibraryNoBooks,
                        _ => Msg::LibraryEmptyShelf,
                    });
                    let l = t.text(Paragraph::plain(msg, s.ui().gray(INK_2), Align::Center));
                    children.push(t.pad(Insets::vh(mm(10.0), Space::L), l));
                } else {
                    children.extend(rows.iter().copied());
                }
                let pages_node = t.add(pages(24).flex(1), children);
                let list = t.add(
                    Node::new(Kind::Padding(list_insets())).flex(1),
                    [pages_node],
                );
                (list, pages_node)
            }
            LibraryLayout::Grid => {
                let available = (width - 2 * home_margin()).max(1);
                // Compact covers leave room for two complete rows and the pager.
                let (columns, card_w) = match panel {
                    crate::theme::PanelLayout::Compact => (3, 188),
                    crate::theme::PanelLayout::Standard => (3, 272),
                    crate::theme::PanelLayout::Large => (4, 320),
                };
                let card_w =
                    card_w.min(((available - (columns as i32 - 1) * 24) / columns as i32).max(1));
                let cover_h = (card_w as f32 * 363.0 / 272.0).round() as i32;
                let card = (card_w, cover_h);
                let col_gap = ((available - columns as i32 * card_w) / (columns as i32 - 1)).max(1);

                for (i, item) in shelf.items.iter().enumerate() {
                    let id = action::ITEM + i as ActionId;
                    let cell = match item {
                        Item::Book(b) => {
                            if filter.is_some_and(|f| !f.accepts(b)) {
                                continue;
                            }
                            let (cell, cover) =
                                Self::book_grid_cell(&mut t, s, b, card, shelf.covers, id);
                            if shelf.covers == CoverMode::Photos {
                                cover_nodes.push((i, cover));
                            }
                            cell
                        }
                        Item::Folder {
                            name,
                            books,
                            reading,
                        } => Self::folder_grid_cell(&mut t, s, name, *books, *reading, card, id),
                        Item::Series {
                            name,
                            books,
                            finished,
                        } => Self::series_grid_cell(&mut t, s, name, *books, *finished, card, id),
                    };
                    rows.push(cell);
                }
                let mut children = Vec::new();
                if rows.is_empty() {
                    let msg = tr(match shelf.place {
                        Place::Home {
                            view: View::Folders,
                            ..
                        } => Msg::LibraryNoFolders,
                        Place::Home {
                            view: View::Series, ..
                        } => Msg::LibraryNoSeries,
                        _ if shelf.items.is_empty() && shelf.hero.is_none() => Msg::LibraryNoBooks,
                        _ => Msg::LibraryEmptyShelf,
                    });
                    let l = t.text(Paragraph::plain(msg, s.ui().gray(INK_2), Align::Center));
                    children.push(t.pad(Insets::vh(mm(10.0), Space::L), l));
                } else {
                    for row_chunk in rows.chunks(columns) {
                        let row = t.add(
                            Node::row(col_gap).cross(Cross::Start),
                            row_chunk.iter().copied(),
                        );
                        children.push(row);
                    }
                }
                let pages_node = t.add(pages(40).flex(1), children);
                let list = t.add(
                    Node::new(Kind::Padding(Insets::new(
                        24,
                        home_margin(),
                        0,
                        home_margin(),
                    )))
                    .flex(1),
                    [pages_node],
                );
                (list, pages_node)
            }
        };

        // Pager.
        let prev = icon_button(&mut t, Icon::ChevronLeft, 44, action::PREV_PAGE);
        let next = icon_button(&mut t, Icon::ChevronRight, 44, action::NEXT_PAGE);
        let initial_page_of = tf!(Msg::LibraryPagerPageOf, page = 1, count = 1);
        let pager_label = t.text(Paragraph::plain(
            initial_page_of,
            s.font_sm().line_height(1.3),
            Align::Center,
        ));
        let pager_hint = t.text(Paragraph::plain(
            tr(Msg::LibraryPagerHintScroll),
            s.font_xs().gray(INK_MID).line_height(1.3),
            Align::Center,
        ));
        let pager_center_col = t.add(
            Node::column(0).cross(Cross::Center),
            [pager_label, pager_hint],
        );
        let pager_btn = t.add(
            Node::new(Kind::MinSize(crate::geom::Size::new(0, MIN_TARGET)))
                .action(action::JUMP)
                .feedback(Feedback::Invert),
            [pager_center_col],
        );
        let pager_center = t.add(Node::column(0).cross(Cross::Center).flex(1), [pager_btn]);
        let pager_row = t.add(
            Node::row(0).cross(Cross::Center),
            [prev, pager_center, next],
        );
        let pager_h = MIN_TARGET;
        let pager_fixed = t.fixed(None, Some(pager_h), Some(pager_row));
        let pager = t.pad(Insets::new(0, home_margin(), 0, home_margin()), pager_fixed);

        column.extend([list, pager]);
        let col = t.add(Node::column(0).flex(1), column);
        let nav = crate::nav::build(&mut t, s, crate::nav::Tab::Library);
        let content = t.add(Node::column(0), [col, nav.root]);

        let (jump_overlay, jump_label) = build_jump_overlay(&mut t, s);
        let root = t.add(Node::new(Kind::Stack), [content, jump_overlay]);
        t.set_root(root);
        let lib = Self {
            list: pages_node,
            pager_label,
            prev: t.node(prev).children[0],
            next: t.node(next).children[0],
            title,
            loading: bar.loading,
            rows,
            cover_nodes,
            jump_overlay,
            jump_label,
            jump_target: 1,
            jump_open: false,
        };
        (t, lib)
    }

    fn status_badge(t: &mut Tree, s: &Styles, text: &str, finished: bool) -> NodeId {
        let style = if finished {
            s.font_xs().bold().line_height(1.0).gray(PAPER)
        } else {
            s.font_xs().line_height(1.0).gray(INK)
        };
        let label = t.label(text, style);
        let pad = t.pad(Insets::new(2, 18, 0, 18), label);
        let bg = if finished { INK } else { PAPER };
        t.add(
            Node::row(0).bg(bg).border(2, INK, 20).cross(Cross::Center),
            [pad],
        )
    }

    fn book_row(
        t: &mut Tree,
        s: &Styles,
        b: &Book,
        covers: CoverMode,
        in_series: bool,
        item_index: usize,
        id: ActionId,
    ) -> (NodeId, Option<NodeId>) {
        let mut image_node = None;
        let cover_box = if covers != CoverMode::None {
            let cover = Cover::new(&b.title, &b.author, s.family)
                .with_mark(b.mark())
                .with_source_tag(b.source.label())
                .with_image(b.cover.clone().filter(|_| covers == CoverMode::Photos));
            let cover_node = t.custom(cover);
            image_node = Some(cover_node);
            Some(t.fixed(Some(108), Some(144), Some(cover_node)))
        } else {
            None
        };

        let mut tp = Paragraph::plain(
            &b.title,
            s.font_base().bold().line_height(1.25),
            Align::Left,
        );
        tp.style.max_lines = Some(1);
        let title = t.text(tp);

        let src_lbl = b.source.label();
        let meta_str = if let Some((series, idx)) = &b.series {
            if let Some(lbl) = series_label(series, *idx, in_series) {
                format!("{} · {} · {} · {}", b.author, b.format, src_lbl, lbl)
            } else {
                format!("{} · {} · {}", b.author, b.format, src_lbl)
            }
        } else {
            format!("{} · {} · {}", b.author, b.format, src_lbl)
        };
        let mut mp = Paragraph::plain(
            &meta_str,
            s.font_xs().gray(INK_MID).line_height(1.3),
            Align::Left,
        );
        mp.style.max_lines = Some(1);
        let meta = t.text(mp);

        let status_row = match b.status {
            Status::Reading => {
                let bar_node = t.custom(RoundedBar {
                    value: b.progress,
                    height: 10,
                    track_gray: PAPER,
                });
                let bar_box = t.fixed(Some(320), Some(10), Some(bar_node));
                let pct_str = format!("{:.0}%", (b.progress * 100.0).floor());
                let pct = t.label(&pct_str, s.font_xs().line_height(1.0));
                let g = t.gap(16, 1);
                let row = t.add(Node::row(0).cross(Cross::Center), [bar_box, g, pct]);
                t.fixed(None, Some(40), Some(row))
            }
            Status::Finished => {
                let badge = Self::status_badge(t, s, tr(Msg::LibraryBookFinished), true);
                t.fixed(None, Some(40), Some(badge))
            }
            Status::Unread => {
                let badge = Self::status_badge(t, s, tr(Msg::LibraryBookUnread), false);
                t.fixed(None, Some(40), Some(badge))
            }
        };

        let info = t.add(
            Node::column(8).cross(Cross::Start).flex(1),
            [title, meta, status_row],
        );

        let more_icon = t.icon(Icon::More, 40, INK);
        let more_align = t.align(0.5, 0.5, more_icon);
        let more_btn = t.add(
            Node::column(0)
                .action(action::ITEM_MORE + item_index as ActionId)
                .feedback(Feedback::Invert),
            [more_align],
        );
        let more_btn = t.fixed(Some(88), Some(88), Some(more_btn));

        let mut card_children = Vec::new();
        if let Some(cb) = cover_box {
            card_children.push(cb);
        }
        card_children.push(info);
        card_children.push(more_btn);

        let card_row = t.add(Node::row(28).cross(Cross::Center), card_children);
        let card_pad = t.pad(Insets::new(35, 24, 36, 24), card_row);
        let card_bg = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [card_pad]);
        let card_fixed = t.fixed(None, Some(215), Some(card_bg));
        let row = t.add(
            Node::new(Kind::MinSize(crate::geom::Size::new(0, MIN_TARGET)))
                .action(id)
                .feedback(press_feedback(covers))
                .hold_ms(LONG_PRESS_MS)
                .full_width_invert(true),
            [card_fixed],
        );
        (row, image_node)
    }

    fn shelf_row(
        t: &mut Tree,
        s: &Styles,
        icon: Icon,
        name: &str,
        sub: &str,
        id: ActionId,
    ) -> NodeId {
        let icon = t.icon(icon, 72, INK);
        let aligned_icon = t.align(0.5, 0.5, icon);
        let icon_box = t.fixed(Some(108), Some(144), Some(aligned_icon));

        let mut tp = Paragraph::plain(name, s.font_base().bold().line_height(1.25), Align::Left);
        tp.style.max_lines = Some(1);
        let title = t.text(tp);

        let mut sp = Paragraph::plain(sub, s.font_xs().gray(INK_MID).line_height(1.3), Align::Left);
        sp.style.max_lines = Some(1);
        let sub_node = t.text(sp);

        let words = t.add(
            Node::column(8).cross(Cross::Start).flex(1),
            [title, sub_node],
        );

        let chevron = t.icon(Icon::ChevronRight, 40, INK_3);
        let aligned_chevron = t.align(0.5, 0.5, chevron);
        let chevron_box = t.fixed(Some(88), Some(88), Some(aligned_chevron));

        let card_row = t.add(
            Node::row(28).cross(Cross::Center),
            [icon_box, words, chevron_box],
        );
        let card_pad = t.pad(Insets::new(35, 24, 36, 24), card_row);
        let card_bg = t.add(Node::column(0).bg(CARD).border(0, 0, 24), [card_pad]);
        let card_fixed = t.fixed(None, Some(215), Some(card_bg));
        t.add(
            Node::new(Kind::MinSize(crate::geom::Size::new(0, MIN_TARGET)))
                .action(id)
                .feedback(Feedback::Invert)
                .full_width_invert(true),
            [card_fixed],
        )
    }

    fn book_grid_cell(
        t: &mut Tree,
        s: &Styles,
        b: &Book,
        (card_w, cover_h): (i32, i32),
        covers: CoverMode,
        id: ActionId,
    ) -> (NodeId, NodeId) {
        let cover = Cover::new(&b.title, &b.author, s.family)
            .with_mark(b.mark())
            .with_source_tag(b.source.label())
            .with_image(b.cover.clone().filter(|_| covers == CoverMode::Photos));
        let cover_node = t.custom(cover);
        let cover_box = t.fixed(Some(card_w), Some(cover_h), Some(cover_node));

        let mut tp = Paragraph::plain(&b.title, s.font_sm().bold().line_height(1.3), Align::Left);
        tp.style.max_lines = Some(1);
        let title = t.text(tp);
        let title_box = t.fixed(Some(card_w), None, Some(title));

        let status = match b.status {
            Status::Reading => {
                let bar = t.custom(RoundedBar {
                    value: b.progress,
                    height: 10,
                    track_gray: CARD,
                });
                let bar = t.add(Node::column(0).flex(1), [bar]);
                let pct = (b.progress * 100.0).floor();
                let pct = t.label(&format!("{pct:.0}%"), s.font_xs().line_height(1.0));
                let g = t.gap(14, 1);
                t.add(Node::row(0).cross(Cross::Center), [bar, g, pct])
            }
            Status::Finished => {
                let badge = Self::status_badge(t, s, tr(Msg::LibraryBookFinished), true);
                t.add(Node::row(0).cross(Cross::Center), [badge])
            }
            Status::Unread => {
                let badge = Self::status_badge(t, s, tr(Msg::LibraryBookUnread), false);
                t.add(Node::row(0).cross(Cross::Center), [badge])
            }
        };
        let status = t.fixed(Some(card_w), Some(40), Some(status));

        let g1 = t.gap(1, 16);
        let g2 = t.gap(1, 10);
        let col = t.add(
            Node::column(0).cross(Cross::Start),
            [cover_box, g1, title_box, g2, status],
        );
        let col = t.fixed(Some(card_w), None, Some(col));
        let cell = t.add(
            Node::new(Kind::MinSize(crate::geom::Size::new(
                MIN_TARGET, MIN_TARGET,
            )))
            .action(id)
            .feedback(Feedback::Invert)
            .hold_ms(LONG_PRESS_MS),
            [col],
        );
        (cell, cover_node)
    }

    /// Loads photos for the laid-out page and releases photos on hidden pages.
    pub fn update_covers(&self, t: &mut Tree, mut load: impl FnMut(usize) -> Option<GrayImage>) {
        let mut changed = false;
        for &(index, node) in &self.cover_nodes {
            let image = if t.rect(node).w > 0 {
                load(index)
            } else {
                None
            };
            changed |= t
                .widget_mut::<Cover>(node)
                .is_some_and(|cover| cover.set_image(image));
        }
        if changed {
            t.invalidate(self.list, Refresh::Auto);
        }
    }

    fn folder_grid_cell(
        t: &mut Tree,
        s: &Styles,
        name: &str,
        books: usize,
        reading: usize,
        (card_w, cover_h): (i32, i32),
        id: ActionId,
    ) -> NodeId {
        let icon = t.icon(Icon::Folder, 72, INK);
        let icon_box = t.align(0.5, 0.5, icon);
        let icon_box = t.add(Node::column(0).border(2, RULE, 14), [icon_box]);
        let icon_box = t.fixed(Some(card_w), Some(cover_h), Some(icon_box));

        let mut np = Paragraph::plain(name, s.font_sm().bold().line_height(1.3), Align::Left);
        np.style.max_lines = Some(1);
        let name_node = t.text(np);
        let name_box = t.fixed(Some(card_w), None, Some(name_node));

        let mut sub = books_label(books);
        if reading > 0 {
            sub.push_str(" · ");
            sub.push_str(&tf!(Msg::LibraryFolderReading, n = reading));
        }
        let mut sp = Paragraph::plain(
            &sub,
            s.font_xs().gray(INK_MID).line_height(1.3),
            Align::Left,
        );
        sp.style.max_lines = Some(1);
        let sub_node = t.text(sp);
        let sub_box = t.fixed(Some(card_w), Some(28), Some(sub_node));

        let g1 = t.gap(1, 16);
        let g2 = t.gap(1, 10);
        let col = t.add(
            Node::column(0).cross(Cross::Start),
            [icon_box, g1, name_box, g2, sub_box],
        );
        let col = t.fixed(Some(card_w), None, Some(col));
        t.add(
            Node::new(Kind::MinSize(crate::geom::Size::new(
                MIN_TARGET, MIN_TARGET,
            )))
            .action(id)
            .feedback(Feedback::Invert),
            [col],
        )
    }

    fn series_grid_cell(
        t: &mut Tree,
        s: &Styles,
        name: &str,
        books: usize,
        finished: usize,
        (card_w, cover_h): (i32, i32),
        id: ActionId,
    ) -> NodeId {
        let icon = t.icon(Icon::Series, 72, INK);
        let icon_box = t.align(0.5, 0.5, icon);
        let icon_box = t.add(Node::column(0).border(2, RULE, 14), [icon_box]);
        let icon_box = t.fixed(Some(card_w), Some(cover_h), Some(icon_box));

        let mut np = Paragraph::plain(name, s.font_sm().bold().line_height(1.3), Align::Left);
        np.style.max_lines = Some(1);
        let name_node = t.text(np);
        let name_box = t.fixed(Some(card_w), None, Some(name_node));

        let mut sub = trn(Plural::LibraryVolumes, books);
        if finished > 0 {
            sub.push_str(" · ");
            sub.push_str(&tf!(Msg::LibrarySeriesFinished, n = finished));
        }
        let mut sp = Paragraph::plain(
            &sub,
            s.font_xs().gray(INK_MID).line_height(1.3),
            Align::Left,
        );
        sp.style.max_lines = Some(1);
        let sub_node = t.text(sp);
        let sub_box = t.fixed(Some(card_w), Some(28), Some(sub_node));

        let g1 = t.gap(1, 16);
        let g2 = t.gap(1, 10);
        let col = t.add(
            Node::column(0).cross(Cross::Start),
            [icon_box, g1, name_box, g2, sub_box],
        );
        let col = t.fixed(Some(card_w), None, Some(col));
        t.add(
            Node::new(Kind::MinSize(crate::geom::Size::new(
                MIN_TARGET, MIN_TARGET,
            )))
            .action(id)
            .feedback(Feedback::Invert),
            [col],
        )
    }

    /// Updates the pager after layout: page label and arrow states.
    pub fn sync(&self, t: &mut Tree) {
        let (page, count) = t.page(self.list);
        let count = count.max(1);
        t.set_text(self.pager_label, &page_of(page + 1, count));
        t.set_icon_gray(self.prev, if page == 0 { RULE } else { INK });
        t.set_icon_gray(self.next, if page + 1 >= count { RULE } else { INK });
        if self.jump_open {
            t.set_text(self.jump_label, &page_of(self.jump_target, count));
        }
    }

    /// Applies an action that stays within this screen. Returns true if it
    /// was handled.
    pub fn handle(&mut self, t: &mut Tree, id: ActionId) -> bool {
        let (page, count) = t.page(self.list);
        let count = count.max(1);
        match id {
            action::PREV_PAGE => {
                t.set_page(self.list, page.saturating_sub(1));
            }
            action::NEXT_PAGE => {
                t.set_page(self.list, page + 1);
            }
            action::JUMP => {
                self.jump_target = page + 1;
                t.set_text(self.jump_label, &page_of(self.jump_target, count));
                t.set_hidden(self.jump_overlay, false);
                self.jump_open = true;
            }
            action::JUMP_PLUS_1 => {
                self.jump_target = (self.jump_target + 1).min(count);
                t.set_text(self.jump_label, &page_of(self.jump_target, count));
            }
            action::JUMP_MINUS_1 => {
                self.jump_target = self.jump_target.saturating_sub(1).max(1);
                t.set_text(self.jump_label, &page_of(self.jump_target, count));
            }
            action::JUMP_PLUS_5 => {
                self.jump_target = (self.jump_target + 5).min(count);
                t.set_text(self.jump_label, &page_of(self.jump_target, count));
            }
            action::JUMP_MINUS_5 => {
                self.jump_target = self.jump_target.saturating_sub(5).max(1);
                t.set_text(self.jump_label, &page_of(self.jump_target, count));
            }
            action::JUMP_FIRST => {
                self.jump_target = 1;
                t.set_text(self.jump_label, &page_of(self.jump_target, count));
            }
            action::JUMP_LAST => {
                self.jump_target = count;
                t.set_text(self.jump_label, &page_of(self.jump_target, count));
            }
            action::JUMP_GO => {
                t.set_page(self.list, self.jump_target.saturating_sub(1));
                t.set_hidden(self.jump_overlay, true);
                self.jump_open = false;
            }
            action::JUMP_CANCEL => {
                t.set_hidden(self.jump_overlay, true);
                self.jump_open = false;
            }
            _ => return false,
        }
        true
    }

    /// Applies a swipe gesture to flip pages. Up/Left flips to next page,
    /// Down/Right flips to previous page. Returns true if handled.
    pub fn handle_swipe(&mut self, t: &mut Tree, dir: Direction) -> bool {
        if self.jump_open {
            return false;
        }
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

fn jump_btn(
    t: &mut Tree,
    s: &Styles,
    label: &str,
    action: ActionId,
    primary: bool,
    border_w: i32,
) -> NodeId {
    let style = if primary { s.ui_bold() } else { s.ui() };
    let border_gray = if primary { INK } else { RULE };
    let l = t.label(label, style);
    let l = t.pad(Insets::vh(10, 16), l);
    let b = t.button(Node::plain().border(border_w, border_gray, 16), l, action);
    t.add(Node::column(0).flex(1), [b])
}

fn build_jump_overlay(t: &mut Tree, s: &Styles) -> (NodeId, NodeId) {
    let scrim = t.add(
        Node::column(0)
            .flex(1)
            .action(action::JUMP_CANCEL)
            .feedback(Feedback::None),
        [],
    );

    let title = t.label(tr(Msg::LibraryJumpTitle), s.title());
    let jump_label = t.label(&page_of(1, 1), s.headline());
    let label_c = t.add(Node::column(0).cross(Cross::Center), [jump_label]);

    let b_m5 = jump_btn(t, s, "−5", action::JUMP_MINUS_5, false, 2);
    let b_m1 = jump_btn(t, s, "−1", action::JUMP_MINUS_1, false, 2);
    let b_p1 = jump_btn(t, s, "+1", action::JUMP_PLUS_1, false, 2);
    let b_p5 = jump_btn(t, s, "+5", action::JUMP_PLUS_5, false, 2);
    let step_row = t.add(Node::row(16).cross(Cross::Center), [b_m5, b_m1, b_p1, b_p5]);

    let b_first = jump_btn(
        t,
        s,
        tr(Msg::LibraryJumpFirst),
        action::JUMP_FIRST,
        false,
        2,
    );
    let b_last = jump_btn(t, s, tr(Msg::LibraryJumpLast), action::JUMP_LAST, false, 2);
    let bounds_row = t.add(Node::row(16).cross(Cross::Center), [b_first, b_last]);

    let b_go = jump_btn(t, s, tr(Msg::ReaderGo), action::JUMP_GO, true, 3);
    let b_cancel = jump_btn(t, s, tr(Msg::CommonCancel), action::JUMP_CANCEL, false, 2);
    let action_row = t.add(Node::row(16).cross(Cross::Center), [b_go, b_cancel]);

    let g1 = t.gap(1, 12);
    let g2 = t.gap(1, 16);
    let g3 = t.gap(1, 16);
    let g4 = t.gap(1, 20);

    let content = t.add(
        Node::column(0).cross(Cross::Stretch),
        [
            title, g1, label_c, g2, step_row, g3, bounds_row, g4, action_row,
        ],
    );
    let padded = t.pad(Insets::new(20, margin(), 28, margin()), content);
    let rule = t.rule(3, INK);
    let sheet = t.add(Node::column(0).bg(255), [rule, padded]);

    let root = t.add(Node::column(0).hidden(true), [scrim, sheet]);
    (root, jump_label)
}

/// Folder and series shelves of a sample library, for the preview.
pub fn sample_items(books: &[Book], view: View) -> Vec<Item> {
    use std::collections::BTreeMap;
    match view {
        View::Books => books.iter().cloned().map(Item::Book).collect(),
        View::Folders => {
            let mut folders: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
            let mut loose = Vec::new();
            for b in books {
                match &b.folder {
                    Some(f) => {
                        let e = folders.entry(f).or_default();
                        e.0 += 1;
                        e.1 += (b.status == Status::Reading) as usize;
                    }
                    None => loose.push(Item::Book(b.clone())),
                }
            }
            let mut items: Vec<Item> = folders
                .into_iter()
                .map(|(name, (books, reading))| Item::Folder {
                    name: name.into(),
                    books,
                    reading,
                })
                .collect();
            items.extend(loose);
            items
        }
        View::Series => {
            let mut series: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
            for b in books {
                if let Some((name, _)) = &b.series {
                    let e = series.entry(name).or_default();
                    e.0 += 1;
                    e.1 += (b.status == Status::Finished) as usize;
                }
            }
            series
                .into_iter()
                .map(|(name, (books, finished))| Item::Series {
                    name: name.into(),
                    books,
                    finished,
                })
                .collect()
        }
    }
}

/// The series line of a library card: "Name · vol. n", or "Volume n"
/// inside the series' own view. A missing index arrives as 0 (and a 0 or
/// negative one numbers nothing), so it is never shown as "vol. 0": the
/// name alone, or no line inside the series.
fn series_label(name: &str, index: f32, in_series: bool) -> Option<String> {
    let numbered = index > 0.0;
    let n = series_number(index);
    match (in_series, numbered) {
        (true, true) => Some(tf!(Msg::LibraryVolume, n = n)),
        (true, false) => None,
        (false, true) => Some(tf!(Msg::LibrarySeriesVolume, series = name, n = n)),
        (false, false) => Some(name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn series_index_zero_is_never_shown() {
        assert_eq!(
            series_label("Gazeta", 0.0, false).as_deref(),
            Some("Gazeta")
        );
        assert_eq!(series_label("Gazeta", 0.0, true), None);
        aristotle_i18n::with_language("en", || {
            assert_eq!(
                series_label("Dialogues", 3.0, false).as_deref(),
                Some("Dialogues · vol. 3")
            );
            assert_eq!(
                series_label("Dialogues", 2.5, true).as_deref(),
                Some("Volume 2.5")
            );
        });
        aristotle_i18n::with_language("pl", || {
            assert_eq!(
                series_label("Dialogi", 3.0, false).as_deref(),
                Some("Dialogi · tom 3")
            );
            assert_eq!(series_label("Dialogi", 3.0, true).as_deref(), Some("Tom 3"));
            assert_eq!(
                series_label("Dialogi", 2.5, true).as_deref(),
                Some("Tom 2,5")
            );
        });
    }
    use crate::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
    use crate::text::{FamilyId, Fonts};
    use crate::ui::Ui;

    fn ui_for(shelf: &Shelf) -> (Ui, Library) {
        let mut text = TextEngine::new(Fonts::embedded());
        let styles = Styles::new(FamilyId(0));
        let (tree, lib) = Library::build_shelf(&mut text, &styles, shelf);
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        lib.sync(&mut ui.tree);
        ui.render();
        (ui, lib)
    }

    fn home<'a>(items: &'a [Item], view: View, filter: Filter, covers: CoverMode) -> Shelf<'a> {
        Shelf {
            place: Place::Home { view, filter },
            hero: None,
            items,
            sort: "Recent",
            status: StatusLine {
                clock: "12:00",
                battery: Some(0.5),
                charging: false,
            },
            covers,
            layout: LibraryLayout::List,
            local_books_count: items.len(),
        }
    }

    #[test]
    fn shelves_follow_the_status() {
        let books = sample_books();
        let items = sample_items(&books, View::Books);
        let count = |f: Filter| books.iter().filter(|b| f.accepts(b)).count();
        assert_eq!(count(Filter::All), books.len());
        assert_eq!(
            count(Filter::Reading) + count(Filter::New) + count(Filter::Finished),
            books.len()
        );
        let (_, lib) = ui_for(&home(
            &items,
            View::Books,
            Filter::Finished,
            CoverMode::None,
        ));
        assert_eq!(lib.rows.len(), count(Filter::Finished));
    }

    #[test]
    fn folders_mirror_the_tree_with_loose_books_last() {
        let books = sample_books();
        let items = sample_items(&books, View::Folders);
        assert!(matches!(&items[0], Item::Folder { name, .. } if name == "Filozofia"));
        assert!(matches!(items.last(), Some(Item::Book(b)) if b.title == "Moby Dick"));
        let platon = items.iter().find_map(|i| match i {
            Item::Folder {
                name,
                books,
                reading,
            } if name == "Platon" => Some((*books, *reading)),
            _ => None,
        });
        assert_eq!(platon, Some((3, 2)));
        let (_, lib) = ui_for(&home(&items, View::Folders, Filter::All, CoverMode::None));
        assert_eq!(lib.rows.len(), items.len());
    }

    #[test]
    fn empty_library_still_builds_with_one_page() {
        let (ui, lib) = ui_for(&home(&[], View::Books, Filter::All, CoverMode::Cards));
        assert!(lib.rows.is_empty());
        assert_eq!(ui.tree.page(lib.list).1, 1);
    }

    #[test]
    fn photo_covers_only_when_asked() {
        let mut books = sample_books();
        // A black cover: the row's cover box turns dark only with photos.
        books[1].cover = Some(GrayImage::new(200, 300, vec![0; 200 * 300]));
        let items = sample_items(&books[1..2], View::Books);
        let dark = |covers| {
            let (ui, lib) = ui_for(&home(&items, View::Books, Filter::All, covers));
            let r = ui.tree.rect(lib.rows[0]);
            (r.y..r.bottom())
                .filter(|&y| ui.screen().get(r.x + 24 + 36, y) < 40)
                .count()
        };
        assert!(dark(CoverMode::Photos) > 80);
        assert!(dark(CoverMode::None) < 40);
        // A black cover fills the compact cell, so the whole screen
        // has thousands more dark pixels with
        // photos than with the generated card.
        let dark_grid = |covers| {
            let (ui, _) = ui_for(&Shelf {
                layout: LibraryLayout::Grid,
                ..home(&items, View::Books, Filter::All, covers)
            });
            let c = ui.screen();
            let mut n = 0;
            for y in 0..c.height as i32 {
                for x in 0..c.width as i32 {
                    n += usize::from(c.get(x, y) < 40);
                }
            }
            n
        };
        assert!(
            dark_grid(CoverMode::Photos) > dark_grid(CoverMode::Cards) + 30_000,
            "the grid draws the photo only in the photos setting"
        );
    }

    #[test]
    fn plurals_follow_the_language() {
        aristotle_i18n::with_language("pl", || {
            assert_eq!(books_label(1), "1 książka");
            assert_eq!(books_label(3), "3 książki");
            assert_eq!(books_label(5), "5 książek");
            assert_eq!(books_label(12), "12 książek");
            assert_eq!(books_label(22), "22 książki");
        });
        aristotle_i18n::with_language("en", || {
            assert_eq!(books_label(1), "1 book");
            assert_eq!(books_label(22), "22 books");
        });
    }

    #[test]
    fn every_row_is_a_full_target() {
        let books = sample_books();
        for view in View::ALL {
            let items = sample_items(&books, view);
            let (ui, lib) = ui_for(&home(&items, view, Filter::All, CoverMode::Cards));
            for r in &lib.rows {
                let rect = ui.tree.rect(*r);
                if !rect.is_empty() {
                    assert!(rect.h >= MIN_TARGET, "{view:?}: {rect:?}");
                }
            }
        }
    }

    #[test]
    fn filters_keep_their_labels_and_touch_targets_on_narrow_screens() {
        for lang in ["en", "pl"] {
            aristotle_i18n::with_language(lang, || {
                for (width, height, dpi) in [
                    (800, 1200, 300),
                    (1072, 1448, 300),
                    (1236, 1648, 300),
                    (600, 800, 167),
                    (758, 1024, 212),
                ] {
                    let profile = crate::geom::Screen::new(width, height, dpi).unwrap();
                    for selected in Filter::ALL {
                        let mut text = TextEngine::new(Fonts::embedded());
                        let styles = Styles::new(FamilyId(0)).with_density(profile.density);
                        let shelf = home(&[], View::Books, selected, CoverMode::None);
                        let (tree, _) = Library::build_shelf_for_width(
                            &mut text,
                            &styles,
                            &shelf,
                            width as i32,
                        );
                        let mut ui = Ui::with_screen(tree, text, profile);
                        ui.layout();
                        for (i, f) in Filter::ALL.iter().enumerate() {
                            let action = action::FILTER + i as ActionId;
                            let id = (0..ui.tree.len())
                                .map(|n| NodeId(n as u32))
                                .find(|n| ui.tree.node(*n).action == Some(action))
                                .unwrap();
                            let rect = ui.tree.rect(id);
                            assert_eq!(rect.intersect(&ui.bounds()), Some(rect));
                            let style = if *f == selected {
                                styles.ui_bold()
                            } else {
                                styles.ui()
                            };
                            let label = ui.text.measure(f.label(), &style).ceil() as i32;
                            assert!(
                                rect.w
                                    >= label
                                        + 2 * profile.density.px(crate::components::CHIP_PADDING_X),
                                "{lang} {width}: {f:?} {rect:?}"
                            );
                            assert_eq!(ui.tree.hit(rect.center()), crate::tree::Hit::Target(id));
                        }
                    }
                }
            });
        }
    }

    #[test]
    fn titles_and_pager_fit_on_screen() {
        let books = sample_books();
        let items = sample_items(&books, View::Books);
        let mut shelf = home(&items, View::Books, Filter::All, CoverMode::Cards);
        shelf.hero = Some(&books[0]);
        let (ui, lib) = ui_for(&shelf);
        let pager = ui.tree.rect(lib.next);
        assert!(pager.bottom() <= SCREEN_HEIGHT, "{pager:?}");
        assert!(ui.tree.page(lib.list).1 >= 2);
    }

    #[test]
    fn grid_layout_paginates_six_items_per_page() {
        let books = sample_books();
        let items = sample_items(&books, View::Books);
        let mut shelf = home(&items, View::Books, Filter::All, CoverMode::Cards);
        shelf.layout = LibraryLayout::Grid;
        let (mut ui, mut lib) = ui_for(&shelf);
        assert_eq!(lib.rows.len(), items.len());
        let expected_pages = items.len().div_ceil(6);
        let (page, count) = ui.tree.page(lib.list);
        assert_eq!(page, 0);
        assert_eq!(count, expected_pages);

        // Turn to page 2.
        assert!(lib.handle(&mut ui.tree, action::NEXT_PAGE));
        ui.layout();
        lib.sync(&mut ui.tree);
        assert_eq!(ui.tree.page(lib.list).0, 1);
    }

    #[test]
    fn grid_layout_folders_and_series() {
        let books = sample_books();
        for view in [View::Folders, View::Series] {
            let items = sample_items(&books, view);
            let mut shelf = home(&items, view, Filter::All, CoverMode::Cards);
            shelf.layout = LibraryLayout::Grid;
            let (ui, lib) = ui_for(&shelf);
            assert_eq!(lib.rows.len(), items.len());
            for r in &lib.rows {
                let rect = ui.tree.rect(*r);
                if !rect.is_empty() {
                    assert!(rect.h >= MIN_TARGET, "{view:?}: {rect:?}");
                    assert!(rect.w >= MIN_TARGET, "{view:?}: {rect:?}");
                }
            }
        }
    }

    #[test]
    fn list_layout_paginates_complete_rows() {
        let books = sample_books();
        let items = sample_items(&books, View::Books);
        let shelf = home(&items, View::Books, Filter::All, CoverMode::Cards);
        let (mut ui, mut lib) = ui_for(&shelf);
        assert_eq!(lib.rows.len(), items.len());
        let expected_pages = items.len().div_ceil(3);
        let (page, count) = ui.tree.page(lib.list);
        assert_eq!(page, 0);
        assert_eq!(count, expected_pages);

        // Turn to page 2.
        assert!(lib.handle(&mut ui.tree, action::NEXT_PAGE));
        ui.layout();
        lib.sync(&mut ui.tree);
        assert_eq!(ui.tree.page(lib.list).0, 1);
    }

    #[test]
    fn photo_loading_tracks_only_the_visible_page() {
        for (layout, per_page) in [(LibraryLayout::List, 3), (LibraryLayout::Grid, 6)] {
            let items: Vec<Item> = (0..200)
                .map(|i| Item::Book(Book::new(&format!("Book {i}"), "Test author", 0.0)))
                .collect();
            let mut shelf = home(&items, View::Books, Filter::All, CoverMode::Photos);
            shelf.layout = layout;
            let mut text = TextEngine::new(Fonts::embedded());
            let (tree, mut lib) =
                Library::build_shelf(&mut text, &Styles::new(FamilyId(0)), &shelf);
            let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
            ui.layout();
            lib.sync(&mut ui.tree);
            let mut eager_items = items.clone();
            for item in &mut eager_items {
                if let Item::Book(book) = item {
                    book.cover = Some(GrayImage::new(2, 2, vec![42; 4]));
                }
            }
            let mut eager_shelf = home(&eager_items, View::Books, Filter::All, CoverMode::Photos);
            eager_shelf.layout = layout;
            let (mut eager_ui, eager_lib) = ui_for(&eager_shelf);
            for page in [0, 1, 10, 0] {
                ui.tree.set_page(lib.list, page);
                ui.layout();
                let mut loaded = Vec::new();
                let mut weak = Vec::new();
                lib.update_covers(&mut ui.tree, |index| {
                    loaded.push(index);
                    let image = GrayImage::new(2, 2, vec![42; 4]);
                    weak.push(Rc::downgrade(&image.pixels));
                    Some(image)
                });
                assert_eq!(
                    loaded,
                    (page * per_page..(page + 1) * per_page).collect::<Vec<_>>()
                );
                ui.render();
                eager_ui.tree.set_page(eager_lib.list, page);
                eager_ui.render();
                assert!(
                    ui.screen() == eager_ui.screen(),
                    "{layout:?} page {page}, pixel differences {:?}",
                    ui.screen().diff_bounds(eager_ui.screen(), ui.bounds())
                );
                let photos = lib
                    .cover_nodes
                    .iter()
                    .filter(|(_, node)| ui.tree.widget::<Cover>(*node).unwrap().image.is_some())
                    .count();
                assert_eq!(photos, per_page);
                assert!(lib.handle(&mut ui.tree, action::NEXT_PAGE));
                ui.layout();
                lib.update_covers(&mut ui.tree, |_| None);
                assert!(weak.iter().all(|image| image.upgrade().is_none()));
            }
        }
    }
}
