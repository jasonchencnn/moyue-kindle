// SPDX-License-Identifier: AGPL-3.0-or-later
//! The last screen of the app: the account, the reading, and the way out.
//!
//! The screen follows the prototype of `docs/ui/prototype-v2`: the account
//! card, the figures of the reading, the streak, the list of entries and
//! the two buttons that leave the app. Everything the prototype shows is
//! here; the features it links to but that are not built yet (the reading
//! calendar, the notes, the quotations) open a page that says so.
//!
//! The measurements are the prototype's, in reference pixels: a 88 px
//! margin, a 112 px header, 24 px round cards on the panel gray.

use super::library::{status_line, StatusLine};
use crate::geom::{Insets, Size};
use crate::icons::Icon;
use crate::status::LoadingNodes;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, CARD, INK, INK_MID, PAPER};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use crate::widgets::GoalBar;
use aristotle_i18n::{tr, trn, Msg, Plural};

pub mod action {
    use crate::tree::ActionId;
    /// The calendar of the header.
    pub const CALENDAR: ActionId = 1;
    /// The button of the account card: sign in, or sync.
    pub const ACCOUNT: ActionId = 2;
    /// The "reading calendar" of the statistics.
    pub const STATS_CALENDAR: ActionId = 3;
    /// The entries of the list.
    pub const ENTRY_CALENDAR: ActionId = 4;
    pub const ENTRY_MARKS: ActionId = 5;
    pub const ENTRY_QUOTES: ActionId = 6;
    /// The settings and the about page.
    pub const SETTINGS: ActionId = 7;
    pub const ABOUT: ActionId = 8;
    /// The two buttons at the foot of the screen.
    pub const RETURN_KINDLE: ActionId = 9;
    pub const OPEN_KUAL: ActionId = 10;
}

/// The account card: who the reader is.
pub enum Account {
    /// Nobody is signed in: the card offers to sign in.
    Guest,
    /// Signed in as `name`; `last_sync` says when the last sync was.
    SignedIn { name: String, last_sync: String },
}

/// The figures of the reading.
pub struct Stats {
    /// The minutes read today.
    pub today_minutes: u32,
    /// The daily goal, in minutes.
    pub goal_minutes: u32,
    /// The hours read this week.
    pub week_hours: f32,
    /// The hours read in all.
    pub total_hours: u32,
    /// The books finished.
    pub finished: u32,
}

/// The streak row: the days read in a row, and the last seven days.
pub struct Streak {
    /// The days read in a row.
    pub days: u32,
    /// The last seven days, the oldest first: true when the day was read.
    pub week: [bool; 7],
}

/// The values the entries of the list show on the right.
pub struct Entries {
    /// The highlights in the library.
    pub highlights: u32,
    /// The notes in the library.
    pub notes: u32,
    /// The version of the program, as "0.3.0".
    pub version: String,
}

/// How many quotations the library holds. The quotations themselves are
/// not in the app yet (`docs/ui/quotes-zh.json`); the count follows the
/// prototype until they are.
pub const QUOTES: u32 = 100;

/// What the "mine" screen shows.
pub struct Mine<'a> {
    pub status: StatusLine<'a>,
    pub account: Account,
    pub stats: Stats,
    pub streak: Streak,
    pub entries: Entries,
}

/// A sample of the screen, for the preview and the tests: the figures of
/// the prototype.
pub fn sample(status: StatusLine<'static>) -> Mine<'static> {
    Mine {
        status,
        account: Account::Guest,
        stats: Stats {
            today_minutes: 23,
            goal_minutes: 30,
            week_hours: 3.2,
            total_hours: 186,
            finished: 24,
        },
        streak: Streak {
            days: 5,
            week: [true, false, true, true, true, true, true],
        },
        entries: Entries {
            highlights: 12,
            notes: 3,
            // The version of the build, like the app shows it.
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    }
}

/// Handles to the nodes of the "mine" screen.
pub struct MineUi {
    /// The rows of the list of entries.
    pub rows: Vec<NodeId>,
    /// The loading area in the middle of the status line, hidden.
    pub loading: LoadingNodes,
}

/// The height of the header, in reference pixels.
const HEADER_H: i32 = 112;
/// The height of a tile of the grid.
const TILE_H: i32 = 156;
/// The radius of a tile of the grid.
const TILE_R: i32 = 24;
/// The height of the account button.
const ACCOUNT_BTN_H: i32 = 80;
/// The height of the buttons at the foot of the screen.
const FOOT_BTN_H: i32 = 72;

impl MineUi {
    /// Builds the "mine" screen.
    pub fn build(_text: &mut TextEngine, s: &Styles, mine: &Mine) -> (Tree, Self) {
        let mut t = Tree::new();
        let bar = status_line(&mut t, s, &mine.status);
        let status = t.pad(Insets::new(0, home_margin(), 0, home_margin()), bar.root);
        let header = header(&mut t, s);
        let header = t.pad(Insets::new(0, home_margin(), 0, home_margin()), header);
        let mut rows = Vec::new();
        let body = body(&mut t, s, mine, &mut rows);
        let nav = crate::nav::build(&mut t, s, crate::nav::Tab::Mine);
        let root = t.add(Node::column(0), [status, header, body, nav.root]);
        t.set_root(root);
        (
            t,
            MineUi {
                rows,
                loading: bar.loading,
            },
        )
    }
}

/// The header: the name of the screen, and the calendar beside it.
fn header(t: &mut Tree, s: &Styles) -> NodeId {
    let title = t.label(
        tr(Msg::MineTitle),
        s.font_lg().bold().line_height(1.2).tracking(4.0),
    );
    let icon = t.icon(Icon::Calendar, 40, INK);
    let label = t.label(tr(Msg::MineCalendar), s.font_link());
    let link = t.add(Node::row(10).cross(Cross::Center), [icon, label]);
    // The header is 64 px tall; the action fills it so the visible target
    // is 64 px while the icon and label keep their size (QA-101 P2-2).
    let link = t.add(
        Node::new(Kind::MinSize(Size::new(0, 64)))
            .action(action::CALENDAR)
            .feedback(Feedback::Invert),
        [link],
    );
    let fill = t.spacer(1);
    let row = t.add(Node::row(0).cross(Cross::Center), [title, fill, link]);
    t.fixed(None, Some(HEADER_H), Some(row))
}

/// Everything between the header and the navigation bar.
fn body(t: &mut Tree, s: &Styles, mine: &Mine, rows: &mut Vec<NodeId>) -> NodeId {
    let mut parts = vec![
        account_card(t, s, &mine.account),
        t.gap(0, 28),
        stats_title(t, s),
        // 6 px: the stats heading is 64 px (8 more than before) so the grid
        // below stays where it was.
        t.gap(0, 6),
        grid(t, s, &mine.stats),
        t.gap(0, 14),
        streak_row(t, s, &mine.streak),
        t.gap(0, 28),
    ];
    parts.push(entry_list(t, s, &mine.entries, rows));
    parts.push(t.gap(0, 24));
    parts.push(foot(t, s));
    let col = t.add(Node::column(0), parts);
    let col = t.pad(Insets::new(28, home_margin(), 32, home_margin()), col);
    t.add(Node::column(0).flex(1), [col])
}

/// The account card: the avatar, the name, the state of the sync, and the
/// button that starts it.
fn account_card(t: &mut Tree, s: &Styles, account: &Account) -> NodeId {
    let glyph = t.icon(Icon::User, 52, PAPER);
    let glyph = t.align(0.5, 0.5, glyph);
    let avatar = t.add(
        Node::new(Kind::Fixed {
            w: Some(88),
            h: Some(88),
        })
        .bg(INK)
        .border(0, INK, 44),
        [glyph],
    );

    let (name, line, button) = match account {
        Account::Guest => (
            tr(Msg::MineAccountGuest).to_string(),
            tr(Msg::MineAccountGuestHint).to_string(),
            tr(Msg::MineAccountLogin).to_string(),
        ),
        Account::SignedIn { name, last_sync } => (
            name.clone(),
            aristotle_i18n::tf!(Msg::MineAccountSignedIn, time = last_sync),
            tr(Msg::MineAccountSync).to_string(),
        ),
    };
    let name_node = one_line(t, &name, s.font_base().bold().line_height(1.2));
    let line_node = one_line(t, &line, s.font_xs().gray(INK_MID).line_height(1.3));
    let words = t.add(Node::column(4).flex(1), [name_node, line_node]);

    let glyph = t.icon(Icon::Sync, 30, PAPER);
    let label = t.label(&button, s.font_link().bold().gray(PAPER));
    let content = t.add(Node::row(10).cross(Cross::Center), [glyph, label]);
    let content = t.pad(Insets::new(0, 28, 0, 28), content);
    let sync = t.add(
        Node::new(Kind::MinSize(Size::new(0, ACCOUNT_BTN_H)))
            .bg(INK)
            .border(0, INK, 20)
            .action(action::ACCOUNT),
        [content],
    );

    let row = t.add(Node::row(24).cross(Cross::Center), [avatar, words, sync]);
    t.add(
        Node::new(Kind::Padding(Insets::new(24, 32, 24, 32)))
            .bg(CARD)
            .border(0, CARD, 32),
        [row],
    )
}

/// The heading of the statistics, and the link to the calendar.
fn stats_title(t: &mut Tree, s: &Styles) -> NodeId {
    let title = t.label(
        tr(Msg::MineStatsTitle),
        s.font_base().bold().line_height(1.2),
    );
    let title = t.pad(Insets::new(6, 0, 0, 0), title);
    let label = t.label(tr(Msg::MineEntryCalendar), s.font_link());
    let chevron = t.icon(Icon::ChevronRight, 28, INK);
    let link = t.add(Node::row(8).cross(Cross::Center), [label, chevron]);
    // The row is 64 px tall and the link fills it, so the visible target is
    // 64 px. Its content keeps the old 6 px top offset; the 8 px the row
    // gains is taken back from the gap before the grid (QA-101 P2-2).
    let link = t.pad(Insets::new(6, 0, 14, 0), link);
    let link = t.add(
        Node::new(Kind::Padding(Insets::all(0)))
            .action(action::STATS_CALENDAR)
            .feedback(Feedback::Invert),
        [link],
    );
    let fill = t.spacer(1);
    let row = t.add(Node::row(0).cross(Cross::Start), [title, fill, link]);
    t.fixed(None, Some(64), Some(row))
}

/// The four figures of the reading, two by two.
fn grid(t: &mut Tree, s: &Styles, stats: &Stats) -> NodeId {
    let today = today_tile(t, s, stats, 1);
    let week = tile(
        t,
        s,
        tr(Msg::MineWeek),
        &hours(stats.week_hours),
        tr(Msg::MineHours),
        1,
    );
    let total = tile(
        t,
        s,
        tr(Msg::MineTotal),
        &stats.total_hours.to_string(),
        tr(Msg::MineHours),
        1,
    );
    let finished = tile(
        t,
        s,
        tr(Msg::MineFinished),
        &stats.finished.to_string(),
        tr(Msg::MineBooks),
        1,
    );
    let first = t.add(Node::row(20).cross(Cross::Start), [today, week]);
    let second = t.add(Node::row(20).cross(Cross::Start), [total, finished]);
    t.add(Node::column(20), [first, second])
}

/// The tile of the minutes read today, with the bar of the goal under the
/// figure.
fn today_tile(t: &mut Tree, s: &Styles, stats: &Stats, flex: u16) -> NodeId {
    let label = t.label(
        tr(Msg::MineToday),
        s.font_xs().gray(INK_MID).line_height(1.2),
    );
    let number = t.label(
        &stats.today_minutes.to_string(),
        s.font_md().bold().line_height(1.1),
    );
    let goal = aristotle_i18n::tf!(Msg::MineGoal, minutes = stats.goal_minutes);
    let unit = t.label(&goal, s.font_xs().line_height(1.2));
    let figures = t.add(Node::row(10).cross(Cross::End), [number, unit]);
    let bar = t.custom(GoalBar {
        value: stats.today_minutes as f32 / stats.goal_minutes.max(1) as f32,
    });
    let block = t.add(Node::column(12), [figures, bar]);
    let fill = t.spacer(1);
    let col = t.add(Node::column(0), [label, fill, block]);
    let col = t.pad(Insets::new(20, 24, 20, 24), col);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(TILE_H),
        })
        .bg(CARD)
        .border(0, CARD, TILE_R)
        .flex(flex),
        [col],
    )
}

/// A tile of the grid: a label on top, a figure at the foot.
fn tile(t: &mut Tree, s: &Styles, label: &str, value: &str, unit: &str, flex: u16) -> NodeId {
    let label = t.label(label, s.font_xs().gray(INK_MID).line_height(1.2));
    let number = t.label(value, s.font_md().bold().line_height(1.1));
    let unit = t.label(unit, s.font_xs().line_height(1.2));
    let figures = t.add(Node::row(10).cross(Cross::End), [number, unit]);
    let fill = t.spacer(1);
    let col = t.add(Node::column(0), [label, fill, figures]);
    let col = t.pad(Insets::new(20, 24, 20, 24), col);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(TILE_H),
        })
        .bg(CARD)
        .border(0, CARD, TILE_R)
        .flex(flex),
        [col],
    )
}

/// The streak: the days read in a row, and the last seven days.
fn streak_row(t: &mut Tree, s: &Styles, streak: &Streak) -> NodeId {
    let label = t.label(
        tr(Msg::MineStreak),
        s.font_xs().gray(INK_MID).line_height(1.2),
    );
    let number = t.label(
        &streak.days.to_string(),
        s.font_md().bold().line_height(1.0),
    );
    let unit = t.label(tr(Msg::MineDays), s.font_xs().line_height(1.0));
    let figures = t.add(Node::row(8).cross(Cross::End), [number, unit]);

    let mut dots = Vec::new();
    for read in streak.week {
        let dot = if read {
            t.add(
                Node::new(Kind::Fixed {
                    w: Some(32),
                    h: Some(32),
                })
                .bg(INK)
                .border(0, INK, 16),
                [],
            )
        } else {
            t.add(
                Node::new(Kind::Fixed {
                    w: Some(32),
                    h: Some(32),
                })
                .bg(PAPER)
                .border(2, INK, 16),
                [],
            )
        };
        dots.push(dot);
    }
    let dots = t.add(Node::row(14).cross(Cross::Center), dots);
    let fill = t.spacer(1);
    let dots = t.add(Node::row(0).cross(Cross::Center), [fill, dots]);

    let row = t.add(Node::row(16).cross(Cross::Center), [label, figures, dots]);
    t.add(
        Node::new(Kind::Padding(Insets::new(18, 24, 18, 24)))
            .bg(CARD)
            .border(0, CARD, 24),
        [row],
    )
}

/// The list of entries: the calendar, the notes, the quotations, the
/// settings and the about page.
fn entry_list(t: &mut Tree, s: &Styles, entries: &Entries, rows: &mut Vec<NodeId>) -> NodeId {
    let marks = format!(
        "{} · {}",
        trn(Plural::MineHighlights, entries.highlights as usize),
        trn(Plural::MineNotes, entries.notes as usize),
    );
    let quotes = trn(Plural::MineQuotes, QUOTES as usize);
    let version = aristotle_i18n::tf!(Msg::MineVersion, version = entries.version);
    let items = [
        (tr(Msg::MineEntryCalendar), None, action::ENTRY_CALENDAR),
        (tr(Msg::MineEntryMarks), Some(marks), action::ENTRY_MARKS),
        (tr(Msg::MineEntryQuotes), Some(quotes), action::ENTRY_QUOTES),
        (tr(Msg::SettingsTitle), None, action::SETTINGS),
        (tr(Msg::MineAbout), Some(version), action::ABOUT),
    ];
    let mut list = Vec::new();
    for (label, value, id) in items {
        let row = entry_row(t, s, label, value.as_deref(), id);
        rows.push(row);
        list.push(row);
    }
    let col = t.add(Node::column(0), list);
    t.add(
        Node::new(Kind::Padding(Insets::new(8, 28, 8, 28)))
            .bg(CARD)
            .border(0, CARD, 24)
            .flex(1),
        [col],
    )
}

/// A row of the list: the name on the left, the value and a chevron on the
/// right.
fn entry_row(t: &mut Tree, s: &Styles, label: &str, value: Option<&str>, id: ActionId) -> NodeId {
    let name = one_line(t, label, s.font_sm().line_height(1.3));
    let name = t.add(Node::column(0).flex(1), [name]);
    let mut parts = Vec::new();
    if let Some(v) = value {
        parts.push(t.label(v, s.font_xs().gray(INK_MID).line_height(1.2)));
    }
    parts.push(t.icon(Icon::ChevronRight, 28, INK));
    let value = t.add(Node::row(10).cross(Cross::Center), parts);
    t.add(
        Node::row(20)
            .cross(Cross::Center)
            .flex(1)
            .action(id)
            .feedback(Feedback::Invert),
        [name, value],
    )
}

/// The two buttons at the foot: back to the stock reader, and to the KUAL
/// extension.
fn foot(t: &mut Tree, s: &Styles) -> NodeId {
    let back = outline_button(
        t,
        s,
        Icon::ChevronLeft,
        tr(Msg::SettingsReturnKindle),
        action::RETURN_KINDLE,
    );
    let kual = outline_button(
        t,
        s,
        Icon::App,
        tr(Msg::SettingsOpenKual),
        action::OPEN_KUAL,
    );
    t.add(Node::row(16).cross(Cross::Center), [back, kual])
}

/// A button with an outline: a white field, a black frame, an icon and a
/// word.
fn outline_button(t: &mut Tree, s: &Styles, icon: Icon, label: &str, id: ActionId) -> NodeId {
    let glyph = t.icon(icon, 32, INK);
    let label = t.label(label, s.font_xs());
    let content = t.add(Node::row(12).cross(Cross::Center), [glyph, label]);
    t.add(
        Node::new(Kind::MinSize(Size::new(0, FOOT_BTN_H)))
            .border(2, INK, 20)
            .flex(1)
            .action(id)
            .feedback(Feedback::Invert),
        [content],
    )
}

/// A line of text that ends in an ellipsis when it does not fit.
fn one_line(t: &mut Tree, text: &str, style: crate::text::TextStyle) -> NodeId {
    let mut p = Paragraph::plain(text, style, Align::Left);
    p.style.max_lines = Some(1);
    t.text(p)
}

/// The hours of a week, with the decimal mark of the language.
fn hours(value: f32) -> String {
    let text = format!("{value:.1}");
    let mark = aristotle_i18n::locale().decimal;
    if mark == "." {
        text
    } else {
        text.replace('.', mark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Fonts, TextEngine};
    use crate::theme::Styles;

    fn mine() -> Mine<'static> {
        sample(StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        })
    }

    fn build() -> (Tree, TextEngine) {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (tree, _view) = MineUi::build(&mut text, &s, &mine());
        (tree, text)
    }

    /// The screen of the prototype: a Kindle Oasis 2 at 300 DPI.
    fn oasis() -> aristotle_screen::Screen {
        aristotle_screen::Screen::new(1264, 1680, 300).unwrap()
    }

    #[test]
    fn the_screen_stands_on_the_navigation_bar() {
        let (mut tree, mut text) = build();
        let screen = oasis();
        tree.layout(
            crate::geom::Rect::new(0, 0, screen.width as i32, screen.height as i32),
            &mut text,
        );
        let root = tree.root().unwrap();
        let children = &tree.node(root).children;
        // The status line, the header, the body and the bar.
        assert_eq!(children.len(), 4);
        assert_eq!(tree.rect(*children.last().unwrap()).h, crate::nav::HEIGHT);
    }

    #[test]
    fn the_body_stays_inside_the_screen() {
        let (mut tree, mut text) = build();
        let screen = oasis();
        tree.layout(
            crate::geom::Rect::new(0, 0, screen.width as i32, screen.height as i32),
            &mut text,
        );
        let root = tree.root().unwrap();
        let body = tree.node(tree.node(root).children[2]);
        let bottom = body.rect.bottom();
        let nav = tree.node(tree.node(root).children[3]);
        assert_eq!(bottom, nav.rect.y);
        assert!(bottom <= screen.height as i32);
    }

    #[test]
    fn the_header_carries_the_title_and_the_calendar() {
        let (mut tree, mut text) = build();
        let screen = oasis();
        tree.layout(
            crate::geom::Rect::new(0, 0, screen.width as i32, screen.height as i32),
            &mut text,
        );
        let root = tree.root().unwrap();
        let header = tree.node(root).children[1];
        assert_eq!(tree.rect(header).h, HEADER_H);
    }

    #[test]
    fn the_list_has_five_entries() {
        let mut text = TextEngine::new(Fonts::embedded());
        let s = Styles::new(crate::text::FamilyId(0));
        let (_tree, view) = MineUi::build(&mut text, &s, &mine());
        assert_eq!(view.rows.len(), 5);
    }
}
