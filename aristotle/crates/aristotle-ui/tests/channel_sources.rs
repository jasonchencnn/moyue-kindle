// SPDX-License-Identifier: AGPL-3.0-or-later
//! Automated QA verification for multi-channel library sources, cover badges,
//! per-channel reader sync buttons, and Settings source toggles across 6-inch
//! (600x800, 758x1024) and KO2 (1264x1680) screens.

use aristotle_i18n::{tr, with_language, Msg};
use aristotle_ui::geom::{Density, Rect, Size};
use aristotle_ui::screens::library::{
    action as lib_action, Book, BookSource, CoverMode, Filter, Item, Library, LibraryLayout, Place,
    Shelf, StatusLine, View,
};
use aristotle_ui::screens::reader::{
    action as reader_action, build_menu_for_channels, MenuSpec, Settings as ReaderSettings,
};
use aristotle_ui::screens::settings::{action as set_action, Settings, SystemView};
use aristotle_ui::text::{FamilyId, FontData, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{ActionId, NodeId, Tree};

fn text_engine(_density: Density) -> TextEngine {
    let mut fonts = Fonts::embedded();
    if let Some(dir) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
        let path = std::path::PathBuf::from(dir).join("fonts/DejaVuSans-Symbols.ttf");
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(face) = fonts.add_face(FontData::Shared(std::sync::Arc::new(bytes))) {
                if let Some(family) =
                    fonts.add_family("Noto Sans SC", [Some(face), None, None, None])
                {
                    fonts.add_fallback(family);
                }
            }
        }
    }
    TextEngine::new(fonts)
}

fn find_actions(tree: &Tree) -> Vec<(ActionId, Rect)> {
    let mut out = Vec::new();
    fn walk(tree: &Tree, id: NodeId, out: &mut Vec<(ActionId, Rect)>) {
        let node = tree.node(id);
        if node.hidden {
            return;
        }
        if let Some(act) = node.action {
            let r = tree.rect(id);
            if r.w > 0 && r.h > 0 {
                out.push((act, r));
            }
        }
        for &child in &node.children {
            walk(tree, child, out);
        }
    }
    if let Some(root) = tree.root() {
        walk(tree, root, &mut out);
    }
    out
}

fn assert_disjoint_and_inside(label: &str, size: Size, items: &[(ActionId, Rect)]) {
    for (act, r) in items {
        assert!(
            r.x >= 0 && r.y >= 0 && r.right() <= size.w && r.bottom() <= size.h,
            "{label}: action {act} out of bounds {r:?} in {size:?}"
        );
    }
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            let (a1, r1) = items[i];
            let (a2, r2) = items[j];
            let overlap_x = r1.x < r2.right() && r2.x < r1.right();
            let overlap_y = r1.y < r2.bottom() && r2.y < r1.bottom();
            assert!(
                !(overlap_x && overlap_y),
                "{label}: actions {a1} ({r1:?}) and {a2} ({r2:?}) overlap"
            );
        }
    }
}

#[test]
fn book_sources_have_distinct_localized_labels() {
    with_language("zh", || {
        assert_eq!(BookSource::Local.label(), "本地");
        assert_eq!(BookSource::Readest.label(), "Readest");
        assert_eq!(BookSource::WeRead.label(), "WeRead");
        assert_eq!(BookSource::Jianguoyun.label(), "坚果云");
    });
    with_language("en", || {
        assert_eq!(BookSource::Local.label(), "Local");
        assert_eq!(BookSource::Readest.label(), "Readest");
        assert_eq!(BookSource::WeRead.label(), "WeRead");
        assert_eq!(BookSource::Jianguoyun.label(), "Nutstore");
    });
}

#[test]
fn library_segments_follow_source_switches_across_screen_sizes() {
    with_language("zh", || {
        let items = vec![
            Item::Book(Book::new("本地书", "作者", 0.2).with_source(BookSource::Local)),
            Item::Book(Book::new("云端书", "作者", 0.5).with_source(BookSource::Readest)),
            Item::Book(Book::new("微信书", "作者", 0.8).with_source(BookSource::WeRead)),
        ];
        let shelf = Shelf {
            place: Place::Home {
                view: View::Books,
                filter: Filter::All,
            },
            hero: None,
            items: &items,
            sort: tr(Msg::LibrarySortRecent),
            status: StatusLine {
                clock: "14:40",
                battery: Some(0.9),
                charging: false,
            },
            covers: CoverMode::Cards,
            layout: LibraryLayout::Grid,
            local_books_count: items.len(),
        };

        for (size, dpi) in [
            (Size::new(600, 800), 167),
            (Size::new(758, 1024), 212),
            (Size::new(1264, 1680), 300),
        ] {
            let density = Density::new(dpi).unwrap();
            let styles = Styles::new(FamilyId(0)).with_density(density);
            for (show_cloud, show_weread) in
                [(true, true), (true, false), (false, true), (false, false)]
            {
                let mut text = text_engine(density);
                let (mut tree, _view) = Library::build_shelf_with_sources(
                    &mut text,
                    &styles,
                    &shelf,
                    size,
                    show_cloud,
                    show_weread,
                );
                tree.layout(Rect::new(0, 0, size.w, size.h), &mut text);
                let actions = find_actions(&tree);
                assert_eq!(
                    actions.iter().any(|(a, _)| *a == lib_action::CLOUD_LIBRARY),
                    show_cloud,
                    "cloud tab mismatch for ({show_cloud}, {show_weread})"
                );
                assert_eq!(
                    actions
                        .iter()
                        .any(|(a, _)| *a == lib_action::WEREAD_LIBRARY),
                    show_weread,
                    "weread tab mismatch for ({show_cloud}, {show_weread})"
                );
            }
        }
    });
}

#[test]
fn empty_library_scan_screen_matches_enabled_cloud_sources() {
    use aristotle_ui::screens::scan::{action as scan_action, ScanScreen, ScanState, ScanUi};

    with_language("zh", || {
        let screen = ScanScreen {
            status: StatusLine {
                clock: "15:40",
                battery: Some(0.9),
                charging: false,
            },
            state: ScanState::Empty,
            local_books_count: 0,
        };
        for (size, dpi) in [(Size::new(758, 1024), 212), (Size::new(1264, 1680), 300)] {
            let density = Density::new(dpi).unwrap();
            let styles = Styles::new(FamilyId(0)).with_density(density);
            for (show_cloud, show_weread) in
                [(true, true), (true, false), (false, true), (false, false)]
            {
                let mut text = text_engine(density);
                let (mut tree, _) = ScanUi::build_with_sources(
                    &mut text,
                    &styles,
                    &screen,
                    show_cloud,
                    show_weread,
                );
                tree.layout(Rect::new(0, 0, size.w, size.h), &mut text);
                let actions = find_actions(&tree);
                assert_eq!(
                    actions
                        .iter()
                        .any(|(a, _)| *a == scan_action::CLOUD_LIBRARY),
                    show_cloud,
                    "empty-library cloud tab mismatch for ({show_cloud}, {show_weread})"
                );
                assert_eq!(
                    actions
                        .iter()
                        .any(|(a, _)| *a == scan_action::WEREAD_LIBRARY),
                    show_weread,
                    "empty-library weread tab mismatch for ({show_cloud}, {show_weread})"
                );
            }
        }
    });
}

#[test]
fn reader_menu_separates_readest_and_weread_without_overlap() {
    with_language("zh", || {
        let spec = MenuSpec {
            title: "赘婿",
            families: None,
            font_list: true,
            night: true,
            sync: true,
            top_extra: 0,
            bottom_extra: 0,
        };
        let settings = ReaderSettings::default();

        for (size, dpi) in [
            (Size::new(600, 800), 167),
            (Size::new(758, 1024), 212),
            (Size::new(1264, 1680), 300),
        ] {
            let density = Density::new(dpi).unwrap();
            let styles = Styles::new(FamilyId(0)).with_density(density);
            for (sync_readest, sync_weread) in
                [(true, true), (true, false), (false, true), (false, false)]
            {
                let mut text = text_engine(density);
                let mut tree = Tree::new();
                let menu = build_menu_for_channels(
                    &mut tree,
                    &styles,
                    &spec,
                    &settings,
                    sync_readest,
                    sync_weread,
                );
                tree.set_root(menu.root);
                tree.set_hidden(menu.root, false);
                tree.layout(Rect::new(0, 0, size.w, size.h), &mut text);

                assert_eq!(menu.btn_pull.is_some(), sync_readest);
                assert_eq!(menu.btn_push.is_some(), sync_readest);
                assert_eq!(menu.btn_weread_pull.is_some(), sync_weread);
                assert_eq!(menu.btn_weread_push.is_some(), sync_weread);

                let actions = find_actions(&tree);
                assert_eq!(
                    actions.iter().any(|(a, _)| *a == reader_action::PULL_SYNC),
                    sync_readest
                );
                assert_eq!(
                    actions.iter().any(|(a, _)| *a == reader_action::PUSH_SYNC),
                    sync_readest
                );
                assert_eq!(
                    actions
                        .iter()
                        .any(|(a, _)| *a == reader_action::WEREAD_PULL_SYNC),
                    sync_weread
                );
                assert_eq!(
                    actions
                        .iter()
                        .any(|(a, _)| *a == reader_action::WEREAD_PUSH_SYNC),
                    sync_weread
                );
                let sync_btns: Vec<(ActionId, Rect)> = actions
                    .into_iter()
                    .filter(|(a, _)| {
                        matches!(
                            *a,
                            reader_action::PULL_SYNC
                                | reader_action::PUSH_SYNC
                                | reader_action::WEREAD_PULL_SYNC
                                | reader_action::WEREAD_PUSH_SYNC
                        )
                    })
                    .collect();
                assert_disjoint_and_inside(
                    &format!("reader_menu({sync_readest},{sync_weread})"),
                    size,
                    &sync_btns,
                );
            }
        }
    });
}

#[test]
fn settings_exposes_readest_and_weread_source_switches() {
    with_language("zh", || {
        let view = SystemView {
            status: StatusLine {
                clock: "14:40",
                battery: Some(0.9),
                charging: false,
            },
            light: Some((12, 24)),
            night: false,
            covers: CoverMode::Photos,
            sleep_minutes: Some(15),
            sleep_screen: 0,
            sleep_screen_cover: false,
            clippings: false,
            battery: None,
            storage: None,
            usb: None,
            sync: None,
            catalogs: 0,
            exit_message: None,
            clock_style: 0,
            home_stats: true,
            home_quote: true,
            home_pet: true,
            font: "Literata",
            refresh_interval: 0,
            wifi_transfer: "off",
            account: None,
            enable_readest: true,
            enable_weread: false,
            enable_jianguoyun: false,
            jianguoyun_account: None,
            weread_readtime: false,
            weread_readtime_note: None,
            weread_notes: None,
        };
        for (size, dpi) in [
            (Size::new(600, 800), 167),
            (Size::new(758, 1024), 212),
            (Size::new(1264, 1680), 300),
        ] {
            let density = Density::new(dpi).unwrap();
            let styles = Styles::new(FamilyId(0)).with_density(density);
            let mut text = text_engine(density);
            let (mut tree, ids) = Settings::build(&mut text, &styles, &view);
            let bounds = Rect::new(0, 0, size.w, size.h);
            tree.layout(bounds, &mut text);
            let page_count = tree.page(ids.list).1;
            let mut found_readest = false;
            let mut found_weread = false;
            let mut found_jianguoyun = false;
            let mut found_jgy_account = false;
            for page in 0..page_count {
                tree.set_page(ids.list, page);
                tree.layout(bounds, &mut text);
                let actions = find_actions(&tree);
                if actions
                    .iter()
                    .any(|(a, _)| *a == set_action::ENABLE_READEST)
                {
                    found_readest = true;
                }
                if actions.iter().any(|(a, _)| *a == set_action::ENABLE_WEREAD) {
                    found_weread = true;
                }
                if actions
                    .iter()
                    .any(|(a, _)| *a == set_action::ENABLE_JIANGUOYUN)
                {
                    found_jianguoyun = true;
                }
                if actions
                    .iter()
                    .any(|(a, _)| *a == set_action::JIANGUOYUN_ACCOUNT)
                {
                    found_jgy_account = true;
                }
                if page >= 3 {
                    assert_disjoint_and_inside(
                        &format!("settings_p{}({size:?})", page + 1),
                        size,
                        &actions,
                    );
                }
            }
            assert!(found_readest, "ENABLE_READEST missing at {size:?}");
            assert!(found_weread, "ENABLE_WEREAD missing at {size:?}");
            assert!(found_jianguoyun, "ENABLE_JIANGUOYUN missing at {size:?}");
            assert!(found_jgy_account, "JIANGUOYUN_ACCOUNT missing at {size:?}");
        }
    });
}

#[test]
fn cjk_typographic_cover_paints_source_badge_pixels() {
    use aristotle_ui::geom::Screen;
    use aristotle_ui::ui::Ui;
    use aristotle_ui::widgets::Cover;

    with_language("zh", || {
        let profile = Screen {
            width: 300,
            height: 400,
            density: Density::new(300).unwrap(),
        };
        let render_cover = |tag: Option<&str>| {
            let text = text_engine(profile.density);
            let mut tree = Tree::new();
            let mut cover = Cover::new("呐喊", "鲁迅", FamilyId(0));
            if let Some(t) = tag {
                cover = cover.with_source_tag(t);
            }
            let node = tree.custom(cover);
            let boxed = tree.fixed(Some(240), Some(320), Some(node));
            tree.set_root(boxed);
            let mut ui = Ui::with_screen(tree, text, profile);
            ui.layout();
            ui.render();
            ui.screen().pixels.clone()
        };

        let plain = render_cover(None);
        let with_weread = render_cover(Some(BookSource::WeRead.label()));
        let with_readest = render_cover(Some(BookSource::Readest.label()));
        let with_jianguoyun = render_cover(Some(BookSource::Jianguoyun.label()));
        // The corner source pill must actually paint onto CJK typographic covers.
        let diff_weread = plain
            .iter()
            .zip(&with_weread)
            .filter(|(a, b)| a != b)
            .count();
        let diff_readest = plain
            .iter()
            .zip(&with_readest)
            .filter(|(a, b)| a != b)
            .count();
        let diff_jianguoyun = plain
            .iter()
            .zip(&with_jianguoyun)
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            diff_weread > 400,
            "expected WeRead source badge pixels on CJK cover, got diff={diff_weread}"
        );
        assert!(
            diff_readest > 400,
            "expected Readest source badge pixels on CJK cover, got diff={diff_readest}"
        );
        assert!(
            diff_jianguoyun > 400,
            "expected Jianguoyun source badge pixels on CJK cover, got diff={diff_jianguoyun}"
        );
    });
}

#[test]
fn four_channel_library_scan_reader_and_settings_cover_all_combinations() {
    use aristotle_ui::screens::library::ChannelVisibility;
    use aristotle_ui::screens::reader::{build_menu_for_sync_channels, ReaderSyncChannels};
    use aristotle_ui::screens::scan::{action as scan_action, ScanScreen, ScanState, ScanUi};

    for lang in ["zh", "en"] {
        with_language(lang, || {
            let items = vec![
                Item::Book(Book::new("本地书", "作者", 0.2).with_source(BookSource::Local)),
                Item::Book(Book::new("云端书", "作者", 0.5).with_source(BookSource::Readest)),
                Item::Book(Book::new("微信书", "作者", 0.8).with_source(BookSource::WeRead)),
                Item::Book(Book::new("坚果书", "作者", 0.6).with_source(BookSource::Jianguoyun)),
            ];
            let shelf = Shelf {
                place: Place::Home {
                    view: View::Books,
                    filter: Filter::All,
                },
                hero: None,
                items: &items,
                sort: tr(Msg::LibrarySortRecent),
                status: StatusLine {
                    clock: "14:40",
                    battery: Some(0.9),
                    charging: false,
                },
                covers: CoverMode::Cards,
                layout: LibraryLayout::Grid,
                local_books_count: items.len(),
            };
            let scan_screen = ScanScreen {
                status: StatusLine {
                    clock: "15:40",
                    battery: Some(0.9),
                    charging: false,
                },
                state: ScanState::Empty,
                local_books_count: 0,
            };
            let spec = MenuSpec {
                title: "三体",
                families: None,
                font_list: true,
                night: true,
                sync: true,
                top_extra: 0,
                bottom_extra: 0,
            };
            let reader_settings = ReaderSettings::default();

            for (size, dpi) in [
                (Size::new(600, 800), 167),
                (Size::new(758, 1024), 212),
                (Size::new(1264, 1680), 300),
            ] {
                let density = Density::new(dpi).unwrap();
                let styles = Styles::new(FamilyId(0)).with_density(density);

                for readest in [false, true] {
                    for weread in [false, true] {
                        for jianguoyun in [false, true] {
                            let vis = ChannelVisibility {
                                readest,
                                weread,
                                jianguoyun,
                            };
                            let mut text = text_engine(density);
                            let (mut tree, _) = Library::build_shelf_with_channels(
                                &mut text, &styles, &shelf, size, vis,
                            );
                            tree.layout(Rect::new(0, 0, size.w, size.h), &mut text);
                            let actions = find_actions(&tree);
                            assert_eq!(
                                actions.iter().any(|(a, _)| *a == lib_action::CLOUD_LIBRARY),
                                readest
                            );
                            assert_eq!(
                                actions
                                    .iter()
                                    .any(|(a, _)| *a == lib_action::WEREAD_LIBRARY),
                                weread
                            );
                            assert_eq!(
                                actions
                                    .iter()
                                    .any(|(a, _)| *a == lib_action::JIANGUOYUN_LIBRARY),
                                jianguoyun
                            );

                            let mut text = text_engine(density);
                            let (mut scan_tree, _) =
                                ScanUi::build_with_channels(&mut text, &styles, &scan_screen, vis);
                            scan_tree.layout(Rect::new(0, 0, size.w, size.h), &mut text);
                            let scan_actions = find_actions(&scan_tree);
                            assert_eq!(
                                scan_actions
                                    .iter()
                                    .any(|(a, _)| *a == scan_action::JIANGUOYUN_LIBRARY),
                                jianguoyun
                            );

                            let sync_channels = ReaderSyncChannels {
                                readest,
                                weread,
                                jianguoyun,
                            };
                            let mut text = text_engine(density);
                            let mut menu_tree = Tree::new();
                            let menu = build_menu_for_sync_channels(
                                &mut menu_tree,
                                &styles,
                                &spec,
                                &reader_settings,
                                sync_channels,
                            );
                            menu_tree.set_root(menu.root);
                            menu_tree.set_hidden(menu.root, false);
                            menu_tree.layout(Rect::new(0, 0, size.w, size.h), &mut text);
                            assert_eq!(menu.btn_jianguoyun_pull.is_some(), jianguoyun);
                            assert_eq!(menu.btn_jianguoyun_push.is_some(), jianguoyun);
                            let menu_actions: Vec<(ActionId, Rect)> = find_actions(&menu_tree)
                                .into_iter()
                                .filter(|(a, _)| {
                                    matches!(
                                        *a,
                                        reader_action::PULL_SYNC
                                            | reader_action::PUSH_SYNC
                                            | reader_action::WEREAD_PULL_SYNC
                                            | reader_action::WEREAD_PUSH_SYNC
                                            | reader_action::JIANGUOYUN_PULL_SYNC
                                            | reader_action::JIANGUOYUN_PUSH_SYNC
                                    )
                                })
                                .collect();
                            assert_disjoint_and_inside(
                                &format!("reader_sync_3ch({readest},{weread},{jianguoyun})"),
                                size,
                                &menu_actions,
                            );
                        }
                    }
                }
            }
        });
    }
}
