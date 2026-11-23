// SPDX-License-Identifier: AGPL-3.0-or-later
//! The first screen of the app.
//!
//! The layout is the one of the prototype in `docs/ui/prototype-v2`, and
//! the screenshot `docs/ui/prototype-v2/reference/home.png` is the
//! yardstick: the clock card, the card of the book being read with the
//! desk cat fishing from it, the three statistics, the books opened lately
//! and the quotation of the day.
//!
//! Everything the screen shows is read once, when it is built: the books of
//! the library, the statistics of today, the quotation and the lunar
//! calendar. Nothing here keeps a timer or a thread alive; the clock ticks
//! by the app changing one word of the tree, which touches one small
//! rectangle of the screen.

use super::library::{Book, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::status::{self, LoadingNodes, StatusBar};
use crate::text::{Align, Paragraph, TextEngine, TextStyle};
use crate::theme::{home_margin, PanelLayout, Styles, CARD_BG, INK, INK_2, PAPER, RULE};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use crate::widgets::Cover;
use aristotle_i18n::{tf, tr, Msg};

pub mod lunar;
pub mod pet;
pub mod quotes;

pub mod action {
    use crate::tree::ActionId;
    /// The card of the book being read.
    pub const RESUME: ActionId = 1;
    /// The cell that leads to the whole library.
    pub const ALL_BOOKS: ActionId = 2;
    /// `RECENT + i` opens the `i`-th book of the recent row.
    pub const RECENT: ActionId = 10;
    /// The button that asks for another quotation.
    pub const NEXT_QUOTE: ActionId = 20;
}

/// The space between the status bar and the first block.
const TOP_PAD: i32 = 28;
/// The space between the last block and the navigation bar.
const BOTTOM_PAD: i32 = 32;
/// The space between two blocks of the content area.
const BLOCK_GAP: i32 = 40;
/// The height of the card of the book being read, without its padding.
const CARD_H: i32 = 360;
/// The padding inside the card of the book being read.
const CARD_PAD: i32 = 36;
/// The size of the cover on the card of the book being read.
const CARD_COVER: (i32, i32) = (240, 320);
/// The size of a cover in the row of recent books.
const RECENT_COVER: (i32, i32) = (144, 192);
/// The width of the column that holds the button of the reading card.
const BUTTON_COL: i32 = 264;
/// The side of the dots that show the page of the recent block.
const DOT: i32 = 12;
/// How many books the recent block shows on one page.
pub const RECENT_PER_PAGE: usize = 3;
/// How many pages of recent books the home screen holds.
pub const RECENT_PAGES: usize = 3;
/// How many books the recent block holds at most.
pub const RECENT_BOOKS: usize = RECENT_PER_PAGE * RECENT_PAGES;

/// How the clock card looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClockStyle {
    /// A big time on the left, the date and the lunar calendar on the
    /// right. The default.
    #[default]
    BigLunar,
    /// A big time on the left, the weekday big on the right.
    BigWeek,
    /// One line: the time on the left, the date on the right.
    Line,
    /// No clock card: the status bar shows the time instead.
    Off,
}

impl ClockStyle {
    /// Every style, in the order the settings offer them.
    pub const ALL: [ClockStyle; 4] = [
        ClockStyle::BigLunar,
        ClockStyle::BigWeek,
        ClockStyle::Line,
        ClockStyle::Off,
    ];
}

/// What the clock card says.
#[derive(Clone, Debug, Default)]
pub struct ClockText {
    /// "21:38".
    pub time: String,
    /// "11月4日 星期三".
    pub date: String,
    /// "11月4日".
    pub month_day: String,
    /// "星期三".
    pub weekday: String,
    /// "丙午年 九月廿六".
    pub lunar: String,
    /// The lunar month and day alone: "九月廿六".
    pub lunar_short: String,
    /// "立冬前三日", when a solar term is within a week.
    pub term: Option<String>,
}

/// The three numbers of the statistics row.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Minutes read today.
    pub today_minutes: u32,
    /// Days read in a row.
    pub streak_days: u32,
    /// Hours read this week.
    pub week_hours: f32,
}

/// What the home screen shows.
pub struct Home<'a> {
    /// The book being read.
    pub reading: Option<&'a Book>,
    /// The books opened most recently, at most [`RECENT_BOOKS`] of them.
    pub recent: &'a [Book],
    /// The facts of the status bar.
    pub status: StatusLine<'a>,
    /// The style of the clock card.
    pub clock: ClockStyle,
    /// What the clock card says.
    pub clock_text: ClockText,
    /// The statistics, when the row of them is on.
    pub stats: Option<Stats>,
    /// The quotation, when the card of it is on.
    pub quote: Option<&'a quotes::QuoteItem>,
    /// Whether the desk cat fishes from the reading card.
    pub pet: bool,
}

/// The words of the quotation card.
pub struct QuoteNodes {
    /// The quotation itself.
    pub text: NodeId,
    /// The line under it: the author and the book.
    pub byline: NodeId,
    /// The words of the mark the app's own quotations carry.
    pub mark: NodeId,
    /// The box around the mark.
    pub mark_box: NodeId,
}

/// Handles to the nodes of the home screen.
pub struct HomeUi {
    /// The card of the book being read, when there is one.
    pub card: Option<NodeId>,
    /// The area the blocks of the screen live in.
    pub content: NodeId,
    /// The navigation bar at the foot of the screen.
    pub nav: NodeId,
    /// The desk cat, when it is on.
    pub pet: Option<NodeId>,
    /// The time of the clock card, when there is one.
    pub clock: Option<NodeId>,
    /// The time in the status bar, when the clock card is off.
    pub status_time: Option<NodeId>,
    /// The words of the quotation card.
    pub quote: Option<QuoteNodes>,
    /// The cells of the recent block, in the order the books come.
    pub recent: Vec<NodeId>,
    /// The pages of the recent block.
    pub recent_pages: NodeId,
    /// The dots that show which page of the recent block is up.
    pub dots: Vec<NodeId>,
    /// The loading area in the middle of the status line, hidden.
    pub loading: LoadingNodes,
    /// Panel density used to scale the desk cat on compact/low-DPI panels.
    density: crate::geom::Density,
}

impl HomeUi {
    /// Builds the home screen.
    ///
    /// `screen_w` is the width of the panel in its own pixels. The clock
    /// card needs it to know whether the date and the solar term fit on
    /// one line: a language with longer words needs a second line.
    pub fn build(text: &mut TextEngine, s: &Styles, home: &Home, screen_w: i32) -> (Tree, Self) {
        let est_h = (screen_w as i64 * 4 / 3) as i32;
        Self::build_for_size(text, s, home, Size::new(screen_w, est_h))
    }

    /// Builds the home screen for a panel of `size` native pixels.
    /// Six-inch panels tighten vertical rhythm and card heights so the clock,
    /// reading card, statistics, recent row and quotation card all fit above
    /// the navigation bar without collapsing.
    pub fn build_for_size(
        text: &mut TextEngine,
        s: &Styles,
        home: &Home,
        size: Size,
    ) -> (Tree, Self) {
        let compact = PanelLayout::for_size(size, s.density) == PanelLayout::Compact;
        let ty = Type::new(s, compact);
        let mut t = Tree::new();
        let screen_w = size.w;

        // The status bar: the slogan on the left, the loading area in the
        // middle and the wireless link with the charge and its battery on
        // the right. It stands above the padded area of the blocks, the way
        // the prototype has it: the bar is 88 px tall and the blocks start
        // 28 px below it.
        let bar = status_bar(&mut t, &ty, &home.status, home.clock);
        let status_time = bar.time;
        let status = t.add(
            Node::new(Kind::Padding(Insets::new(
                0,
                home_margin(),
                0,
                home_margin(),
            ))),
            [bar.root],
        );

        // The clock card, when there is one.
        let mut inner = Vec::new();
        let clock = match home.clock {
            ClockStyle::Off => None,
            style => {
                let (card, time) = clock_card(
                    &mut t,
                    text,
                    s,
                    &ty,
                    style,
                    &home.clock_text,
                    screen_w,
                    compact,
                );
                inner.push(card);
                Some(time)
            }
        };

        // The blocks of the screen: the card of the book being read, the
        // statistics, the books opened lately and the quotation.
        let mut blocks = Vec::new();
        let mut card = None;
        if let Some(b) = home.reading {
            let c = reading_card(&mut t, text, s, &ty, b, compact);
            blocks.push(c);
            card = Some(c);
        } else {
            // Nothing to continue: the screen says so rather than leaving a
            // hole where the card would be.
            let hint = t.text(Paragraph::plain(
                tr(Msg::HomeNoReading),
                ty.meta.clone().gray(INK_2),
                Align::Left,
            ));
            blocks.push(hint);
        }
        if let Some(st) = home.stats {
            blocks.push(stats_row(&mut t, &ty, st, compact));
        }
        let recent = recent_block(&mut t, s, &ty, home.recent, compact);
        blocks.push(recent.block);
        let quote = home.quote.map(|q| quote_card(&mut t, &ty, q, compact));

        let block_gap = if compact { 16 } else { BLOCK_GAP };
        let top_pad = if compact { 12 } else { TOP_PAD };
        let bottom_pad = if compact { 12 } else { BOTTOM_PAD };

        // The blocks stand apart, and the quotation card sits at the
        // foot of the area, the way the prototype pushes it down.
        let mut content_children = vec![t.gap(0, block_gap)];
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                content_children.push(t.gap(0, block_gap));
            }
            content_children.push(*b);
        }
        let (quote_node, quote_nodes) = match quote {
            Some((node, nodes)) => {
                content_children.push(t.spacer(1));
                content_children.push(node);
                (Some(node), Some(nodes))
            }
            None => (None, None),
        };
        let _ = quote_node;
        let content = t.add(Node::column(0).flex(1), content_children);

        // The clock card and the blocks share the padded area between the
        // status bar and the navigation bar.
        inner.push(content);
        let inner = t.add(Node::column(0).flex(1), inner);
        let inner = t.add(
            Node::new(Kind::Padding(Insets::new(
                top_pad,
                home_margin(),
                bottom_pad,
                home_margin(),
            )))
            .flex(1),
            [inner],
        );
        let nav = crate::nav::build(&mut t, s, crate::nav::Tab::Home);
        let main = t.add(Node::column(0), [status, inner, nav.root]);

        // The desk cat draws over the whole screen: the cat on the reading
        // card, the line down the margin and the fish in the navigation
        // bar.
        let pet_node = if home.pet {
            Some(t.custom(pet::Pet::new()))
        } else {
            None
        };
        let root = match pet_node {
            Some(p) => t.add(Node::new(Kind::Stack), [main, p]),
            None => main,
        };
        t.set_root(root);
        (
            t,
            HomeUi {
                card,
                content,
                nav: nav.root,
                pet: pet_node,
                clock,
                status_time,
                quote: quote_nodes,
                recent: recent.cells,
                recent_pages: recent.pages,
                dots: recent.dots,
                loading: bar.loading,
                density: s.density,
            },
        )
    }

    /// Fills the dots of the recent block after its page has changed.
    pub fn sync(&self, t: &mut Tree) {
        let (page, _) = t.page(self.recent_pages);
        for (i, &dot) in self.dots.iter().enumerate() {
            t.set_bg(dot, if i == page { INK } else { RULE });
        }
    }

    /// Puts the desk cat where the laid-out screen says it goes. The tree
    /// has to be laid out first: the cat hangs from the reading card and
    /// the fish bites in the navigation bar.
    ///
    /// The cat sits on the card of the book being read, so a library with
    /// nothing to continue has no cat, no line and no fish.
    pub fn place_pet(&self, t: &mut Tree) {
        let Some(id) = self.pet else { return };
        let content = t.rect(self.content);
        let nav = t.rect(self.nav);
        let card_rect = self.card.map(|c| t.rect(c));
        let Some(widget) = t.widget_mut::<pet::Pet>(id) else {
            return;
        };
        let Some(card_rect) = card_rect else {
            widget.active = false;
            return;
        };
        widget.geometry =
            pet::Geometry::for_card(content, card_rect, nav.right(), nav.y, self.density);
    }

    /// Changes the time the clock card shows. Only the word changes, so
    /// only the rectangle of that word goes out.
    pub fn set_time(&self, t: &mut Tree, time: &str) {
        if let Some(node) = self.clock {
            t.set_text(node, time);
        }
        if let Some(node) = self.status_time {
            t.set_text(node, time);
        }
    }

    /// Changes the quotation, and the line under it.
    pub fn set_quote(&self, t: &mut Tree, q: &quotes::QuoteItem, brand: &str) {
        let Some(nodes) = &self.quote else { return };
        t.set_text(nodes.text, &format!("「{}」", q.text));
        if q.is_original {
            t.set_text(nodes.byline, "");
            t.set_text(nodes.mark, brand);
            t.set_hidden(nodes.mark_box, false);
        } else {
            t.set_text(nodes.byline, &format!("—— {}{}", q.author, q.source));
            t.set_text(nodes.mark, "");
            t.set_hidden(nodes.mark_box, true);
        }
    }
}

/// The type of the home screen, in the prototype's pixels.
struct Type {
    /// The big time of the clock card: 160 px.
    clock: TextStyle,
    /// The time of the one-line clock: 96 px.
    clock_line: TextStyle,
    /// The weekday of the "big weekday" clock: 56 px.
    weekday: TextStyle,
    /// The title of the book being read: 64 px.
    book: TextStyle,
    /// A number of the statistics row: 48 px.
    number: TextStyle,
    /// A section title, a date: 40 px.
    section: TextStyle,
    /// A date that is not a title: 40 px.
    date: TextStyle,
    /// The quotation: 40 px.
    quote: TextStyle,
    /// The lunar calendar: 36 px.
    lunar: TextStyle,
    /// The words on a black button: 36 px.
    button: TextStyle,
    /// A line of metadata: 34 px.
    meta: TextStyle,
    /// The title of a book in the recent row: 34 px.
    recent: TextStyle,
    /// A percentage: 40 px.
    percent: TextStyle,
    /// A small label: 32 px.
    small: TextStyle,
    /// The words of a chip: 32 px.
    chip: TextStyle,
}

impl Type {
    fn new(s: &Styles, compact: bool) -> Self {
        let make = |size: f32, lh: f32, bold: bool| {
            // The sizes are the prototype's, in reference pixels.
            let st = TextStyle::new(s.family, size)
                .at_density(s.density)
                .line_height(lh);
            if bold {
                st.bold()
            } else {
                st
            }
        };
        if compact {
            Type {
                clock: make(116.0, 0.86, false),
                clock_line: make(76.0, 1.0, true),
                weekday: make(44.0, 1.15, true),
                book: make(48.0, 1.15, true),
                number: make(42.0, 1.1, true),
                section: make(36.0, 1.15, true),
                date: make(34.0, 1.15, false),
                quote: make(34.0, 1.35, false),
                lunar: make(32.0, 1.15, false),
                button: make(34.0, 1.15, true),
                meta: make(30.0, 1.25, false),
                recent: make(32.0, 1.25, true),
                percent: make(34.0, 1.0, true),
                small: make(30.0, 1.15, false),
                chip: make(28.0, 1.0, false),
            }
        } else {
            Type {
                clock: make(160.0, 0.86, false),
                clock_line: make(96.0, 1.0, true),
                weekday: make(56.0, 1.2, true),
                book: make(64.0, 1.2, true),
                number: make(48.0, 1.1, true),
                section: make(40.0, 1.2, true),
                date: make(40.0, 1.2, false),
                quote: make(40.0, 1.5, false),
                lunar: make(36.0, 1.2, false),
                button: make(36.0, 1.2, true),
                meta: make(34.0, 1.4, false),
                recent: make(34.0, 1.3, true),
                percent: make(40.0, 1.0, true),
                small: make(32.0, 1.2, false),
                chip: make(32.0, 1.0, false),
            }
        }
    }
}

/// The status bar: the slogan on the left, the loading area in the middle
/// and the wireless link with the battery on the right.
///
/// The bar of the home screen is its own: it has its own type scale, and
/// the time stands on the left when the clock card is off.
fn status_bar(t: &mut Tree, ty: &Type, line: &StatusLine, clock: ClockStyle) -> StatusBar {
    let left = if clock == ClockStyle::Off {
        status::Left::TimeAndSlogan
    } else {
        status::Left::Slogan
    };
    status::status_bar(t, &ty.small, line, left)
}

/// The clock card. Its style decides what stands on the right of the
/// time: the date and the lunar calendar, the weekday, or the date alone.
///
/// The date, the lunar calendar and the solar term stand on two lines, the
/// solar term beside the calendar, as the prototype has them. A language
/// with longer words than the Chinese needs a third line: the term then
/// stands below the calendar, and the card grows by the height of the
/// term.
#[allow(clippy::too_many_arguments)]
fn clock_card(
    t: &mut Tree,
    engine: &mut TextEngine,
    s: &Styles,
    ty: &Type,
    style: ClockStyle,
    text: &ClockText,
    screen_w: i32,
    compact: bool,
) -> (NodeId, NodeId) {
    let time = match style {
        ClockStyle::Line => t.label(&text.time, ty.clock_line.clone()),
        _ => t.label(&text.time, ty.clock.clone()),
    };

    let right = match style {
        ClockStyle::BigLunar => {
            let date = t.label(&text.date, ty.section.clone());
            let mut term_row = None;
            if let Some(term) = &text.term {
                let chip = chip(t, ty, term, if compact { 16 } else { 22 }, compact);
                term_row = Some(chip);
            }
            // The term stands beside the calendar when the two fit on one
            // line, and below it when they do not.
            let beside = term_row
                .as_ref()
                .map(|_| {
                    let d = s.density;
                    let time_w = engine.measure(&text.time, &ty.clock).ceil() as i32;
                    // The card's own padding, and the two gaps of the row
                    // that holds the time and the date.
                    let room =
                        screen_w - 2 * d.px(home_margin()) - 2 * d.px(40) - time_w - 2 * d.px(40);
                    let lunar_w = engine.measure(&text.lunar, &ty.lunar).ceil() as i32;
                    let term_w = text
                        .term
                        .as_ref()
                        .map(|term| engine.measure(term, &ty.chip).ceil() as i32 + 2 * d.px(22))
                        .unwrap_or(0);
                    lunar_w + d.px(20) + term_w <= room
                })
                .unwrap_or(false);
            let col_gap = if compact { 8 } else { 12 };
            match term_row {
                None => {
                    let lunar = t.label(&text.lunar, ty.lunar.clone());
                    t.add(Node::column(col_gap).cross(Cross::End), [date, lunar])
                }
                Some(chip) if beside => {
                    let lunar = t.label(&text.lunar, ty.lunar.clone());
                    let g = t.gap(20, 1);
                    let row = t.add(Node::row(0).cross(Cross::Center), [lunar, g, chip]);
                    t.add(Node::column(col_gap).cross(Cross::End), [date, row])
                }
                Some(chip) => {
                    let lunar = t.label(&text.lunar, ty.lunar.clone());
                    t.add(Node::column(col_gap).cross(Cross::End), [date, lunar, chip])
                }
            }
        }
        ClockStyle::BigWeek => {
            let weekday = t.label(&text.weekday, ty.weekday.clone());
            let date = t.label(&text.month_day, ty.date.clone());
            t.add(
                Node::column(if compact { 6 } else { 10 }).cross(Cross::End),
                [weekday, date],
            )
        }
        ClockStyle::Line => {
            let mut line = text.date.clone();
            if !text.lunar_short.is_empty() {
                line.push_str(" · ");
                line.push_str(&text.lunar_short);
            }
            t.label(&line, ty.date.clone())
        }
        ClockStyle::Off => unreachable!("the clock card is not built when it is off"),
    };

    let (padding, radius, gap) = match (style, compact) {
        (ClockStyle::Line, true) => (Insets::vh(14, 28), 20, 24),
        (ClockStyle::Line, false) => (Insets::vh(24, 36), 24, 32),
        (_, true) => (Insets::vh(16, 30), 24, 24),
        (_, false) => (Insets::vh(28, 40), 32, 40),
    };
    let fill = t.spacer(1);
    let row = t.add(Node::row(gap).cross(Cross::Center), [time, fill, right]);
    let card = t.add(
        Node::new(Kind::Padding(padding))
            .bg(CARD_BG)
            .border(0, CARD_BG, radius),
        [row],
    );
    (card, time)
}

/// A small outlined label, the way the prototype marks the card.
fn chip(t: &mut Tree, ty: &Type, text: &str, pad_x: i32, compact: bool) -> NodeId {
    let label = t.label(text, ty.chip.clone());
    let pad_y = if compact { 8 } else { 14 };
    t.add(
        Node::new(Kind::Padding(Insets::vh(pad_y, pad_x)))
            .bg(PAPER)
            .border(2, INK, 16),
        [label],
    )
}

/// The card of the book being read: the cover, the title, where it
/// stopped, how far it has come, and the way back into it.
fn reading_card(
    t: &mut Tree,
    text: &mut TextEngine,
    s: &Styles,
    ty: &Type,
    b: &Book,
    compact: bool,
) -> NodeId {
    let card_h = if compact { 288 } else { CARD_H };
    let card_pad = if compact { 22 } else { CARD_PAD };
    let card_cover = if compact { (180, 240) } else { CARD_COVER };
    let button_col_min = if compact { 220 } else { BUTTON_COL };

    // The chip that says what the card is.
    let chip = chip(
        t,
        ty,
        tr(Msg::HomeReadingNow),
        if compact { 16 } else { 22 },
        compact,
    );

    // The title of the book.
    let mut tp = Paragraph::plain(&b.title, ty.book.clone(), Align::Left);
    tp.style.max_lines = Some(2);
    let title = t.text(tp);
    let title = t.pad(Insets::new(if compact { 10 } else { 22 }, 0, 0, 0), title);

    // The author and the chapter on one line, as the prototype has it.
    let mut sub = b.author.clone();
    if !b.chapter.is_empty() {
        if !sub.is_empty() {
            sub.push_str(" · ");
        }
        sub.push_str(&b.chapter);
    }
    let mut sp = Paragraph::plain(&sub, ty.meta.clone().gray(INK_2), Align::Left);
    sp.style.max_lines = Some(1);
    let sub = t.text(sp);
    let sub = t.pad(Insets::new(if compact { 4 } else { 6 }, 0, 0, 0), sub);

    let head = t.add(Node::column(0).cross(Cross::Start), [chip, title, sub]);
    // What is left of the book, when the statistics can say: whole hours,
    // at least one, the way the prototype writes it.
    let head = match b.minutes_left {
        Some(minutes) => {
            let hours = ((minutes + 30) / 60).max(1);
            let left = t.label(
                &tf!(Msg::HomeLeftHours, hours = hours),
                ty.meta.clone().gray(INK_2),
            );
            let left = t.pad(Insets::new(if compact { 2 } else { 4 }, 0, 0, 0), left);
            t.add(Node::column(0).cross(Cross::Start), [head, left])
        }
        None => head,
    };

    // How far the book has come: the bar and the number, side by side.
    let pct = (b.progress * 100.0).floor();
    let bar = t.custom(Bar {
        value: b.progress,
        height: 12,
        track: PAPER,
    });
    let bar = t.add(Node::column(0).flex(1), [bar]);
    let number = t.label(&format!("{pct:.0}%"), ty.percent.clone());
    let g = t.gap(if compact { 16 } else { 24 }, 1);
    let where_ = t.add(Node::row(0).cross(Cross::Center), [bar, g, number]);

    // The title at the top of the column, the bar at its foot.
    let fill = t.spacer(1);
    let info = t.add(Node::column(0), [head, fill, where_]);
    // The column takes the room the card has left over.
    let info = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(card_h),
        })
        .flex(1),
        [info],
    );

    // The cover, standing in the middle of the card.
    let cover = Cover::new(&b.title, &b.author, s.family)
        .with_mark(b.mark())
        .with_source_tag(b.source.label())
        .with_image(b.cover.clone());
    let cover = t.custom(cover);
    let cover = t.fixed(Some(card_cover.0), Some(card_cover.1), Some(cover));

    // The button that goes back into the book, in the corner of the card.
    // Its column is as wide as the button, so the words of a language
    // with longer words stay on one line.
    let (button, button_w) = resume_button(t, text, s, ty, compact);
    let spacer = t.spacer(1);
    let button_col = t.add(Node::column(0).cross(Cross::End), [spacer, button]);
    let button_col = t.add(
        Node::new(Kind::Fixed {
            w: Some(button_col_min.max(button_w)),
            h: Some(card_h),
        }),
        [button_col],
    );

    // The cover stands in the middle of the card, the other two columns
    // fill its height.
    let row = t.add(
        Node::row(if compact { 24 } else { 36 }).cross(Cross::Center),
        [cover, info, button_col],
    );
    let row = t.fixed(None, Some(card_h), Some(row));

    // The card is a light gray box with round corners, and the whole of
    // it opens the book.
    t.add(
        Node::new(Kind::Padding(Insets::all(card_pad)))
            .bg(CARD_BG)
            .border(0, CARD_BG, if compact { 24 } else { 32 })
            .action(action::RESUME)
            .feedback(Feedback::Invert),
        [row],
    )
}

/// The black button that goes back into the book being read. It returns
/// the button and its width in reference pixels.
fn resume_button(
    t: &mut Tree,
    text: &mut TextEngine,
    s: &Styles,
    ty: &Type,
    compact: bool,
) -> (NodeId, i32) {
    let label = tr(Msg::HomeContinue);
    let style = ty.button.clone().gray(PAPER);
    // `text.measure` returns native pixels at `s.density`; convert back to
    // reference pixels before passing into `Tree::fixed`, which scales once.
    let native_w = text.measure(label, &style).ceil() as i32;
    let ref_w = (native_w as f32 * 300.0 / s.density.dpi() as f32).ceil() as i32;
    let words = t.label(label, style);
    let chevron = t.icon(Icon::ChevronRight, 28, PAPER);
    let g = t.gap(12, 1);
    let row = t.add(Node::row(0).cross(Cross::Center), [words, g, chevron]);
    let row = t.align(0.5, 0.5, row);
    let pad_x = if compact { 24 } else { 36 };
    let button = t.add(
        Node::new(Kind::Padding(Insets::vh(0, pad_x)))
            .bg(INK)
            .border(0, INK, 20)
            .action(action::RESUME)
            .feedback(Feedback::Invert),
        [row],
    );
    // The words, the gap, the chevron and the padding on both sides.
    let width = ref_w + 12 + 28 + 2 * pad_x;
    let height = if compact { 76 } else { 88 };
    (t.fixed(Some(width), Some(height), Some(button)), width)
}

/// The row of three statistics: what was read today, the days in a row
/// and the hours of the week.
fn stats_row(t: &mut Tree, ty: &Type, stats: Stats, compact: bool) -> NodeId {
    let today = stat_card(
        t,
        ty,
        tr(Msg::HomeStatToday),
        &format!("{}", stats.today_minutes),
        tr(Msg::HomeStatMinutes),
        compact,
    );
    let streak = stat_card(
        t,
        ty,
        tr(Msg::HomeStatStreak),
        &format!("{}", stats.streak_days),
        tr(Msg::HomeStatDays),
        compact,
    );
    let week = stat_card(
        t,
        ty,
        tr(Msg::HomeStatWeek),
        &format!("{:.1}", stats.week_hours),
        tr(Msg::HomeStatHours),
        compact,
    );
    let row = t.add(
        Node::row(if compact { 14 } else { 20 }).cross(Cross::Start),
        [today, streak, week],
    );
    t.fixed(None, None, Some(row))
}

/// One of the three statistics.
fn stat_card(
    t: &mut Tree,
    ty: &Type,
    label: &str,
    number: &str,
    unit: &str,
    compact: bool,
) -> NodeId {
    let label = t.label(label, ty.small.clone().gray(INK_2));
    let number = t.label(number, ty.number.clone());
    let unit = t.label(unit, ty.small.clone());
    let g = t.gap(if compact { 8 } else { 10 }, 1);
    let value = t.add(Node::row(0).cross(Cross::End), [number, g, unit]);
    let col = t.add(Node::column(4).cross(Cross::Start), [label, value]);
    let pad = if compact {
        Insets::vh(14, 18)
    } else {
        Insets::vh(20, 24)
    };
    t.add(
        Node::new(Kind::Padding(pad))
            .bg(CARD_BG)
            .border(0, CARD_BG, if compact { 20 } else { 24 })
            .flex(1),
        [col],
    )
}

/// The nodes of the block of the books opened lately.
pub struct RecentBlock {
    /// The block itself: the heading and the pages of books.
    pub block: NodeId,
    /// The pages of the block.
    pub pages: NodeId,
    /// The cells of the books, in the order the books come.
    pub cells: Vec<NodeId>,
    /// The dots that show which page is up.
    pub dots: Vec<NodeId>,
}

/// The block of the books opened lately: the heading with the way into the
/// whole library, the books three to a page, and the dots that show which
/// page is up.
fn recent_block(t: &mut Tree, s: &Styles, ty: &Type, books: &[Book], compact: bool) -> RecentBlock {
    // The heading, and the way into the whole library.
    let title = t.label(tr(Msg::HomeRecent), ty.section.clone());
    let fill = t.spacer(1);
    let all = t.label(tr(Msg::HomeAllBooks), ty.meta.clone());
    let chevron = t.icon(Icon::ChevronRight, 28, INK);
    let g = t.gap(8, 1);
    let all = t.add(Node::row(0).cross(Cross::Center), [all, g, chevron]);
    // The action node fills the heading, so the visible target is the full
    // 64 px while the words and chevron stay at their old place (QA-101 P2-1).
    let all = t.add(
        Node::new(Kind::MinSize(Size::new(0, 64)))
            .action(action::ALL_BOOKS)
            .feedback(Feedback::Invert),
        [all],
    );
    let heading = t.add(Node::row(0).cross(Cross::Center), [title, fill, all]);
    // The link is 64 px tall, so the heading is too.
    let heading = t.fixed(None, Some(64), Some(heading));

    // The books, three to a page.
    let recent_cover = if compact { (108, 144) } else { RECENT_COVER };
    let row_h = recent_cover.1;
    let mut cells = Vec::new();
    let mut rows = Vec::new();
    for page in books.chunks(RECENT_PER_PAGE) {
        let mut row = Vec::new();
        for b in page {
            let i = cells.len();
            let cell = recent_cell(t, s, ty, b, i, recent_cover, compact);
            cells.push(cell);
            row.push(cell);
        }
        // A short page keeps its places: the empty cells take the room of
        // the books that are not there.
        for _ in page.len()..RECENT_PER_PAGE {
            row.push(t.spacer(1));
        }
        let row = t.add(
            Node::row(if compact { 18 } else { 32 }).cross(Cross::Start),
            row,
        );
        rows.push(t.fixed(None, Some(row_h), Some(row)));
    }
    let pages_node = t.add(crate::tree::pages(0), rows.clone());
    // One row to a page: the block is as tall as a row of books.
    let books_row = t.fixed(None, Some(row_h), Some(pages_node));

    // The dots, one for every page.
    let dots: Vec<NodeId> = (0..rows.len())
        .map(|i| {
            t.add(
                Node::new(Kind::Fixed {
                    w: Some(DOT),
                    h: Some(DOT),
                })
                .bg(if i == 0 { INK } else { RULE }),
                [],
            )
        })
        .collect();

    // The heading stands above the books.
    let gap = t.gap(0, if compact { 10 } else { 24 });
    let mut children = vec![heading, gap, books_row];
    // The dots show which page is up, but only when there is more than
    // one: a single page has nothing to point at.
    if dots.len() > 1 {
        let gap = t.gap(0, if compact { 10 } else { 16 });
        let row = t.add(Node::row(12), dots.clone());
        let row = t.add(Node::column(0).cross(Cross::Center), [row]);
        children.push(gap);
        children.push(row);
    }
    let block = t.add(Node::column(0), children);

    RecentBlock {
        block,
        pages: pages_node,
        cells,
        dots,
    }
}

/// One book of the recent block: its cover, its title, and how far it has
/// come.
fn recent_cell(
    t: &mut Tree,
    s: &Styles,
    ty: &Type,
    b: &Book,
    i: usize,
    cover_size: (i32, i32),
    compact: bool,
) -> NodeId {
    let cover = Cover::new(&b.title, &b.author, s.family)
        .with_mark(b.mark())
        .with_source_tag(b.source.label())
        .with_image(b.cover.clone());
    let cover = t.custom(cover);
    let cover = t.fixed(Some(cover_size.0), Some(cover_size.1), Some(cover));

    let mut tp = Paragraph::plain(&b.title, ty.recent.clone(), Align::Left);
    tp.style.max_lines = Some(1);
    let title = t.text(tp);
    let pct = (b.progress * 100.0).floor();
    let bar = t.custom(Bar {
        value: b.progress,
        height: 10,
        track: CARD_BG,
    });
    let bar = t.add(Node::column(0).flex(1), [bar]);
    let pct = t.label(&format!("{pct:.0}%"), ty.small.clone());
    let g = t.gap(if compact { 8 } else { 12 }, 1);
    let line = t.add(Node::row(0).cross(Cross::Center), [bar, g, pct]);

    let info = t.add(
        Node::column(if compact { 8 } else { 10 }).cross(Cross::Start),
        [title, line],
    );
    let cell = t.add(
        Node::row(if compact { 12 } else { 20 }).cross(Cross::Center),
        [cover, info],
    );
    t.add(
        Node::new(Kind::Padding(Insets::all(0)))
            .action(action::RECENT + i as ActionId)
            .feedback(Feedback::Invert)
            .flex(1),
        [cell],
    )
}

/// The card of the quotation of the day.
fn quote_card(
    t: &mut Tree,
    ty: &Type,
    q: &quotes::QuoteItem,
    compact: bool,
) -> (NodeId, QuoteNodes) {
    let mut qp = Paragraph::plain(format!("「{}」", q.text), ty.quote.clone(), Align::Left);
    if compact {
        qp.style.max_lines = Some(2);
    }
    let text = t.text(qp);

    // The line under the quotation: the author and the book, or the mark
    // of the quotations the app wrote itself. It stands at the right of
    // the card, as the prototype has it.
    let byline_text = if q.is_original {
        String::new()
    } else {
        format!("—— {}{}", q.author, q.source)
    };
    let byline = t.add(
        Node::new(Kind::Text {
            para: Paragraph::plain(&byline_text, ty.small.clone().gray(INK_2), Align::Right),
            cache: None,
        })
        .flex(1),
        [],
    );
    let mark_label = t.label(tr(Msg::HomeQuoteBrand), ty.small.clone().bold().gray(PAPER));
    let mark_box = t.add(
        Node::new(Kind::Padding(Insets::vh(
            if compact { 6 } else { 10 },
            if compact { 14 } else { 18 },
        )))
        .bg(INK)
        .border(0, INK, 16)
        .hidden(!q.is_original),
        [mark_label],
    );
    // The byline takes the room the mark does not, so the words of the
    // mark stand at the very right of the card.
    let byline_row = t.add(Node::row(0).cross(Cross::Center), [byline, mark_box]);
    let words = t.add(
        Node::column(if compact { 6 } else { 8 }).cross(Cross::Start),
        [text, byline_row],
    );
    let words = t.add(Node::column(0).flex(1), [words]);

    // The button that asks for another quotation.
    let btn_side = if compact { 72 } else { 88 };
    let icon = t.icon(Icon::Refresh, if compact { 32 } else { 36 }, INK);
    let icon = t.align(0.5, 0.5, icon);
    let button = t.add(
        Node::new(Kind::Padding(Insets::all(0)))
            .bg(PAPER)
            .border(2, INK, 20)
            .action(action::NEXT_QUOTE)
            .feedback(Feedback::Invert),
        [icon],
    );
    let button = t.fixed(Some(btn_side), Some(btn_side), Some(button));

    let row = t.add(
        Node::row(if compact { 16 } else { 24 }).cross(Cross::Start),
        [words, button],
    );
    let card_pad = if compact {
        Insets::vh(14, 22)
    } else {
        Insets::vh(26, 30)
    };
    let card = t.add(
        Node::new(Kind::Padding(card_pad)).bg(CARD_BG).border(
            0,
            CARD_BG,
            if compact { 20 } else { 24 },
        ),
        [row],
    );
    (
        card,
        QuoteNodes {
            text,
            byline,
            mark: mark_label,
            mark_box,
        },
    )
}

/// The progress bar of the prototype: a light track with a black fill,
/// both rounded.
pub struct Bar {
    /// How far the book has come, 0 to 1.
    pub value: f32,
    /// The height of the bar.
    pub height: i32,
    /// The gray of the track.
    pub track: u8,
}

impl Widget for Bar {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(c.max.w, self.height))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        let radius = self.height as f32 / 2.0;
        p.fill_round_rect(r, radius, self.track);
        let w = (r.w as f32 * self.value.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            p.fill_round_rect(Rect::new(r.x, r.y, w, self.height), radius, INK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Screen;
    use crate::text::{FamilyId, FontData, Fonts, TextEngine};
    use crate::ui::Ui;

    /// The fonts of the test: Literata and the Chinese fixture behind it,
    /// so the Chinese words have real glyphs.
    fn fonts() -> Fonts {
        let mut f = Fonts::new();
        let literata = f
            .add_face(FontData::Static(include_bytes!(
                "../../assets/fonts/Literata-Regular.ttf"
            )))
            .expect("Literata is valid");
        let cjk = f
            .add_face(FontData::Static(include_bytes!(
                "../../assets/test-fonts/CjkFixture-Regular.ttf"
            )))
            .expect("the fixture is valid");
        let latin = f
            .add_family("Literata", [Some(literata), None, None, None])
            .expect("Literata has a regular face");
        let chinese = f
            .add_family("Noto Sans SC", [Some(cjk), None, None, None])
            .expect("the fixture has a regular face");
        f.add_fallback(chinese);
        let _ = latin;
        f
    }

    /// The screen the reference screenshot was taken on: a Kindle Oasis 2.
    const W: u32 = 1264;
    const H: u32 = 1680;
    const DPI: u16 = 300;

    /// The home screen of the reference screenshot: the clock card, the
    /// book being read, the statistics and the quotation.
    fn sample<'a>(books: &'a [Book]) -> Home<'a> {
        Home {
            reading: Some(&books[0]),
            recent: &books[1..],
            status: StatusLine {
                clock: "21:38",
                battery: Some(0.82),
                charging: false,
            },
            clock: ClockStyle::BigLunar,
            clock_text: ClockText {
                time: "21:38".into(),
                date: "11月4日 星期三".into(),
                month_day: "11月4日".into(),
                weekday: "星期三".into(),
                lunar: "丙午年 九月廿六".into(),
                lunar_short: "九月廿六".into(),
                term: Some("立冬前三日".into()),
            },
            stats: Some(Stats {
                today_minutes: 23,
                streak_days: 5,
                week_hours: 3.2,
            }),
            quote: Some(&quotes::QUOTES[0]),
            pet: true,
        }
    }

    fn books() -> Vec<Book> {
        let mut reading = Book::new("红楼梦", "曹雪芹", 0.43);
        reading.chapter = "第七回".into();
        vec![
            reading,
            Book::new("西游记", "吴承恩", 0.68),
            Book::new("呐喊", "鲁迅", 0.15),
            Book::new("傲慢与偏见", "简·奥斯汀", 0.91),
        ]
    }

    fn build(home: &Home) -> (Ui, HomeUi) {
        let mut text = TextEngine::new(fonts());
        let styles = Styles::new(FamilyId(0));
        let profile = Screen::new(W, H, DPI).unwrap();
        let (tree, ids) = HomeUi::build(&mut text, &styles, home, W as i32);
        let mut ui = Ui::with_screen(tree, text, profile);
        ui.layout();
        ids.place_pet(&mut ui.tree);
        (ui, ids)
    }

    /// The reading card draws the book's own cover whenever the model has
    /// one. A flat mid-gray cover is easy to find on the screen: the
    /// generated card is black or white, and antialiased type never covers
    /// 60 000 pixels.
    #[test]
    fn the_reading_card_draws_the_book_cover_it_is_given() {
        let mut books = books();
        books[0].cover = Some(crate::widgets::GrayImage::new(
            200,
            300,
            vec![128; 200 * 300],
        ));
        let home = sample(&books);
        let (mut ui, _) = build(&home);
        ui.render();
        let c = ui.screen();
        let mut gray = 0;
        for y in 0..c.height as i32 {
            for x in 0..c.width as i32 {
                gray += usize::from(c.get(x, y) == 128);
            }
        }
        assert!(
            gray > 50_000,
            "the card did not draw the cover: {gray} pixels of its gray"
        );
    }

    #[test]
    fn a_new_minute_draws_the_clock_again_and_nothing_else() {
        let books = books();
        let home = sample(&books);
        let (mut ui, ids) = build(&home);
        ui.render();

        // The minute turns: the time of the clock card changes.
        ids.set_time(&mut ui.tree, "21:39");
        ui.layout();
        let damage = ui.render();
        assert!(!damage.is_empty(), "the new minute changed nothing");
        for d in &damage {
            // The word of the time, not the whole clock card and certainly
            // not the whole screen.
            assert!(
                d.rect.h < 200 && d.rect.w < 600,
                "the damage is bigger than the time: {:?}",
                d.rect
            );
        }
    }

    #[test]
    fn the_clock_card_stands_at_the_top_of_the_screen() {
        let books = books();
        let home = sample(&books);
        let (ui, ids) = build(&home);
        let clock = ids.clock.expect("the clock card is there");
        let time = ui.tree.rect(clock);
        // The card is 88 px from the side and 28 px below the status bar;
        // its own padding of 40 px holds the time.
        assert_eq!(time.x, 128);
        assert!(time.y >= 144 && time.y <= 150, "the time is at {time:?}");
        // The card ends before the reading card starts.
        assert!(time.bottom() <= 310, "the time is at {time:?}");
    }

    #[test]
    fn the_navigation_bar_stays_at_the_foot_of_the_screen() {
        let books = books();
        let home = sample(&books);
        let (ui, ids) = build(&home);
        let nav = ui.tree.rect(ids.nav);
        assert_eq!(nav.y, 1528);
        assert_eq!(nav.h, 152);
    }

    #[test]
    fn the_reading_card_fills_its_place() {
        let books = books();
        let home = sample(&books);
        let (ui, ids) = build(&home);
        let card = ids.card.expect("the card is there");
        let r = ui.tree.rect(card);
        // The card of the book being read: 432 px tall, the prototype's.
        assert_eq!(r.h, 432);
        assert_eq!(r.x, 88);
        assert_eq!(r.w, 1088);
    }

    #[test]
    fn the_quotation_turns_to_the_next_one() {
        let books = books();
        let home = sample(&books);
        let (mut ui, ids) = build(&home);
        ui.render();
        let next = quotes::next(0);
        ids.set_quote(&mut ui.tree, &quotes::QUOTES[next], "墨阅");
        let q = ids.quote.as_ref().expect("the quotation card is there");
        let Kind::Text { para, .. } = &ui.tree.node(q.text).kind else {
            panic!("the quotation is a text node");
        };
        assert_eq!(para.text(), format!("「{}」", quotes::QUOTES[next].text));
    }

    #[test]
    fn the_quotation_card_is_the_only_thing_drawn_again() {
        let books = books();
        let home = sample(&books);
        let (mut ui, ids) = build(&home);
        ui.render();
        let q = ids.quote.as_ref().expect("the quotation card is there");
        // Latin words, so the test does not need the Chinese font.
        ui.tree.set_text(q.text, "A different quotation altogether");
        ui.layout();
        let damage = ui.render();
        assert!(!damage.is_empty(), "the new quotation changed nothing");
        for d in &damage {
            // The card of the quotation is 158 px tall and stands at the
            // foot of the screen.
            assert!(
                d.rect.y >= 1330 && d.rect.bottom() <= 1500,
                "the damage is outside the quotation card: {:?}",
                d.rect
            );
        }
    }

    #[test]
    fn the_clock_styles_are_four() {
        assert_eq!(ClockStyle::ALL.len(), 4);
        assert_eq!(ClockStyle::default(), ClockStyle::BigLunar);
    }

    #[test]
    fn the_cat_sits_where_the_prototype_puts_it() {
        let books = books();
        let home = sample(&books);
        let (mut ui, ids) = build(&home);
        let id = ids.pet.expect("the pet is there");
        let pet = ui.tree.widget_mut::<pet::Pet>(id).expect("the pet widget");
        assert!(pet.active);
        assert_eq!(pet.geometry.cat, Rect::new(923, 354, 287, 280));
        assert_eq!(pet.geometry.line, Rect::new(1208, 363, 2, 1528 - 363));
        assert_eq!(pet.geometry.fish, Rect::new(1169, 1526, 79, 150));
    }

    #[test]
    fn without_a_book_to_continue_there_is_no_cat() {
        let books = books();
        let mut home = sample(&books);
        home.reading = None;
        let (mut ui, ids) = build(&home);
        let id = ids.pet.expect("the pet is there");
        let pet = ui.tree.widget_mut::<pet::Pet>(id).expect("the pet widget");
        assert!(
            !pet.active,
            "the cat sits on the card of the book being read"
        );
    }
}
