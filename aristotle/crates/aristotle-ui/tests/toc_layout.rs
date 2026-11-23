// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! The contents list, on the host: the fold markers, the pager and the
//! buttons that turn the pages of the list.
//!
//! The rules checked here:
//!
//! * the fold marker is its own button: a tap on it folds, and never
//!   jumps to the chapter, and
//! * the pager says which page is shown, and the buttons that have
//!   nowhere to go are grey.

use aristotle_ui::geom::{Point, Rect, Screen};
use aristotle_ui::screens::toc as toc_screen;
use aristotle_ui::text::{FamilyId, Fonts, TextEngine};
use aristotle_ui::theme::{Styles, INK, RULE};
use aristotle_ui::tree::{Hit, Kind, NodeId, Tree};
use aristotle_ui::ui::Ui;

/// The panels the list has to fit on.
const PANELS: [(u32, u32, u16); 3] = [(600, 800, 167), (758, 1024, 212), (1264, 1680, 300)];

fn engine() -> TextEngine {
    TextEngine::new(Fonts::embedded())
}

/// A book with three chapters, the second one holding two sections: the
/// reader is in one of the sections, so that group is open.
fn contents() -> toc_screen::Contents {
    let entry = |title: &str, depth: usize, index: usize| toc_screen::Entry {
        title: title.into(),
        depth,
        index,
        page: Some(index * 12),
    };
    toc_screen::Contents::new(
        vec![
            entry("One", 0, 0),
            entry("Two", 0, 1),
            entry("Two.1", 1, 2),
            entry("Two.2", 1, 3),
            entry("Three", 0, 4),
        ],
        Some(2),
    )
}

fn build(screen: Screen) -> (Ui, toc_screen::TocUi) {
    build_with(screen, contents())
}

/// The same screen, with a book of the given size.
fn build_with(screen: Screen, contents: toc_screen::Contents) -> (Ui, toc_screen::TocUi) {
    let st = Styles::new(FamilyId(0)).with_density(screen.density);
    let mut tree = Tree::new();
    let l = toc_screen::build(&mut tree, &st, &contents, "Contents", "No contents.");
    tree.set_root(l.root);
    let mut ui = Ui::with_screen(tree, engine(), screen);
    ui.layout();
    l.sync(&mut ui.tree);
    ui.layout();
    (ui, l)
}

/// The node with the given action, if the tree has one.
fn node_with_action(tree: &Tree, action: u32) -> Option<NodeId> {
    fn walk(tree: &Tree, id: NodeId, action: u32) -> Option<NodeId> {
        if tree.node(id).action == Some(action) {
            return Some(id);
        }
        for &child in &tree.node(id).children {
            if let Some(found) = walk(tree, child, action) {
                return Some(found);
            }
        }
        None
    }
    tree.root().and_then(|root| walk(tree, root, action))
}

/// The ink of an icon.
fn icon_gray(tree: &Tree, id: NodeId) -> u8 {
    match tree.node(id).kind {
        Kind::Icon { gray, .. } => gray,
        _ => panic!("the node is not an icon"),
    }
}

/// The middle of a node's box.
fn centre(tree: &Tree, id: NodeId) -> Point {
    let r = tree.rect(id);
    Point {
        x: r.x + r.w / 2,
        y: r.y + r.h / 2,
    }
}

#[test]
fn the_fold_marker_folds_and_never_jumps() {
    for (w, h, dpi) in PANELS {
        let screen = Screen::new(w, h, dpi).expect("a panel");
        let (ui, _l) = build(screen);
        // The second chapter is the group that is open: its marker folds
        // it shut, so it is the second row on the screen.
        let marker = node_with_action(&ui.tree, toc_screen::action::TOGGLE_BASE + 1)
            .expect("the open group has a fold marker");
        let row = node_with_action(&ui.tree, toc_screen::action::ITEM_BASE + 1)
            .expect("the chapter is a row of its own");
        // A tap on the marker hits the marker, not the chapter: folding
        // never jumps to the chapter by mistake.
        assert_eq!(ui.tree.hit(centre(&ui.tree, marker)), Hit::Target(marker));
        assert_ne!(ui.tree.hit(centre(&ui.tree, marker)), Hit::Target(row));
        // The chapter itself is still a tap that jumps.
        assert_eq!(ui.tree.hit(centre(&ui.tree, row)), Hit::Target(row));
    }
}

#[test]
fn every_row_jumps_to_its_own_chapter() {
    for (w, h, dpi) in PANELS {
        let screen = Screen::new(w, h, dpi).expect("a panel");
        let (ui, _l) = build(screen);
        let contents = contents();
        for row in 0..contents.rows().len() {
            let node = node_with_action(&ui.tree, toc_screen::action::ITEM_BASE + row as u32)
                .unwrap_or_else(|| panic!("row {row} is on the screen"));
            assert_eq!(
                ui.tree.node(node).action,
                Some(toc_screen::action::ITEM_BASE + row as u32)
            );
        }
    }
}

/// A book with enough chapters for the list to run over several pages.
fn long_book() -> toc_screen::Contents {
    let mut entries = Vec::new();
    for i in 0..40 {
        entries.push(toc_screen::Entry {
            title: format!("Chapter {i}"),
            depth: 0,
            index: i,
            page: Some(i * 12),
        });
        entries.push(toc_screen::Entry {
            title: format!("Section {i}"),
            depth: 1,
            index: 100 + i,
            page: Some(i * 12 + 3),
        });
    }
    toc_screen::Contents::new(entries, Some(100))
}

#[test]
fn the_pager_counts_the_pages_and_the_ends_are_grey() {
    for (w, h, dpi) in PANELS {
        let screen = Screen::new(w, h, dpi).expect("a panel");
        let (mut ui, l) = build_with(screen, long_book());
        let (page, count) = ui.tree.page(l.pages);
        assert_eq!(page, 0);
        assert!(count > 1, "the sample book has more than one page");
        // The pager says which page is shown.
        let label = ui.tree.node(l.pager);
        let text: String = match &label.kind {
            Kind::Text { para, .. } => para.spans.iter().map(|s| s.text.as_str()).collect(),
            _ => panic!("the pager is a label"),
        };
        assert_eq!(text, format!("1 / {count}"));
        // On the first page the page before has nowhere to go.
        assert_eq!(icon_gray(&ui.tree, l.prev_icon), RULE);
        assert_eq!(icon_gray(&ui.tree, l.next_icon), INK);

        // On the last page it is the other way round.
        ui.tree.set_page(l.pages, count - 1);
        ui.layout();
        l.sync(&mut ui.tree);
        ui.layout();
        assert_eq!(icon_gray(&ui.tree, l.next_icon), RULE);
        assert_eq!(icon_gray(&ui.tree, l.prev_icon), INK);
    }
}

#[test]
fn the_page_buttons_stay_on_the_panel_and_off_each_other() {
    for (w, h, dpi) in PANELS {
        let screen = Screen::new(w, h, dpi).expect("a panel");
        let (ui, l) = build(screen);
        let panel = Rect::new(0, 0, screen.width as i32, screen.height as i32);
        let prev = ui.tree.rect(l.prev);
        let next = ui.tree.rect(l.next);
        let pager = ui.tree.rect(l.pager);
        assert!(
            panel.contains_rect(&prev),
            "{w}x{h}: the page before is on the panel"
        );
        assert!(
            panel.contains_rect(&next),
            "{w}x{h}: the page after is on the panel"
        );
        assert!(!prev.intersects(&next), "{w}x{h}: the page buttons overlap");
        assert!(
            !prev.intersects(&pager),
            "{w}x{h}: the pager is under the page before"
        );
        assert!(
            !next.intersects(&pager),
            "{w}x{h}: the pager is under the page after"
        );
    }
}

#[test]
fn the_page_buttons_stay_on_the_panel_for_a_long_multi_page_book() {
    for (w, h, dpi) in PANELS {
        let screen = Screen::new(w, h, dpi).expect("a panel");
        let (ui, l) = build_with(screen, long_book());
        let (_page, count) = ui.tree.page(l.pages);
        assert!(count > 1, "{w}x{h}: long book must paginate");
        let panel = Rect::new(0, 0, screen.width as i32, screen.height as i32);
        let prev = ui.tree.rect(l.prev);
        let next = ui.tree.rect(l.next);
        let pager = ui.tree.rect(l.pager);
        assert!(
            panel.contains_rect(&prev) && prev.h > 0,
            "{w}x{h}: prev button {prev:?} must stay visible on panel {panel:?}"
        );
        assert!(
            panel.contains_rect(&next) && next.h > 0,
            "{w}x{h}: next button {next:?} must stay visible on panel {panel:?}"
        );
        assert!(
            panel.contains_rect(&pager) && pager.h > 0,
            "{w}x{h}: pager {pager:?} must stay visible on panel {panel:?}"
        );
    }
}
