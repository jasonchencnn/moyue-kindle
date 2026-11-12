// SPDX-License-Identifier: AGPL-3.0-or-later
//! Single book reading statistics: cover card, 2x3 grid, predicted
//! completion, and the 7-day duration chart.
//!
//! The screen follows the prototype in `docs/ui/prototype-v2/mcode/BookStats.dc.html`:
//! - Header: "‹ Title", returns to reading calendar. No bottom bar.
//! - Cover card: Cover widget, status chip ("在读"), meta ("作者 · 格式 · 当前章节"),
//!   page progress ("已读 X 页 · 共 Y 页"), progress bar + percentage.
//! - 2x3 grid: 累计时长, 阅读天数, 开始日期, 平均每页耗时, 已读页数, 划线与笔记.
//! - Prediction card: Black pill, "预计读完 M 月 D 日 · 还有 N 天".
//! - Bar chart: "近 7 天阅读时长", 7 white tracks with black bars, minutes on top,
//!   dates on bottom, 7-day sum on right.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Rect, Size};
use crate::icons::Icon;
use crate::text::TextEngine;
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER};
use crate::tree::{Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget};
use crate::widgets::{Cover, GrayImage};
use aristotle_i18n::{language, locale, tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// Back to the caller (reading calendar).
    pub const BACK: ActionId = 1;
    /// Open the book in the reader.
    pub const OPEN: ActionId = 2;
}

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 112;
/// The size of the back arrow.
const ARROW: i32 = 44;

/// The reading status of a book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookStatus {
    Unread,
    Reading,
    Finished,
}

/// Estimated completion of a book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prediction {
    Estimated { month: u8, day: u8, days_left: u32 },
    Finished,
    None,
}

/// One day of the 7-day reading duration chart.
#[derive(Clone, Debug)]
pub struct DayBar {
    pub minutes: u32,
    pub label: String,
}

/// What the single book reading statistics screen shows.
pub struct BookStats<'a> {
    pub status: StatusLine<'a>,
    pub title: String,
    pub author: String,
    pub format: String,
    pub chapter: Option<String>,
    pub status_chip: BookStatus,
    pub read_pages: u32,
    pub total_pages: Option<u32>,
    pub progress_percent: u32,
    pub cover_image: Option<GrayImage>,
    pub total_hours: u32,
    pub total_minutes: u32,
    pub read_days: u32,
    pub start_date: Option<(u8, u8)>,
    pub avg_time_per_page: Option<(u32, u32)>,
    pub highlights_count: u32,
    pub notes_count: u32,
    pub prediction: Prediction,
    pub bars: Vec<DayBar>,
    pub week_total_minutes: u32,
}

/// Handles to the nodes of the book statistics screen.
pub struct BookStatsUi;

/// Sample data from the prototype (BookStats.dc.html).
pub fn sample(status: StatusLine<'static>) -> BookStats<'static> {
    BookStats {
        status,
        title: "红楼梦".to_string(),
        author: "曹雪芹".to_string(),
        format: "EPUB".to_string(),
        chapter: Some("第七回".to_string()),
        status_chip: BookStatus::Reading,
        read_pages: 450,
        total_pages: Some(1046),
        progress_percent: 43,
        cover_image: None,
        total_hours: 38,
        total_minutes: 20,
        read_days: 96,
        start_date: Some((3, 12)),
        avg_time_per_page: Some((5, 7)),
        highlights_count: 12,
        notes_count: 3,
        prediction: Prediction::Estimated {
            month: 12,
            day: 18,
            days_left: 44,
        },
        bars: vec![
            DayBar {
                minutes: 16,
                label: "10/29".to_string(),
            },
            DayBar {
                minutes: 0,
                label: "10/30".to_string(),
            },
            DayBar {
                minutes: 14,
                label: "10/31".to_string(),
            },
            DayBar {
                minutes: 41,
                label: "11/1".to_string(),
            },
            DayBar {
                minutes: 85,
                label: "11/2".to_string(),
            },
            DayBar {
                minutes: 84,
                label: "11/3".to_string(),
            },
            DayBar {
                minutes: 23,
                label: "11/4".to_string(),
            },
        ],
        week_total_minutes: 263,
    }
}

/// A thin horizontal progress bar with rounded ends.
pub struct FractionBar {
    pub fraction: f32,
}

impl Widget for FractionBar {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        Size::new(c.max.w, 12)
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        p.fill_round_rect(r, 6.0, PAPER);
        let w = (r.w as f32 * self.fraction.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            let fill_r = Rect::new(r.x, r.y, w, r.h);
            let radius = (w as f32 / 2.0).min(6.0);
            p.fill_round_rect(fill_r, radius, INK);
        }
    }
}

/// A vertical white bar track with bottom-aligned black fill.
pub struct BarTrack {
    pub ratio: f32,
}

impl Widget for BarTrack {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        let w = c.max.w.min(76);
        Size::new(w, c.max.h)
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        let radius = 12.0;
        p.fill_round_rect(r, radius, PAPER);
        let h = (r.h as f32 * self.ratio.clamp(0.0, 1.0)).round() as i32;
        if h > 0 {
            let fill_r = Rect::new(r.x, r.bottom() - h, r.w, h);
            let fill_radius = (h as f32 / 2.0).min(radius);
            p.fill_round_rect(fill_r, fill_radius, INK);
        }
    }
}

impl BookStatsUi {
    /// Builds the book statistics screen tree.
    pub fn build(_text: &mut TextEngine, s: &Styles, stats: &BookStats) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &stats.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let header = header(&mut t, s, &stats.title);
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
        let body = body(&mut t, s, stats);
        let root = t.add(Node::column(0).bg(PAPER), [status, header, body]);
        t.set_root(root);
        (t, BookStatsUi)
    }
}

/// The header: back chevron button and book title.
fn header(t: &mut Tree, s: &Styles, title_text: &str) -> NodeId {
    let glyph = t.icon(Icon::ChevronLeft, ARROW, INK);
    let back = t.add(
        Node::new(Kind::MinSize(Size::new(72, 72)))
            .action(action::BACK)
            .feedback(Feedback::Invert),
        [glyph],
    );
    let title = t.label(
        title_text,
        s.font_lg().bold().line_height(1.2).tracking(4.0),
    );
    let row = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    t.fixed(None, Some(HEADER_H), Some(row))
}

/// The body containing all 4 sections.
fn body(t: &mut Tree, s: &Styles, stats: &BookStats) -> NodeId {
    let cover_card = cover_card(t, s, stats);
    let grid = grid_section(t, s, stats);
    let prediction = prediction_card(t, s, stats);
    let chart = chart_card(t, s, stats);

    let parts = vec![
        cover_card,
        t.gap(0, 28),
        grid,
        t.gap(0, 20),
        prediction,
        t.gap(0, 28),
        chart,
    ];
    let col = t.add(Node::column(0), parts);
    t.pad(Insets::new(24, home_margin(), 32, home_margin()), col)
}

/// Section 1: Cover card (Cover widget, status chip, metadata, progress bar).
fn cover_card(t: &mut Tree, s: &Styles, stats: &BookStats) -> NodeId {
    let cover =
        Cover::new(&stats.title, &stats.author, s.family).with_image(stats.cover_image.clone());
    let cover_node = t.custom(cover);
    let cover_btn = t.add(
        Node::new(Kind::Fixed {
            w: Some(208),
            h: Some(300),
        })
        .action(action::OPEN)
        .feedback(Feedback::Invert),
        [cover_node],
    );

    let chip_str = match stats.status_chip {
        BookStatus::Reading => tr(Msg::BookStatsStatusReading),
        BookStatus::Finished => tr(Msg::BookStatsStatusFinished),
        BookStatus::Unread => tr(Msg::BookStatsStatusUnread),
    };
    let chip_text = t.label(chip_str, s.font_px(32).line_height(1.0));
    let chip_pill = t.add(
        Node::new(Kind::Padding(Insets::new(10, 18, 10, 18)))
            .bg(PAPER)
            .border(2, INK, 16),
        [chip_text],
    );
    let chip_row = t.add(Node::row(0), [chip_pill]);

    let mut meta_parts = Vec::new();
    if !stats.author.is_empty() {
        meta_parts.push(stats.author.as_str());
    }
    if !stats.format.is_empty() {
        meta_parts.push(stats.format.as_str());
    }
    if let Some(ref ch) = stats.chapter {
        if !ch.is_empty() {
            meta_parts.push(ch.as_str());
        }
    }
    let meta1_str = meta_parts.join(" · ");
    let meta1 = t.label(&meta1_str, s.font_px(36).gray(INK_MID).line_height(1.4));

    let meta2_str = if let Some(total) = stats.total_pages {
        tf!(
            Msg::BookStatsPagesProgress,
            read = stats.read_pages,
            total = total
        )
    } else {
        tf!(Msg::BookStatsPagesReadOnly, read = stats.read_pages)
    };
    let meta2 = t.label(&meta2_str, s.font_px(34).gray(INK_MID).line_height(1.4));

    let g1 = t.gap(0, 18);
    let g2 = t.gap(0, 4);
    let meta_group = t.add(
        Node::column(0).cross(Cross::Start),
        [chip_row, g1, meta1, g2, meta2],
    );

    let progress_bar = t.custom(FractionBar {
        fraction: stats.progress_percent as f32 / 100.0,
    });
    let progress_bar = t.add(Node::column(0).flex(1), [progress_bar]);
    let percent_str = format!("{}%", stats.progress_percent);
    let percent_lbl = t.label(&percent_str, s.font_px(40).bold().line_height(1.0));
    let progress_row = t.add(
        Node::row(24).cross(Cross::Center),
        [progress_bar, percent_lbl],
    );

    let spacer = t.spacer(1);
    let right_col = t.add(
        Node::column(0).cross(Cross::Stretch).flex(1),
        [meta_group, spacer, progress_row],
    );

    let card_row = t.add(Node::row(32).cross(Cross::Stretch), [cover_btn, right_col]);
    let card_box = t.fixed(None, Some(300), Some(card_row));
    t.add(
        Node::new(Kind::Padding(Insets::new(32, 32, 32, 32)))
            .bg(CARD)
            .border(0, CARD, 32),
        [card_box],
    )
}

/// Section 2: 2x3 Grid.
fn grid_section(t: &mut Tree, s: &Styles, stats: &BookStats) -> NodeId {
    let cell_total_time = {
        let big = stats.total_hours.to_string();
        let unit = if stats.total_hours > 0 {
            tf!(Msg::BookStatsHoursMinsUnit, minutes = stats.total_minutes)
        } else {
            tf!(Msg::BookStatsMinsOnlyUnit)
        };
        stat_cell_numeric(t, s, tr(Msg::BookStatsTotalTime), &big, &unit)
    };

    let cell_reading_days = {
        let big = stats.read_days.to_string();
        let unit = tr(Msg::BookStatsDaysUnit);
        stat_cell_numeric(t, s, tr(Msg::BookStatsReadingDays), &big, unit)
    };

    let cell_start_date = {
        let big = if let Some((m, d)) = stats.start_date {
            format_month_day(m, d)
        } else {
            "—".to_string()
        };
        stat_cell_text(t, s, tr(Msg::BookStatsStartDate), &big)
    };

    let cell_time_per_page = {
        let big = if let Some((mins, secs)) = stats.avg_time_per_page {
            if mins > 0 {
                tf!(Msg::BookStatsTimePerPageVal, minutes = mins, seconds = secs)
            } else {
                tf!(Msg::BookStatsTimePerPageSec, seconds = secs)
            }
        } else {
            "—".to_string()
        };
        stat_cell_text(t, s, tr(Msg::BookStatsTimePerPage), &big)
    };

    let cell_pages_read = {
        let big = stats.read_pages.to_string();
        let unit = tf!(
            Msg::BookStatsPagesReadUnit,
            percent = stats.progress_percent
        );
        stat_cell_numeric(t, s, tr(Msg::BookStatsPagesRead), &big, &unit)
    };

    let cell_marks = {
        let big = stats.highlights_count.to_string();
        let unit = tf!(Msg::BookStatsHighlightsNotesUnit, notes = stats.notes_count);
        stat_cell_numeric(t, s, tr(Msg::BookStatsHighlightsNotes), &big, &unit)
    };

    let row1 = t.add(Node::row(20), [cell_total_time, cell_reading_days]);
    let row2 = t.add(Node::row(20), [cell_start_date, cell_time_per_page]);
    let row3 = t.add(Node::row(20), [cell_pages_read, cell_marks]);

    t.add(Node::column(20), [row1, row2, row3])
}

fn stat_cell_numeric(t: &mut Tree, s: &Styles, label: &str, big: &str, unit: &str) -> NodeId {
    let lbl = t.label(label, s.font_px(32).gray(INK_MID).line_height(1.2));
    let big_node = t.label(big, s.font_px(48).bold().line_height(1.1));
    let unit_node = t.label(unit, s.font_px(32).line_height(1.2));
    let val_row = t.add(Node::row(10).cross(Cross::End), [big_node, unit_node]);
    let spacer = t.spacer(1);
    let col = t.add(Node::column(0).cross(Cross::Start), [lbl, spacer, val_row]);
    let pad = t.pad(Insets::new(20, 24, 20, 24), col);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(136),
        })
        .flex(1)
        .bg(CARD)
        .border(0, CARD, 24),
        [pad],
    )
}

fn stat_cell_text(t: &mut Tree, s: &Styles, label: &str, big_text: &str) -> NodeId {
    let lbl = t.label(label, s.font_px(32).gray(INK_MID).line_height(1.2));
    let big_node = t.label(big_text, s.font_px(40).bold().line_height(1.2));
    let spacer = t.spacer(1);
    let col = t.add(Node::column(0).cross(Cross::Start), [lbl, spacer, big_node]);
    let pad = t.pad(Insets::new(20, 24, 20, 24), col);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(136),
        })
        .flex(1)
        .bg(CARD)
        .border(0, CARD, 24),
        [pad],
    )
}

/// Section 3: Prediction card (Black background, estimated date & days left).
fn prediction_card(t: &mut Tree, s: &Styles, stats: &BookStats) -> NodeId {
    let title = t.label(
        tr(Msg::BookStatsPredictionTitle),
        s.font_px(32).gray(PAPER).line_height(1.2),
    );
    let spacer = t.spacer(1);
    let right = match &stats.prediction {
        Prediction::Estimated {
            month,
            day,
            days_left,
        } => {
            let date_str = format_month_day(*month, *day);
            let date_lbl = t.label(&date_str, s.font_px(40).bold().gray(PAPER).line_height(1.2));
            let days_str = tf!(Msg::BookStatsPredictionDaysLeft, days = days_left);
            let days_lbl = t.label(&days_str, s.font_px(32).gray(0xDD).line_height(1.2));
            t.add(Node::row(14).cross(Cross::End), [date_lbl, days_lbl])
        }
        Prediction::Finished => t.label(
            tr(Msg::BookStatsPredictionFinished),
            s.font_px(40).bold().gray(PAPER).line_height(1.2),
        ),
        Prediction::None => t.label(
            tr(Msg::BookStatsPredictionUnknown),
            s.font_px(40).bold().gray(PAPER).line_height(1.2),
        ),
    };
    let row = t.add(Node::row(24).cross(Cross::Center), [title, spacer, right]);
    t.add(
        Node::new(Kind::Padding(Insets::new(22, 28, 22, 28)))
            .bg(INK)
            .border(0, INK, 24),
        [row],
    )
}

/// Section 4: 7-day reading duration bar chart.
fn chart_card(t: &mut Tree, s: &Styles, stats: &BookStats) -> NodeId {
    let title = t.label(
        tr(Msg::BookStatsRecent7Days),
        s.font_px(36).bold().line_height(1.2),
    );
    let spacer = t.spacer(1);
    let total_str = tf!(
        Msg::BookStatsRecent7DaysTotal,
        minutes = stats.week_total_minutes
    );
    let total = t.label(&total_str, s.font_px(32).gray(INK_MID).line_height(1.2));
    let head = t.add(Node::row(20).cross(Cross::Center), [title, spacer, total]);

    let max_min = stats
        .bars
        .iter()
        .map(|b| b.minutes)
        .max()
        .unwrap_or(0)
        .max(1);
    let mut cols = Vec::new();
    for bar in &stats.bars {
        let min_lbl = t.label(
            &bar.minutes.to_string(),
            s.font_px(32).gray(INK_MID).line_height(1.0),
        );
        let ratio = bar.minutes as f32 / max_min as f32;
        let track = t.custom(BarTrack { ratio });
        let track_centered = t.align(0.5, 0.5, track);
        let track_flex = t.add(Node::column(0).flex(1), [track_centered]);
        let day_lbl = t.label(&bar.label, s.font_px(32).gray(INK_MID).line_height(1.0));
        let col = t.add(
            Node::column(12).cross(Cross::Center).flex(1),
            [min_lbl, track_flex, day_lbl],
        );
        cols.push(col);
    }
    let chart_row = t.add(Node::row(20).cross(Cross::Stretch), cols);
    let chart_box = t.fixed(None, Some(300), Some(chart_row));

    let content = t.add(Node::column(22), [head, chart_box]);
    t.add(
        Node::new(Kind::Padding(Insets::new(24, 28, 22, 28)))
            .bg(CARD)
            .border(0, CARD, 24),
        [content],
    )
}

fn format_month_day(month: u8, day: u8) -> String {
    let m = month.clamp(1, 12);
    if language() == "zh" {
        tf!(Msg::BookStatsDateFormat, month = m, day = day)
    } else {
        let l = locale();
        let month_name = &l.months[(m - 1) as usize];
        tf!(Msg::BookStatsDateFormat, month = month_name, day = day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Fonts, TextEngine};
    use crate::theme::Styles;

    fn book_stats() -> BookStats<'static> {
        sample(StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        })
    }

    fn build() -> (Tree, TextEngine) {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (tree, _view) = BookStatsUi::build(&mut text, &s, &book_stats());
        (tree, text)
    }

    fn oasis() -> aristotle_screen::Screen {
        aristotle_screen::Screen::new(1264, 1680, 300).unwrap()
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
        // Header height matches HEADER_H
        assert_eq!(tree.rect(tree.node(root).children[1]).h, HEADER_H);
    }

    #[test]
    fn sample_has_seven_bars() {
        let stats = book_stats();
        assert_eq!(stats.bars.len(), 7);
        assert_eq!(stats.week_total_minutes, 263);
    }
}
