// SPDX-License-Identifier: AGPL-3.0-or-later

use aristotle_ui::geom::{Point, Screen, Size};
use aristotle_ui::screens::{library::*, mine};
use aristotle_ui::text::{FamilyId, FontData, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{Hit, NodeId};
use aristotle_ui::ui::Ui;

const STATUS: StatusLine<'static> = StatusLine {
    clock: "21:38",
    battery: Some(0.82),
    charging: false,
};

fn engine() -> TextEngine {
    let mut fonts = Fonts::embedded();
    if let Some(dir) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
        let path = std::path::PathBuf::from(dir).join("fonts/DejaVuSans-Symbols.ttf");
        let bytes = std::fs::read(path).expect("configured CJK font");
        let face = fonts
            .add_face(FontData::Shared(std::sync::Arc::new(bytes)))
            .unwrap();
        let family = fonts
            .add_family("Noto Sans SC", [Some(face), None, None, None])
            .unwrap();
        fonts.add_fallback(family);
    }
    TextEngine::new(fonts)
}

fn target(ui: &Ui, id: NodeId, min_height: i32) {
    let r = ui.tree.rect(id);
    assert!(ui.bounds().contains_rect(&r), "outside screen: {r:?}");
    assert!(r.h >= min_height, "short target: {r:?}");
    for y in [r.y + 1, r.y + r.h / 2, r.bottom() - 2] {
        assert!(
            matches!(ui.tree.hit(Point { x: r.x + r.w / 2, y }), Hit::Target(hit) if hit == id),
            "covered target: {id:?} {r:?}"
        );
    }
}

fn shot(ui: &mut Ui, name: &str) {
    let Some(dir) = std::env::var_os("UI_SIZE_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    ui.render();
    let mut canvas = ui.screen().clone();
    canvas.quantize16();
    let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
    let mut encoder = png::Encoder::new(file, canvas.width, canvas.height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&canvas.packed())
        .unwrap();
}

#[test]
fn panel_arrangements_keep_books_and_mine_actions_reachable() {
    for lang in ["en", "zh"] {
        assert!(aristotle_i18n::set_language(lang));
        for (name, w, h, dpi) in [
            ("pw4", 1072, 1448, 300),
            ("six-167dpi", 600, 800, 167),
            ("oasis2", 1264, 1680, 300),
            ("eleven-inch", 1980, 2640, 300),
        ] {
            let screen = Screen::new(w, h, dpi).unwrap();
            let size = Size::new(w as i32, h as i32);
            let styles = Styles::new(FamilyId(0)).with_density(screen.density);
            let mut text = engine();
            let (tree, mine_ui) =
                mine::MineUi::build_for_size(&mut text, &styles, &mine::sample(STATUS), size);
            let mut ui = Ui::with_screen(tree, text, screen);
            ui.layout();
            let goal_bar = (0..ui.tree.len())
                .map(|i| NodeId(i as u32))
                .find(|&id| {
                    ui.tree
                        .widget::<aristotle_ui::widgets::GoalBar>(id)
                        .is_some()
                })
                .unwrap();
            assert!(
                ui.tree.rect(goal_bar).h >= screen.density.px(10),
                "goal bar was squeezed"
            );
            for &id in &mine_ui.rows {
                target(&ui, id, screen.density.px(aristotle_ui::geom::MIN_TARGET));
            }
            for action in [
                mine::action::ACCOUNT,
                mine::action::RETURN_KINDLE,
                mine::action::OPEN_KUAL,
            ] {
                let id = (0..ui.tree.len())
                    .map(|i| NodeId(i as u32))
                    .find(|&id| ui.tree.node(id).action == Some(action))
                    .unwrap();
                target(&ui, id, screen.density.px(64));
            }
            shot(&mut ui, &format!("{name}-{lang}-mine"));
            let books = sample_books();
            for view in View::ALL {
                let items = sample_items(&books, view);
                for layout in [LibraryLayout::List, LibraryLayout::Grid] {
                    let shelf = Shelf {
                        place: Place::Home {
                            view,
                            filter: Filter::All,
                        },
                        hero: None,
                        items: &items,
                        sort: aristotle_i18n::tr(aristotle_i18n::Msg::LibrarySortRecent),
                        status: STATUS,
                        covers: CoverMode::Cards,
                        layout,
                        local_books_count: books.len(),
                    };
                    let mut text = engine();
                    let (tree, library) =
                        Library::build_shelf_for_size(&mut text, &styles, &shelf, size);
                    let mut ui = Ui::with_screen(tree, text, screen);
                    ui.layout();
                    let count = ui.tree.page(library.list).1;
                    let mut reached = 0;
                    for page in 0..count {
                        ui.tree.set_page(library.list, page);
                        ui.layout();
                        library.sync(&mut ui.tree);
                        for &id in &library.rows {
                            if !ui.tree.rect(id).is_empty() {
                                reached += 1;
                                let r = ui.tree.rect(id);
                                let list = ui.tree.rect(library.list);
                                assert!(list.contains_rect(&r), "{name} {view:?} {layout:?} overlaps pager: {r:?} outside {list:?}");
                                target(&ui, id, screen.density.px(64));
                            }
                        }
                        if page == 0 {
                            shot(&mut ui, &format!("{name}-{lang}-{view:?}-{layout:?}"));
                        }
                    }
                    assert_eq!(reached, library.rows.len(), "each item reachable once");
                }
            }
        }
    }
}
