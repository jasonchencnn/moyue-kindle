// SPDX-License-Identifier: AGPL-3.0-or-later
//! A one-off measurement of the three sections: how long a screen takes
//! to build, lay out and paint, with a library of a thousand books.
//!
//!     cargo run -p aristotle-ui --example sections-bench

use std::time::Instant;

use aristotle_ui::geom::{SCREEN_HEIGHT, SCREEN_WIDTH};
use aristotle_ui::screens::home::{self as home_screen, Home};
use aristotle_ui::screens::library::{
    sample_books, CoverMode, Filter, Item, Library, LibraryLayout, Place, Shelf, StatusLine, View,
};
use aristotle_ui::screens::mine as mine_screen;
use aristotle_ui::text::{FamilyId, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::ui::Ui;

const STATUS: StatusLine<'static> = StatusLine {
    clock: "21:38",
    battery: Some(0.8),
    charging: false,
};

fn styles() -> Styles {
    Styles::new(FamilyId(0))
}

/// A library of `n` books, the newest first.
fn books(n: usize) -> Vec<aristotle_ui::screens::library::Book> {
    let mut out = sample_books();
    while out.len() < n {
        let i = out.len();
        let mut b = out[i % 12].clone();
        b.title = format!("{} #{i}", b.title);
        out.push(b);
    }
    out
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
        "{what:<28} best {best:>7.2} ms   mean {:>7.2} ms",
        total / runs as f64
    );
}

fn main() {
    let books = books(1000);
    let items: Vec<Item> = books.iter().cloned().map(Item::Book).collect();

    time("home: build", 20, || {
        let mut text = TextEngine::new(Fonts::embedded());
        let home = Home {
            reading: Some(&books[0]),
            recent: &books[1..],
            status: STATUS,
            clock: home_screen::ClockStyle::BigLunar,
            clock_text: home_screen::ClockText::default(),
            stats: None,
            quote: None,
            pet: false,
        };
        home_screen::HomeUi::build(&mut text, &styles(), &home, 1264)
    });

    time("mine: build", 20, || {
        let mut text = TextEngine::new(Fonts::embedded());
        let mine = mine_screen::sample(STATUS);
        mine_screen::MineUi::build(&mut text, &styles(), &mine)
    });

    time("library: build (1000 books)", 20, || {
        let mut text = TextEngine::new(Fonts::embedded());
        let shelf = Shelf {
            place: Place::Home {
                view: View::Books,
                filter: Filter::All,
            },
            hero: Some(&books[0]),
            items: &items,
            sort: "Recent",
            status: STATUS,
            covers: CoverMode::None,
            layout: LibraryLayout::List,
            local_books_count: items.len(),
        };
        Library::build_shelf(&mut text, &styles(), &shelf)
    });

    // The whole screen: build, lay out and paint.
    time("home: build+layout+paint", 20, || {
        let mut text = TextEngine::new(Fonts::embedded());
        let home = Home {
            reading: Some(&books[0]),
            recent: &books[1..],
            status: STATUS,
            clock: home_screen::ClockStyle::BigLunar,
            clock_text: home_screen::ClockText::default(),
            stats: None,
            quote: None,
            pet: false,
        };
        let (tree, _ids) = home_screen::HomeUi::build(&mut text, &styles(), &home, 1264);
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        ui.render()
    });

    time("library: build+layout+paint", 20, || {
        let mut text = TextEngine::new(Fonts::embedded());
        let shelf = Shelf {
            place: Place::Home {
                view: View::Books,
                filter: Filter::All,
            },
            hero: Some(&books[0]),
            items: &items,
            sort: "Recent",
            status: STATUS,
            covers: CoverMode::None,
            layout: LibraryLayout::List,
            local_books_count: items.len(),
        };
        let (tree, _ids) = Library::build_shelf(&mut text, &styles(), &shelf);
        let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
        ui.layout();
        ui.render()
    });
}
