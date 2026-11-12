// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three sections of the app and the bar that links them, checked
//! on the host.
//!
//! - Every section carries the bar at its foot, and the bar shows the
//!   section the reader is in.
//! - The three cells are targets a finger can hit, and a tap on one
//!   sends the action of that tab.
//! - Switching a section is one full-screen update, as any other screen
//!   switch is (see `transitions.rs`).

use aristotle_ui::geom::{Rect, SCREEN_HEIGHT, SCREEN_WIDTH};
use aristotle_ui::input::{Direction, Phase, TouchEvent};
use aristotle_ui::nav::{self, Tab};
use aristotle_ui::screens::about as about_screen;
use aristotle_ui::screens::home::{self as home_screen, Home};
use aristotle_ui::screens::library::{
    sample_books, CoverMode, Filter, Item, Library, LibraryLayout, Place, Shelf, StatusLine, View,
};
use aristotle_ui::screens::mine as mine_screen;
use aristotle_ui::text::{FamilyId, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{Hit, NodeId, Tree};
use aristotle_ui::ui::{Ui, UiEvent};
use aristotle_ui::Waveform;

const STATUS: StatusLine<'static> = StatusLine {
    clock: "21:38",
    battery: Some(0.8),
    charging: false,
};

fn styles() -> Styles {
    Styles::new(FamilyId(0))
}

/// The home page.
fn home() -> (Tree, TextEngine) {
    let mut text = TextEngine::new(Fonts::embedded());
    let books = sample_books();
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
    (tree, text)
}

/// The library.
fn library() -> (Tree, TextEngine) {
    let mut text = TextEngine::new(Fonts::embedded());
    let books = sample_books();
    let items: Vec<Item> = books.iter().skip(1).cloned().map(Item::Book).collect();
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
    let (tree, _lib) = Library::build_shelf(&mut text, &styles(), &shelf);
    (tree, text)
}

/// The "mine" page.
fn mine() -> (Tree, TextEngine) {
    let mut text = TextEngine::new(Fonts::embedded());
    let mine = mine_screen::sample(STATUS);
    let (tree, _ids) = mine_screen::MineUi::build(&mut text, &styles(), &mine);
    (tree, text)
}

/// The about page, under the "mine" section.
fn about() -> (Tree, TextEngine) {
    let mut text = TextEngine::new(Fonts::embedded());
    let facts = about_screen::About {
        status: STATUS,
        facts: &[],
    };
    let (tree, _ids) = about_screen::AboutUi::build(&mut text, &styles(), &facts);
    (tree, text)
}

/// The tree of a section, laid out in a screen.
fn laid_out(tree: Tree, text: TextEngine) -> Ui {
    let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
    ui.layout();
    ui
}

/// The node of the tree that sends `action`, if it has one.
fn node_with(tree: &Tree, action: u32) -> Option<NodeId> {
    (0..tree.len())
        .map(|n| NodeId(n as u32))
        .find(|&n| tree.node(n).action == Some(action))
}

/// The cell of `tab`, and where it is.
fn cell_of(tree: &Tree, tab: Tab) -> (NodeId, Rect) {
    let i = Tab::ALL
        .iter()
        .position(|t| *t == tab)
        .expect("the bar has three tabs") as u32;
    let id = node_with(tree, nav::action::TAB + i).expect("the bar has a cell for every tab");
    (id, tree.rect(id))
}

/// The tabs the tree shows, in the order they stand.
fn tabs_of(tree: &Tree) -> Vec<Tab> {
    (0..tree.len())
        .map(|n| NodeId(n as u32))
        .filter_map(|n| tree.node(n).action)
        .filter_map(nav::tab_of)
        .collect()
}

/// The three sections that carry the bar, and the about page that does not.
fn sections() -> Vec<(&'static str, Ui)> {
    let (tree, text) = home();
    let mut out = vec![("home", laid_out(tree, text))];
    let (tree, text) = library();
    out.push(("library", laid_out(tree, text)));
    let (tree, text) = mine();
    out.push(("mine", laid_out(tree, text)));
    let (tree, text) = about();
    out.push(("about", laid_out(tree, text)));
    out
}

#[test]
fn every_section_carries_the_bar() {
    for (name, ui) in sections() {
        if name == "about" {
            continue;
        }
        assert_eq!(
            tabs_of(&ui.tree),
            Tab::ALL.to_vec(),
            "{name}: the bar shows the three tabs"
        );
    }
}

#[test]
fn the_bar_stands_at_the_foot_of_the_screen() {
    for (name, ui) in sections() {
        if name == "about" {
            continue;
        }
        let mut bottom = 0;
        for tab in Tab::ALL {
            let (_id, rect) = cell_of(&ui.tree, tab);
            assert!(rect.w > 0 && rect.h > 0, "{name}: {tab:?} has a size");
            bottom = bottom.max(rect.bottom());
        }
        assert!(
            bottom <= SCREEN_HEIGHT,
            "{name}: the bar runs off the screen: {bottom}"
        );
        // The bar is at the foot: only the last pixels are under it.
        assert!(
            bottom > SCREEN_HEIGHT - nav::HEIGHT - 8,
            "{name}: the bar is not at the foot: {bottom}"
        );
    }
}

#[test]
fn the_cells_are_targets_a_finger_can_hit() {
    let (tree, text) = home();
    let ui = laid_out(tree, text);
    for tab in Tab::ALL {
        let (id, rect) = cell_of(&ui.tree, tab);
        assert!(
            rect.w >= 64 && rect.h >= 64,
            "{tab:?}: the cell is too small to hit: {rect:?}"
        );
        // A tap in the middle of the cell lands on it.
        assert_eq!(
            ui.tree.hit(rect.center()),
            Hit::Target(id),
            "{tab:?}: the tap misses the cell"
        );
    }
}

#[test]
fn the_cells_of_the_bar_do_not_overlap() {
    let (tree, text) = home();
    let ui = laid_out(tree, text);
    let cells: Vec<Rect> = Tab::ALL
        .into_iter()
        .map(|tab| cell_of(&ui.tree, tab).1)
        .collect();
    for (i, a) in cells.iter().enumerate() {
        for b in cells.iter().skip(i + 1) {
            assert!(
                a.intersect(b).is_none(),
                "two cells of the bar overlap: {a:?} and {b:?}"
            );
        }
    }
}

#[test]
fn the_current_tab_is_the_inverted_one() {
    let (tree, text) = home();
    let ui = laid_out(tree, text);
    let inverted: Vec<Tab> = Tab::ALL
        .into_iter()
        .filter(|tab| {
            let (id, _rect) = cell_of(&ui.tree, *tab);
            let column = ui.tree.node(id).children[0];
            ui.tree
                .node(column)
                .children
                .iter()
                .any(|n| ui.tree.node(*n).inverted)
        })
        .collect();
    assert_eq!(inverted, vec![Tab::Home]);
}

#[test]
fn a_tap_on_a_cell_sends_the_tab() {
    let (tree, text) = home();
    let mut ui = laid_out(tree, text);
    let (_id, rect) = cell_of(&ui.tree, Tab::Mine);
    let at = rect.center();
    let down = ui.handle_touch(TouchEvent {
        phase: Phase::Down,
        id: 1,
        x: at.x,
        y: at.y,
        time_us: 0,
    });
    assert!(down.is_empty(), "the touch down only shows the press");
    let up = ui.handle_touch(TouchEvent {
        phase: Phase::Up,
        id: 1,
        x: at.x,
        y: at.y,
        time_us: 60_000,
    });
    let sent: Vec<u32> = up
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Action { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    let mine = Tab::ALL.iter().position(|t| *t == Tab::Mine).unwrap() as u32;
    assert_eq!(
        sent,
        vec![nav::action::TAB + mine],
        "the cell of the tab sends the action of that tab"
    );
}

#[test]
fn the_about_page_has_no_bar_of_its_own() {
    // The about page hangs under the "mine" section: it has a way back,
    // not a second bar.
    let (tree, text) = about();
    let ui = laid_out(tree, text);
    assert!(tabs_of(&ui.tree).is_empty(), "the about page has no bar");
}

#[test]
fn the_cells_stay_inside_the_screen() {
    for (name, ui) in sections() {
        if name == "about" {
            continue;
        }
        for tab in Tab::ALL {
            let (_id, rect) = cell_of(&ui.tree, tab);
            assert!(
                rect.x >= 0 && rect.right() <= SCREEN_WIDTH,
                "{name}: {tab:?}: {rect:?}"
            );
        }
    }
}

#[test]
fn switching_a_section_is_one_full_flash() {
    // A section is a new screen of text: one clean full-screen update,
    // like a book page (see `transitions.rs`).
    let (tree, text) = home();
    let mut ui = laid_out(tree, text);
    let (tree, _text) = library();
    ui.set_tree(tree);
    let damage = ui.render();
    assert_eq!(damage.len(), 1, "the switch is one update: {damage:?}");
    assert_eq!(damage[0].rect, ui.bounds(), "the update covers the screen");
    assert_eq!(
        damage[0].waveform,
        Waveform::GC16,
        "the switch is a full flash"
    );
    // Nothing follows a switch: no late redraw.
    assert!(ui.render().is_empty());
}

#[test]
fn library_grid_view_toggle_pagination_and_opening() {
    use aristotle_ui::screens::library::action;

    let mut text = TextEngine::new(Fonts::embedded());
    let books = sample_books();
    let items: Vec<Item> = books.iter().cloned().map(Item::Book).collect();
    let mut shelf = Shelf {
        place: Place::Home {
            view: View::Books,
            filter: Filter::All,
        },
        hero: None,
        items: &items,
        sort: "Recent",
        status: STATUS,
        covers: CoverMode::Cards,
        layout: LibraryLayout::List,
        local_books_count: items.len(),
    };

    // 1. Start in List layout.
    let (tree, lib) = Library::build_shelf(&mut text, &styles(), &shelf);
    let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
    ui.layout();
    lib.sync(&mut ui.tree);

    let toggle_action = (0..ui.tree.len())
        .map(|i| NodeId(i as u32))
        .find(|&id| ui.tree.node(id).action == Some(action::TOGGLE_LAYOUT));
    assert!(toggle_action.is_some(), "toggle button exists in header");

    // 2. Toggle to Grid layout.
    shelf.layout = shelf.layout.toggle();
    assert_eq!(shelf.layout, LibraryLayout::Grid);
    let (tree, mut lib) = Library::build_shelf(&mut ui.text, &styles(), &shelf);
    ui.set_tree(tree);
    ui.layout();
    lib.sync(&mut ui.tree);

    // 12 books -> 2 pages in grid mode (6 per page).
    let (page, count) = ui.tree.page(lib.list);
    assert_eq!(page, 0);
    assert_eq!(count, 2);

    // 3. Turn to page 2 in Grid layout.
    assert!(lib.handle(&mut ui.tree, action::NEXT_PAGE));
    ui.layout();
    lib.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib.list).0, 1);

    // 4. Change filter to Reading.
    shelf.place = Place::Home {
        view: View::Books,
        filter: Filter::Reading,
    };
    let (tree, lib) = Library::build_shelf(&mut ui.text, &styles(), &shelf);
    ui.set_tree(tree);
    ui.layout();
    lib.sync(&mut ui.tree);

    // 5. Click book in grid view sends action::ITEM.
    let book_cell = (0..ui.tree.len())
        .map(|i| NodeId(i as u32))
        .find(|&id| ui.tree.node(id).action == Some(action::ITEM));
    assert!(book_cell.is_some(), "book item is clickable in grid view");
}

#[test]
fn library_swipe_and_jump_page_interactions() {
    use aristotle_ui::screens::library::action;

    let mut text = TextEngine::new(Fonts::embedded());
    let books = sample_books();
    // Replicate books to get at least 30 items for multi-page list and grid testing.
    let mut items = Vec::new();
    for i in 0..30 {
        let mut b = books[i % books.len()].clone();
        b.title = format!("Book {i}");
        items.push(Item::Book(b));
    }

    let mut shelf = Shelf {
        place: Place::Home {
            view: View::Books,
            filter: Filter::All,
        },
        hero: None,
        items: &items,
        sort: "Recent",
        status: STATUS,
        covers: CoverMode::Cards,
        layout: LibraryLayout::List,
        local_books_count: items.len(),
    };

    // 1. List layout swipe interactions.
    let (tree, mut lib) = Library::build_shelf(&mut text, &styles(), &shelf);
    let mut ui = Ui::new(tree, text, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
    ui.layout();
    lib.sync(&mut ui.tree);

    let (initial_page, page_count) = ui.tree.page(lib.list);
    assert_eq!(initial_page, 0);
    assert!(
        page_count >= 5,
        "expected multiple pages in list layout, got {page_count}"
    );

    // Swipe Up -> Next Page
    assert!(lib.handle_swipe(&mut ui.tree, Direction::Up));
    ui.layout();
    lib.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib.list).0, 1);

    // Swipe Down -> Prev Page
    assert!(lib.handle_swipe(&mut ui.tree, Direction::Down));
    ui.layout();
    lib.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib.list).0, 0);

    // Swipe Left -> Next Page
    assert!(lib.handle_swipe(&mut ui.tree, Direction::Left));
    ui.layout();
    lib.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib.list).0, 1);

    // Swipe Right -> Prev Page
    assert!(lib.handle_swipe(&mut ui.tree, Direction::Right));
    ui.layout();
    lib.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib.list).0, 0);

    // 2. Grid layout swipe interactions.
    shelf.layout = LibraryLayout::Grid;
    let (tree, mut lib_grid) = Library::build_shelf(&mut ui.text, &styles(), &shelf);
    ui.set_tree(tree);
    ui.layout();
    lib_grid.sync(&mut ui.tree);

    let (grid_page, grid_count) = ui.tree.page(lib_grid.list);
    assert_eq!(grid_page, 0);
    assert_eq!(grid_count, 5, "30 items at 6 per page should be 5 pages");

    // Grid: Swipe Up -> Next Page
    assert!(lib_grid.handle_swipe(&mut ui.tree, Direction::Up));
    ui.layout();
    lib_grid.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib_grid.list).0, 1);

    // Grid: Swipe Down -> Prev Page
    assert!(lib_grid.handle_swipe(&mut ui.tree, Direction::Down));
    ui.layout();
    lib_grid.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib_grid.list).0, 0);

    // Grid: Swipe Left -> Next Page
    assert!(lib_grid.handle_swipe(&mut ui.tree, Direction::Left));
    ui.layout();
    lib_grid.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib_grid.list).0, 1);

    // Grid: Swipe Right -> Prev Page
    assert!(lib_grid.handle_swipe(&mut ui.tree, Direction::Right));
    ui.layout();
    lib_grid.sync(&mut ui.tree);
    assert_eq!(ui.tree.page(lib_grid.list).0, 0);

    // 3. Jump-to-page dialog interactions.
    assert!(!lib_grid.jump_open);
    assert!(ui.tree.node(lib_grid.jump_overlay).hidden);

    // Open jump dialog by tapping pager
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP));
    assert!(lib_grid.jump_open);
    assert!(!ui.tree.node(lib_grid.jump_overlay).hidden);
    assert_eq!(lib_grid.jump_target, 1);

    // Step +5
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_PLUS_5));
    assert_eq!(lib_grid.jump_target, 5); // clamped to grid_count = 5

    // Step -1
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_MINUS_1));
    assert_eq!(lib_grid.jump_target, 4);

    // Step First
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_FIRST));
    assert_eq!(lib_grid.jump_target, 1);

    // Step Last
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_LAST));
    assert_eq!(lib_grid.jump_target, 5);

    // Step -5
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_MINUS_5));
    assert_eq!(lib_grid.jump_target, 1);

    // While dialog is open, swipe gestures must be suppressed/ignored
    assert!(!lib_grid.handle_swipe(&mut ui.tree, Direction::Up));
    assert!(!lib_grid.handle_swipe(&mut ui.tree, Direction::Down));
    assert_eq!(ui.tree.page(lib_grid.list).0, 0);

    // Cancel dialog
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_CANCEL));
    assert!(!lib_grid.jump_open);
    assert!(ui.tree.node(lib_grid.jump_overlay).hidden);
    assert_eq!(
        ui.tree.page(lib_grid.list).0,
        0,
        "cancelling does not change page"
    );

    // Open dialog again, adjust target to 3, and confirm (GO)
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP));
    assert_eq!(lib_grid.jump_target, 1);
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_PLUS_1));
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_PLUS_1));
    assert_eq!(lib_grid.jump_target, 3);
    assert!(lib_grid.handle(&mut ui.tree, action::JUMP_GO));
    assert!(!lib_grid.jump_open);
    assert!(ui.tree.node(lib_grid.jump_overlay).hidden);
    assert_eq!(
        ui.tree.page(lib_grid.list).0,
        2,
        "target 3 turns to page index 2"
    );
}
