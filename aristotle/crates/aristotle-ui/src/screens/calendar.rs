// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reading calendar: the months of reading, the days, and the books.
//!
//! The screen follows the prototype of `docs/ui/prototype-v2`: the header
//! with a way back, the month with its two arrows, the legend of the four
//! levels, the grid of the days, the total of the month, and the day
//! picked with the books read then.
//!
//! The measurements are the prototype's, in reference pixels: an 88 px
//! margin, a 112 px header, a 128 px day, a 20 px round day and a 24 px
//! round card on the panel gray.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Point, Rect, Size};
use crate::icons::Icon;
use crate::text::{Align, Paragraph, TextEngine, TextStyle};
use crate::theme::{home_margin, Styles, CARD, DISABLED, INK, INK_MID, PAPER};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use crate::widgets::GrayImage;
use aristotle_i18n::{language, locale, tf, tr, Msg};
use zeno::{Command, PathBuilder};

pub mod action {
    use crate::tree::ActionId;
    /// The way back to the "mine" screen.
    pub const BACK: ActionId = 1;
    /// The month before.
    pub const PREV_MONTH: ActionId = 2;
    /// The month after.
    pub const NEXT_MONTH: ActionId = 3;
    /// The first day of the month: the day `d` is `FIRST_DAY + d - 1`.
    pub const FIRST_DAY: ActionId = 10;
    /// A book read on the day picked. The book statistics are not built
    /// yet, so the row opens the page that says so.
    pub const BOOK: ActionId = 50;
}

/// The months of the year, as the title of the screen writes them.
const MONTHS: [Msg; 12] = [
    Msg::CalendarMonth1,
    Msg::CalendarMonth2,
    Msg::CalendarMonth3,
    Msg::CalendarMonth4,
    Msg::CalendarMonth5,
    Msg::CalendarMonth6,
    Msg::CalendarMonth7,
    Msg::CalendarMonth8,
    Msg::CalendarMonth9,
    Msg::CalendarMonth10,
    Msg::CalendarMonth11,
    Msg::CalendarMonth12,
];

/// The heads of the columns of the grid, Monday first.
const WEEKDAYS: [Msg; 7] = [
    Msg::CalendarWeekday1,
    Msg::CalendarWeekday2,
    Msg::CalendarWeekday3,
    Msg::CalendarWeekday4,
    Msg::CalendarWeekday5,
    Msg::CalendarWeekday6,
    Msg::CalendarWeekday7,
];

/// How long a day was read: the four levels of the legend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Nothing was read: an empty ring.
    Empty,
    /// Less than fifteen minutes: a small dot.
    Dot,
    /// From fifteen to forty-five minutes: half a ring.
    Half,
    /// More than forty-five minutes: a full disc.
    Full,
}

/// The level of a day with `minutes` of reading.
pub fn level_of(minutes: u32) -> Level {
    match minutes {
        0 => Level::Empty,
        m if m < 15 => Level::Dot,
        m if m <= 45 => Level::Half,
        _ => Level::Full,
    }
}

/// A day of the grid.
#[derive(Clone, Copy)]
pub struct Day {
    /// The day of the month; 0 for a cell without a date.
    pub day: u8,
    /// The minutes read that day.
    pub minutes: u32,
    /// The day is after today: no mark, and it cannot be picked.
    pub future: bool,
}

/// A book read on the day picked.
pub struct Book {
    /// The ID or hash of the book.
    pub hash: String,
    /// The title of the book.
    pub title: String,
    /// Who wrote it.
    pub author: String,
    /// The minutes read in it that day.
    pub minutes: u32,
    /// The book's own cover, when the library has one.
    pub cover: Option<GrayImage>,
}

/// What the calendar shows.
pub struct Calendar<'a> {
    /// The status line.
    pub status: StatusLine<'a>,
    /// The year of the month on the screen.
    pub year: i32,
    /// The month on the screen, from 1 to 12.
    pub month: u8,
    /// The days of the month, the first one first; the cells before it
    /// have no date.
    pub days: Vec<Day>,
    /// The day picked in the month.
    pub selected: u8,
    /// The weekday of the day picked, Sunday is 0.
    pub weekday: u8,
    /// The minutes read in the month.
    pub month_minutes: u32,
    /// The days of the month with reading.
    pub month_days: u32,
    /// The minutes read on the day picked.
    pub day_minutes: u32,
    /// The books read on the day picked.
    pub books: Vec<Book>,
    /// The month before can be shown.
    pub can_prev: bool,
    /// The month after can be shown.
    pub can_next: bool,
}

/// Handles to the nodes of the calendar screen.
pub struct CalendarUi {
    /// The cells of the grid, the empty ones first.
    pub cells: Vec<NodeId>,
}

/// A sample of the screen, for the preview and the tests: the prototype's
/// November 2026, with the fourth of the month picked.
pub fn sample(status: StatusLine<'static>) -> Calendar<'static> {
    let minutes = [41, 85, 84, 23];
    // November 2026 starts on a Sunday, so six cells come before the first.
    let mut days = vec![
        Day {
            day: 0,
            minutes: 0,
            future: false,
        };
        6
    ];
    for (i, m) in minutes.iter().enumerate() {
        days.push(Day {
            day: i as u8 + 1,
            minutes: *m,
            future: false,
        });
    }
    for day in 5..=30u8 {
        days.push(Day {
            day,
            minutes: 0,
            future: true,
        });
    }
    Calendar {
        status,
        year: 2026,
        month: 11,
        days,
        selected: 4,
        weekday: 3,
        month_minutes: 233,
        month_days: 4,
        day_minutes: 23,
        books: vec![Book {
            hash: "sample".to_string(),
            title: "红楼梦".to_string(),
            author: "曹雪芹".to_string(),
            minutes: 23,
            cover: None,
        }],
        can_prev: true,
        can_next: false,
    }
}

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 112;
/// The height of the row with the month.
const MONTH_H: i32 = 88;
/// The size of the arrows of the month.
const ARROW: i32 = 44;
/// The touch area of the arrows of the month.
const ARROW_TAP: i32 = 88;
/// The size of a mark of the legend and of a day.
const MARK: i32 = 34;
/// The height of a row of the grid of a month of six weeks.
const ROW_H: i32 = 128;
/// The height of a row of the grid of a month of five weeks.
const ROW_H_FEW: i32 = 140;
/// The radius of a day.
const DAY_R: i32 = 20;
/// The radius of a card.
const CARD_R: i32 = 24;
/// The width of the cover of a book.
const COVER_W: i32 = 56;
/// The height of the cover of a book.
const COVER_H: i32 = 76;

impl CalendarUi {
    /// Builds the calendar screen.
    pub fn build(text: &mut TextEngine, s: &Styles, cal: &Calendar) -> (Tree, Self) {
        Self::build_for_size(
            text,
            s,
            cal,
            Size::new(s.density.px(1264), s.density.px(1680)),
        )
    }

    /// Builds the calendar screen for a panel of `size` native pixels.
    /// Six-inch panels tighten grid row height and gaps so the month summary
    /// and selected-day detail card stay fully visible.
    pub fn build_for_size(
        _text: &mut TextEngine,
        s: &Styles,
        cal: &Calendar,
        size: Size,
    ) -> (Tree, Self) {
        let compact = crate::theme::PanelLayout::for_size(size, s.density)
            == crate::theme::PanelLayout::Compact;
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &cal.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let header = header(&mut t, s, compact);
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
        let mut cells = Vec::new();
        let body = body(&mut t, s, cal, &mut cells, compact);
        let root = t.add(Node::column(0).bg(PAPER), [status, header, body]);
        t.set_root(root);
        (t, CalendarUi { cells })
    }
}

/// The header: the way back and the name of the screen.
fn header(t: &mut Tree, s: &Styles, compact: bool) -> NodeId {
    let glyph = t.icon(Icon::ChevronLeft, ARROW, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(
        tr(Msg::CalendarTitle),
        s.font_lg().bold().line_height(1.2).tracking(4.0),
    );
    let row = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    t.fixed(None, Some(if compact { 88 } else { HEADER_H }), Some(row))
}

/// Everything under the header.
fn body(
    t: &mut Tree,
    s: &Styles,
    cal: &Calendar,
    cells: &mut Vec<NodeId>,
    compact: bool,
) -> NodeId {
    let mut parts = vec![
        month_row(t, s, cal, compact),
        t.gap(0, if compact { 6 } else { 12 }),
        legend(t, s),
        t.gap(0, if compact { 10 } else { 20 }),
        week_row(t, s),
        t.gap(0, if compact { 6 } else { 10 }),
    ];
    parts.push(grid(t, s, cal, cells, compact));
    parts.push(t.gap(0, if compact { 12 } else { 28 }));
    parts.push(month_card(t, s, cal, compact));
    parts.push(t.gap(0, if compact { 10 } else { 24 }));
    parts.push(day_card(t, s, cal, compact));
    let col = t.add(Node::column(0), parts);
    let pad = if compact {
        Insets::new(10, home_margin(), 14, home_margin())
    } else {
        Insets::new(24, home_margin(), 32, home_margin())
    };
    t.pad(pad, col)
}

/// The row with the month and the two arrows that change it.
fn month_row(t: &mut Tree, s: &Styles, cal: &Calendar, compact: bool) -> NodeId {
    let prev = arrow(t, Icon::ChevronLeft, cal.can_prev, action::PREV_MONTH);
    let next = arrow(t, Icon::ChevronRight, cal.can_next, action::NEXT_MONTH);
    let title = t.label(
        &tf!(
            Msg::CalendarMonthTitle,
            month = title_month(cal.month),
            year = cal.year
        ),
        s.font_px(44).bold().line_height(1.2).tracking(2.0),
    );
    let fill_left = t.spacer(1);
    let fill_right = t.spacer(1);
    let row = t.add(
        Node::row(0).cross(Cross::Center),
        [prev, fill_left, title, fill_right, next],
    );
    t.fixed(None, Some(if compact { 72 } else { MONTH_H }), Some(row))
}

/// One of the two arrows of the month. A month that cannot be shown leaves
/// the arrow gray.
fn arrow(t: &mut Tree, icon: Icon, can: bool, id: ActionId) -> NodeId {
    let gray = if can { INK } else { DISABLED };
    let glyph = t.icon(icon, ARROW, gray);
    let mut node =
        Node::new(Kind::MinSize(Size::new(ARROW_TAP, ARROW_TAP))).feedback(Feedback::Invert);
    if can {
        node = node.action(id);
    }
    t.add(node, [glyph])
}

/// The legend: the four levels and what they mean.
fn legend(t: &mut Tree, s: &Styles) -> NodeId {
    let items = [
        (Level::Empty, Msg::CalendarLegendNone),
        (Level::Dot, Msg::CalendarLegend1),
        (Level::Half, Msg::CalendarLegend2),
        (Level::Full, Msg::CalendarLegend3),
    ];
    let mut parts = Vec::new();
    for (level, msg) in items {
        let mark = t.custom(LevelMark { level, gray: INK });
        let label = t.label(tr(msg), s.font_xs().gray(INK_MID).line_height(1.2));
        parts.push(t.add(Node::row(10).cross(Cross::Center), [mark, label]));
    }
    t.add(Node::row(24).cross(Cross::Center), parts)
}

/// The heads of the columns of the grid.
fn week_row(t: &mut Tree, s: &Styles) -> NodeId {
    let mut parts = Vec::new();
    for msg in WEEKDAYS {
        let label = t.label(tr(msg), s.font_xs().gray(INK_MID).line_height(1.2));
        parts.push(t.add(Node::column(0).flex(1).cross(Cross::Center), [label]));
    }
    t.add(Node::row(10), parts)
}

/// The grid of the days of the month.
fn grid(
    t: &mut Tree,
    s: &Styles,
    cal: &Calendar,
    cells: &mut Vec<NodeId>,
    compact: bool,
) -> NodeId {
    let count = cal.days.len().div_ceil(7) * 7;
    let rows = count / 7;
    let row_h = match (compact, rows > 5) {
        (true, true) => 90,
        (true, false) => 104,
        (false, true) => ROW_H,
        (false, false) => ROW_H_FEW,
    };
    let gap = if compact { 6 } else { 10 };
    let mut parts = Vec::new();
    for row in 0..rows {
        let mut line = Vec::new();
        for col in 0..7 {
            let index = row * 7 + col;
            let day = cal.days.get(index).copied();
            let cell = cell(t, s, day, cal.selected, row_h, compact);
            cells.push(cell);
            line.push(cell);
        }
        parts.push(t.add(Node::row(gap), line));
    }
    t.add(Node::column(gap), parts)
}

/// One day of the grid: the number of the day and the mark of its level.
fn cell(
    t: &mut Tree,
    s: &Styles,
    day: Option<Day>,
    selected: u8,
    row_h: i32,
    compact: bool,
) -> NodeId {
    let Some(day) = day.filter(|d| d.day > 0) else {
        // A cell without a date: nothing to show.
        return t.add(
            Node::new(Kind::Fixed {
                w: None,
                h: Some(row_h),
            })
            .flex(1),
            [],
        );
    };
    let on = day.day == selected && !day.future;
    let (bg, fg) = if on { (INK, PAPER) } else { (CARD, INK) };
    let number = t.label(
        &day.day.to_string(),
        s.font_px(34).gray(fg).line_height(1.0),
    );
    let mut parts = vec![number];
    if !day.future {
        parts.push(t.custom(LevelMark {
            level: level_of(day.minutes),
            gray: fg,
        }));
    }
    let col = t.add(
        Node::column(if compact { 6 } else { 16 }).cross(Cross::Center),
        parts,
    );
    let col = t.align(0.5, 0.5, col);
    let mut node = Node::new(Kind::Fixed {
        w: None,
        h: Some(row_h),
    })
    .flex(1)
    .bg(bg)
    .border(0, bg, DAY_R);
    if !day.future {
        node = node
            .action(action::FIRST_DAY + ActionId::from(day.day) - 1)
            .feedback(Feedback::Invert);
    }
    t.add(node, [col])
}

/// The card with the total of the month.
fn month_card(t: &mut Tree, s: &Styles, cal: &Calendar, compact: bool) -> NodeId {
    let hours = cal.month_minutes / 60;
    let minutes = cal.month_minutes % 60;
    let total_label = t.label(
        tr(Msg::CalendarMonthTotal),
        s.font_xs().gray(INK_MID).line_height(1.2),
    );
    let total = figure(
        t,
        s,
        &[
            (hours.to_string(), true),
            (tr(Msg::CalendarHours).to_string(), false),
            (minutes.to_string(), true),
            (tr(Msg::CalendarMinutes).to_string(), false),
        ],
    );
    let left = t.add(Node::column(4).cross(Cross::Start), [total_label, total]);
    let days_label = t.label(
        tr(Msg::CalendarActiveDays),
        s.font_xs().gray(INK_MID).line_height(1.2),
    );
    let days = figure(
        t,
        s,
        &[
            (cal.month_days.to_string(), true),
            (tr(Msg::CalendarDays).to_string(), false),
        ],
    );
    let right = t.add(Node::column(4).cross(Cross::End), [days_label, days]);
    let fill = t.spacer(1);
    let row = t.add(Node::row(24).cross(Cross::Center), [left, fill, right]);
    let pad = if compact {
        Insets::new(14, 24, 14, 24)
    } else {
        Insets::new(22, 28, 22, 28)
    };
    t.add(
        Node::new(Kind::Padding(pad))
            .bg(CARD)
            .border(0, CARD, CARD_R),
        [row],
    )
}

/// A number and its unit, the number in the display size.
fn figure(t: &mut Tree, s: &Styles, parts: &[(String, bool)]) -> NodeId {
    let mut nodes = Vec::new();
    for (text, big) in parts {
        let style = if *big {
            s.font_md().bold().line_height(1.1)
        } else {
            s.font_xs().line_height(1.2)
        };
        nodes.push(t.label(text, style));
    }
    t.add(Node::row(8).cross(Cross::End), nodes)
}

/// The card with the day picked: its date, its total, and the books.
fn day_card(t: &mut Tree, s: &Styles, cal: &Calendar, compact: bool) -> NodeId {
    let title = t.label(
        &tf!(
            Msg::CalendarDayTitle,
            month = day_month(cal.month),
            day = cal.selected,
            weekday = locale().weekdays[(cal.weekday % 7) as usize]
        ),
        s.font_px(36).bold().line_height(1.25),
    );
    let total = if cal.day_minutes > 0 {
        tf!(Msg::CalendarDayTotal, minutes = cal.day_minutes)
    } else {
        tr(Msg::CalendarNotRead).to_string()
    };
    let total = t.label(&total, s.font_xs().gray(INK_MID).line_height(1.2));
    let fill = t.spacer(1);
    let head = t.add(Node::row(20).cross(Cross::Center), [title, fill, total]);
    let mut parts = vec![head, t.gap(0, if compact { 8 } else { 14 })];
    if cal.books.is_empty() {
        let empty = t.label(
            tr(Msg::CalendarEmptyDay),
            s.font_xs().gray(INK_MID).line_height(1.3),
        );
        parts.push(t.add(Node::row(0).cross(Cross::Center), [empty]));
    } else {
        for (i, book) in cal.books.iter().enumerate() {
            parts.push(book_row(t, s, book, i));
        }
    }
    let col = t.add(Node::column(0), parts);
    let pad = if compact {
        Insets::new(14, 24, 10, 24)
    } else {
        Insets::new(22, 28, 10, 28)
    };
    t.add(
        Node::new(Kind::Padding(pad))
            .bg(CARD)
            .border(0, CARD, CARD_R),
        [col],
    )
}

/// One book of the day picked: the cover, the title, the author, and the
/// minutes read in it.
fn book_row(t: &mut Tree, s: &Styles, book: &Book, index: usize) -> NodeId {
    // The book's own cover when there is one; the prototype's plain shape
    // otherwise, so the row still looks like a book.
    let cover = match &book.cover {
        Some(image) => {
            let node = t.custom(
                crate::widgets::Cover::new(&book.title, &book.author, s.family)
                    .with_image(Some(image.clone())),
            );
            t.fixed(Some(COVER_W), Some(COVER_H), Some(node))
        }
        None => t.custom(Cover { skin: index % 3 }),
    };
    let title = one_line(t, &book.title, s.font_px(36).bold().line_height(1.25));
    let author = one_line(t, &book.author, s.font_xs().gray(INK_MID).line_height(1.2));
    let texts = t.add(Node::column(4).flex(1), [title, author]);
    let minutes = t.label(&book.minutes.to_string(), s.font_px(36).line_height(1.0));
    let unit = t.label(tr(Msg::CalendarMinutes), s.font_xs().line_height(1.2));
    let time = t.add(Node::row(6).cross(Cross::End), [minutes, unit]);
    let row = t.add(
        Node::row(20)
            .cross(Cross::Center)
            .action(action::BOOK + index as ActionId),
        [cover, texts, time],
    );
    t.fixed(None, Some(88), Some(row))
}

/// A line of text that does not wrap.
fn one_line(t: &mut Tree, text: &str, style: TextStyle) -> NodeId {
    let mut p = Paragraph::plain(text, style, Align::Left);
    p.style.max_lines = Some(1);
    t.text(p)
}

/// The month as the title of the screen writes it: a number in Chinese
/// ("2026 年 11 月"), a name in the other languages ("November 2026").
fn title_month(month: u8) -> String {
    tr(MONTHS[(month.clamp(1, 12) - 1) as usize]).to_string()
}

/// The month as the day card writes it: the name in English ("November"),
/// the number in Chinese ("11 月") and in Polish ("4.11").
fn day_month(month: u8) -> String {
    if language() == "en" {
        title_month(month)
    } else {
        month.to_string()
    }
}

/// The mark of a level: an empty ring, a dot, half a ring, or a full disc.
/// The prototype draws it on a 32-unit grid; the mark keeps that grid, so
/// the ring is 24 units wide and the stroke 3.
pub struct LevelMark {
    /// The level the mark stands for.
    pub level: Level,
    /// The ink of the mark.
    pub gray: u8,
}

impl Widget for LevelMark {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let d = text.density();
        c.constrain(Size::new(d.px(MARK), d.px(MARK)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let unit = (r.w.min(r.h) as f32) / 32.0;
        let center = Point {
            x: r.x + r.w / 2,
            y: r.y + r.h / 2,
        };
        let radius = 12.0 * unit;
        let stroke = 3.0 * unit;
        match self.level {
            Level::Empty => ring(cx, center, radius, stroke, self.gray),
            Level::Dot => cx.painter.fill_circle(center, 6.0 * unit, self.gray),
            Level::Half => {
                ring(cx, center, radius, stroke, self.gray);
                let top = center.y as f32 - radius;
                let bottom = center.y as f32 + radius;
                let d = format!(
                    "M{} {} A{} {} 0 0 1 {} {} Z",
                    center.x, top, radius, radius, center.x, bottom
                );
                cx.painter.fill_path(d.as_str(), None, self.gray);
            }
            Level::Full => cx.painter.fill_circle(center, radius, self.gray),
        }
    }
}

/// The empty ring of a level.
fn ring(cx: &mut PaintCx, center: Point, radius: f32, width: f32, gray: u8) {
    let mut path: Vec<Command> = Vec::new();
    path.add_circle([center.x as f32, center.y as f32], radius);
    cx.painter.stroke_path(&path, width, None, gray);
}

/// The cover of a book: a plain shape, as the prototype draws it. The three
/// skins keep the row readable without a thumbnail.
pub struct Cover {
    /// The skin of the cover: 0 black, 1 dark gray, 2 white with a border.
    pub skin: usize,
}

impl Widget for Cover {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let d = text.density();
        c.constrain(Size::new(d.px(COVER_W), d.px(COVER_H)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let d = cx.text.density();
        let radius = d.scale(10.0);
        let (bg, border) = match self.skin {
            0 => (INK, None),
            1 => (INK_MID, None),
            _ => (PAPER, Some(INK)),
        };
        let painter = &mut *cx.painter;
        painter.fill_round_rect(r, radius, bg);
        if let Some(color) = border {
            painter.stroke_round_rect(r, radius, d.scale(2.0), color);
        }
        // The white band on the cover.
        let inset = d.px(10);
        let band = Rect::new(r.x + inset, r.y + inset, d.px(24), d.px(56));
        painter.fill_round_rect(band, d.scale(4.0), PAPER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Fonts, TextEngine};
    use crate::theme::Styles;

    fn calendar() -> Calendar<'static> {
        sample(StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        })
    }

    fn build() -> (Tree, TextEngine) {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (tree, _view) = CalendarUi::build(&mut text, &s, &calendar());
        (tree, text)
    }

    /// The screen of the prototype: a Kindle Oasis 2 at 300 DPI.
    fn oasis() -> aristotle_screen::Screen {
        aristotle_screen::Screen::new(1264, 1680, 300).unwrap()
    }

    /// A book read on the day shows its own cover in the row: the flat gray
    /// of a stand-in cover is nowhere on the prototype's decorative spine.
    #[test]
    fn a_book_with_a_cover_shows_it_in_its_row() {
        let mut cal = calendar();
        cal.books[0].cover = Some(crate::widgets::GrayImage::new(50, 76, vec![128; 50 * 76]));
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (tree, _) = CalendarUi::build(&mut text, &s, &cal);
        let mut ui = crate::ui::Ui::with_screen(tree, text, oasis());
        ui.layout();
        ui.render();
        let c = ui.screen();
        let mut gray = 0;
        for y in 0..c.height as i32 {
            for x in 0..c.width as i32 {
                gray += usize::from(c.get(x, y) == 128);
            }
        }
        assert!(gray > 2_000, "the row did not draw the cover: {gray}");
    }

    #[test]
    fn the_levels_follow_the_minutes() {
        assert_eq!(level_of(0), Level::Empty);
        assert_eq!(level_of(1), Level::Dot);
        assert_eq!(level_of(14), Level::Dot);
        assert_eq!(level_of(15), Level::Half);
        assert_eq!(level_of(45), Level::Half);
        assert_eq!(level_of(46), Level::Full);
    }

    #[test]
    fn the_grid_holds_six_rows_of_seven_days() {
        let cal = calendar();
        let count = cal.days.len().div_ceil(7) * 7;
        assert_eq!(count, 42);
        assert_eq!(count / 7, 6);
    }

    #[test]
    fn the_screen_lays_out_the_prototype() {
        let (mut tree, mut text) = build();
        let screen = oasis();
        tree.layout(
            Rect::new(0, 0, screen.width as i32, screen.height as i32),
            &mut text,
        );
        let root = tree.root().unwrap();
        // The status line, the header and the body.
        assert_eq!(tree.node(root).children.len(), 3);
        // The header is 112 px tall.
        assert_eq!(tree.rect(tree.node(root).children[1]).h, HEADER_H);
    }

    #[test]
    fn the_grid_has_a_cell_for_every_day() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (_tree, view) = CalendarUi::build(&mut text, &s, &calendar());
        assert_eq!(view.cells.len(), 42);
    }
}
