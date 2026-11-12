// SPDX-License-Identifier: AGPL-3.0-or-later
//! A one-off measurement of the home screen: the calendar the clock card
//! reads, the quotation it picks, and the screen itself.
//!
//!     cargo run -p aristotle-ui --example home-bench
//!
//! The statistics come from a file in the app's crate, so they are not
//! read here; the app prints its own timing for them.

use std::time::Instant;

use aristotle_ui::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
use aristotle_ui::screens::home::{self as home_screen, ClockText, Home, Stats};
use aristotle_ui::screens::home::{lunar, quotes};
use aristotle_ui::screens::library::{sample_books, Book, StatusLine};
use aristotle_ui::text::{Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::ui::Ui;

const STATUS: StatusLine<'static> = StatusLine {
    clock: "21:38",
    battery: Some(0.8),
    charging: false,
};

fn styles() -> Styles {
    Styles::new(aristotle_ui::text::FamilyId(0))
}

/// The words the clock card shows, the way the app writes them.
fn clock_text() -> ClockText {
    let lunar_day = lunar::Lunar::of(2026, 11, 4);
    let mut term = None;
    if let Some(t) = &lunar_day.term {
        term = Some(t.zh());
    }
    ClockText {
        time: "21:38".into(),
        date: "11月4日 星期三".into(),
        month_day: "11月4日".into(),
        weekday: "星期三".into(),
        lunar: format!(
            "{} {}{}",
            lunar_day.ganzhi, lunar_day.month_name, lunar_day.day_name
        ),
        lunar_short: format!("{}{}", lunar_day.month_name, lunar_day.day_name),
        term,
    }
}

fn time<T>(what: &str, runs: usize, mut f: impl FnMut() -> T) {
    let mut best = f64::MAX;
    let mut total = 0.0;
    for _ in 0..runs {
        let t = Instant::now();
        let v = f();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        best = best.min(ms);
        std::hint::black_box(v);
    }
    println!(
        "{what:<34} best {best:>7.3} ms   mean {:>7.3} ms",
        total / runs as f64
    );
}

fn main() {
    let books: Vec<Book> = sample_books();
    let stats = Stats {
        today_minutes: 23,
        streak_days: 5,
        week_hours: 3.2,
    };
    let quote = &quotes::QUOTES[quotes::of_day(0)];

    time("lunar: the day and its term", 200, || {
        let l = lunar::Lunar::of(2026, 11, 4);
        let _ = l.zh();
        let _ = l.en();
    });

    time("quote: the one of the day", 200, || quotes::of_day(0));

    time("home: build", 20, || {
        let mut text = TextEngine::new(Fonts::embedded());
        let home = Home {
            reading: Some(&books[0]),
            recent: &books[1..],
            status: STATUS,
            clock: home_screen::ClockStyle::BigLunar,
            clock_text: clock_text(),
            stats: Some(stats),
            quote: Some(quote),
            pet: true,
        };
        home_screen::HomeUi::build(&mut text, &styles(), &home, 1264)
    });

    time("home: build+layout+paint", 20, || {
        let mut text = TextEngine::new(Fonts::embedded());
        let home = Home {
            reading: Some(&books[0]),
            recent: &books[1..],
            status: STATUS,
            clock: home_screen::ClockStyle::BigLunar,
            clock_text: clock_text(),
            stats: Some(stats),
            quote: Some(quote),
            pet: true,
        };
        let (tree, _view) = home_screen::HomeUi::build(&mut text, &styles(), &home, 1264);
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        ui.render()
    });
}
