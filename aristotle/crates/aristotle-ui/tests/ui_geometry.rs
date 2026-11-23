// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Host-side geometry tests for the narrow panels.
//!
//! Two rules hold on every screen, at every size:
//!
//! * two controls on one line never overlap, and
//! * no control sticks out of the box that holds it.
//!
//! The panels are the narrowest ones the app has to run on: the 6-inch
//! Kindle 10 (600 x 800 at 167 DPI) and the Paperwhite 2 (758 x 1024 at
//! 212 DPI). The reference panel (1264 x 1680 at 300 DPI) is checked as
//! well, so a fix for the narrow ones cannot quietly break the wide one.

use aristotle_i18n::{tr, Msg};
use aristotle_ui::geom::{Rect, Screen, Size};
use aristotle_ui::icons::Icon;
use aristotle_ui::screens::book_actions::{self as book_actions, ActionStep, BookActionInfo};
use aristotle_ui::screens::icon_button;
use aristotle_ui::screens::library::{
    self as library_screen, CoverMode, Filter, Item, LibraryLayout, Place, Shelf, StatusLine, View,
};
use aristotle_ui::screens::mine as mine_screen;
use aristotle_ui::screens::ota as ota_screen;
use aristotle_ui::screens::reader::{Reader, Settings};
use aristotle_ui::screens::selection as selection_screen;
use aristotle_ui::screens::settings::{self as settings_screen, BatteryInfo, SystemView, Usb};
use aristotle_ui::screens::sheet::{self, Button};
use aristotle_ui::screens::toc as toc_screen;
use aristotle_ui::screens::translate as translate_screen;
use aristotle_ui::screens::typeset::{self as typeset_screen, TypesetView};
use aristotle_ui::screens::weread_shelf as weread_screen;
use aristotle_ui::text::{Align, FamilyId, FontData, Fonts, Paragraph, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{pages, Kind, Node, NodeId, Tree};
use aristotle_ui::ui::Ui;

type Panel = (&'static str, u32, u32, u16);

/// The panels the narrow-screen rules are checked on.
const PANELS: [Panel; 4] = [
    ("kindle10-167", 600, 800, 167),
    ("pw2-212", 758, 1024, 212),
    ("pw4-300", 1072, 1448, 300),
    ("oasis2-300", 1264, 1680, 300),
];

/// The panels the task asks for: the two narrow ones, the 7-inch one and
/// the reference one.
const CHECKED: [&str; 4] = ["kindle10-167", "pw2-212", "pw4-300", "oasis2-300"];

const LANGUAGES: [&str; 3] = ["zh", "en", "pl"];

fn status() -> StatusLine<'static> {
    StatusLine {
        clock: "21:38",
        battery: Some(0.82),
        charging: false,
    }
}

/// A text engine with the UI font, plus the CJK font when the host has one.
fn engine() -> TextEngine {
    let mut fonts = Fonts::embedded();
    if let Some(dir) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
        let path = std::path::PathBuf::from(dir).join("fonts/DejaVuSans-Symbols.ttf");
        if let Ok(bytes) = std::fs::read(path) {
            let face = fonts
                .add_face(FontData::Shared(std::sync::Arc::new(bytes)))
                .expect("configured CJK font");
            let family = fonts
                .add_family("Noto Sans SC", [Some(face), None, None, None])
                .expect("configured CJK family");
            fonts.add_fallback(family);
        }
    }
    TextEngine::new(fonts)
}

fn styles(screen: Screen) -> Styles {
    Styles::new(FamilyId(0)).with_density(screen.density)
}

/// Every screen the rules are checked on, built for one panel.
fn screens(screen: Screen) -> Vec<(String, Ui)> {
    let size = Size::new(screen.width as i32, screen.height as i32);
    let st = styles(screen);
    let mut out: Vec<(String, Ui)> = Vec::new();

    // The reader typeset sheet.
    {
        let mut tree = Tree::new();
        let view = TypesetView::default();
        let mut text = engine();
        let (root, _) =
            typeset_screen::build_typeset_screen_for_size(&mut tree, &mut text, &st, &view, size);
        tree.set_root(root);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("typeset".into(), ui));
    }

    // The reader page itself.
    {
        let mut text = engine();
        let settings = Settings {
            light: 0.0,
            ..Settings::default()
        };
        let (tree, _r) = Reader::build_for_size(
            &mut text,
            &st,
            [FamilyId(0), FamilyId(0), FamilyId(0)],
            settings,
            size,
        );
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("reader".into(), ui));
    }

    // The "mine" page.
    {
        let mut text = engine();
        let mine = mine_screen::sample(status());
        let (tree, _ids) = mine_screen::MineUi::build_for_size(&mut text, &st, &mine, size);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("mine".into(), ui));
    }

    // The software update page, with a package on its way.
    {
        let mut text = engine();
        let ota = ota_screen::Ota {
            status: status(),
            current_version: "0.3.2".into(),
            remote_version: Some("0.3.3".into()),
            stage: ota_screen::Stage::Downloading,
            percent: 42,
            downloaded_bytes: 5_600_000,
            total_bytes: Some(13_500_000),
            message: None,
        };
        let (tree, _ids) = ota_screen::OtaUi::build_for_size(&mut text, &st, &ota, size);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("ota".into(), ui));
    }

    // The software update page, with the package ready to install.
    {
        let mut text = engine();
        let ota = ota_screen::Ota {
            status: status(),
            current_version: "0.3.2".into(),
            remote_version: Some("0.3.3".into()),
            stage: ota_screen::Stage::ReadyToApply,
            percent: 100,
            downloaded_bytes: 13_500_000,
            total_bytes: Some(13_500_000),
            message: None,
        };
        let (tree, _ids) = ota_screen::OtaUi::build_for_size(&mut text, &st, &ota, size);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("ota-ready".into(), ui));
    }

    // The software update page, after a failure.
    {
        let mut text = engine();
        let ota = ota_screen::Ota {
            status: status(),
            current_version: "0.3.2".into(),
            remote_version: Some("0.3.3".into()),
            stage: ota_screen::Stage::Failed,
            percent: 0,
            downloaded_bytes: 0,
            total_bytes: Some(13_500_000),
            message: Some("The package's SHA-256 is 0000, expected aaaa".into()),
        };
        let (tree, _ids) = ota_screen::OtaUi::build_for_size(&mut text, &st, &ota, size);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("ota-failed".into(), ui));
    }

    // The settings page.
    {
        let mut text = engine();
        let view = SystemView {
            status: status(),
            light: Some((8, 20)),
            night: false,
            covers: CoverMode::Cards,
            sleep_minutes: Some(15),
            sleep_screen: 0,
            sleep_screen_cover: false,
            clippings: true,
            battery: Some(BatteryInfo {
                level: 0.82,
                charging: false,
                state: "Discharging",
                voltage_mv: 3950,
                temperature_c: 31,
            }),
            storage: Some((2 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024)),
            usb: Some(Usb::Storage { minutes_left: 12 }),
            sync: Some("21:30"),
            catalogs: 3,
            exit_message: None,
            clock_style: 0,
            home_stats: true,
            home_quote: true,
            home_pet: true,
            font: "Noto Sans SC",
            refresh_interval: 15,
            wifi_transfer: "off",
            account: Some("reader@example.com"),
            enable_readest: true,
            enable_weread: true,
            enable_jianguoyun: true,
            jianguoyun_account: Some("anna@example.com"),
            weread_readtime: false,
            weread_readtime_note: None,
            weread_notes: Some((2, 12, 4)),
        };
        let (tree, _ids) = settings_screen::Settings::build(&mut text, &st, &view);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("settings".into(), ui));
    }

    // The library shelf, in its list layout.
    {
        let mut text = engine();
        let books = library_screen::sample_books();
        let items: Vec<Item> = books.iter().cloned().map(Item::Book).collect();
        let shelf = Shelf {
            place: Place::Home {
                view: View::Books,
                filter: Filter::All,
            },
            hero: None,
            items: &items,
            sort: "Recently opened",
            status: status(),
            covers: CoverMode::Cards,
            layout: LibraryLayout::List,
            local_books_count: books.len(),
        };
        let (tree, _ids) =
            library_screen::Library::build_shelf_for_size(&mut text, &st, &shelf, size);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("library".into(), ui));
    }

    // The WeRead shelf.
    {
        let mut text = engine();
        let books = vec![weread_book("Silent People", "EPUB", 0.43)];
        let view = weread_screen::WereadShelfView {
            status: status(),
            signed_in: true,
            book_count: books.len(),
            local_books_count: 128,
            last_sync: Some("21:30"),
            filter: weread_screen::WereadFilter::All,
            loading: false,
            message: None,
            page: 0,
            books: &books,
        };
        let (tree, shelf) =
            weread_screen::WereadShelfView::build_for_width(&mut text, &st, &view, size.w);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        shelf.sync(&mut ui.tree);
        out.push(("weread-shelf".into(), ui));
    }

    // The book actions sheet.
    {
        let mut tree = Tree::new();
        let info = BookActionInfo {
            id: "sample".into(),
            title: "Dream of the Red Chamber".into(),
            author: "Cao Xueqin".into(),
            path: "文学/红楼梦.epub".into(),
            full_path: "/mnt/us/books/dream.epub".into(),
            format: "EPUB".into(),
            size: "2.4 MB".into(),
            progress: 0.43,
            finished: false,
            unread: false,
            read_secs: 3600,
            start_date: Some("2026-11-01".into()),
            cover: None,
        };
        let sheet = book_actions::build_book_actions(&mut tree, &st, &info, ActionStep::Menu);
        tree.set_root(sheet.root);
        let mut ui = Ui::with_screen(tree, engine(), screen);
        ui.layout();
        out.push(("book-actions".into(), ui));
    }

    // The sheet of actions under a selection.
    {
        let mut tree = Tree::new();
        let mut text = engine();
        let st = styles(screen);
        let size = Size::new(screen.width as i32, screen.height as i32);
        let available = sheet::available_width(&st, size.w);
        let items = [
            (tr(Msg::ReaderHighlight), 1003),
            (tr(Msg::ReaderDictionary), 1005),
            (tr(Msg::ReaderGloss), 1006),
            (tr(Msg::ReaderTranslate), 1012),
            (tr(Msg::ReaderTranslateSentence), 1013),
            (tr(Msg::CommonCancel), 1008),
        ];
        let buttons: Vec<Button> = items
            .iter()
            .map(|(label, action)| Button::new(&mut text, &st, label, *action))
            .collect();
        let s = selection_screen::build(
            &mut tree,
            &st,
            tr(Msg::ReaderSelection),
            &buttons,
            available,
            sheet::Place::Bottom,
        );
        tree.set_root(s.root);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("selection".into(), ui));
    }

    // The translation sheet, with a translation in it.
    {
        let mut tree = Tree::new();
        let mut text = engine();
        let st = styles(screen);
        let size = Size::new(screen.width as i32, screen.height as i32);
        let available = sheet::available_width(&st, size.w);
        let close = Button::new(&mut text, &st, tr(Msg::CommonClose), 1008);
        let view = translate_screen::TranslateView {
            title: tr(Msg::ReaderTranslateTitle),
            original_label: tr(Msg::ReaderTranslateOriginal),
            original: "The quick brown fox jumps over the lazy dog.",
            translation: Some("敏捷的棕色狐狸跳过懒狗。"),
            origin: Some("Edge"),
            status: None,
            buttons: vec![close],
        };
        let sheet = translate_screen::build(
            &mut tree,
            &st,
            &view,
            available,
            sheet::Place::Bottom,
            size.h / 3,
        );
        tree.set_root(sheet.root);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("translate".into(), ui));
    }

    // The dictionary popup: five buttons and the pager.
    {
        let mut tree = Tree::new();
        let mut text = engine();
        let st = styles(screen);
        let size = Size::new(screen.width as i32, screen.height as i32);
        let available = sheet::available_width(&st, size.w);
        let items = [
            (tr(Msg::ReaderUnhighlight), 1004),
            (tr(Msg::CommonMore), 1010),
            (tr(Msg::ReaderTranslate), 1012),
            (tr(Msg::ReaderTranslateSentence), 1013),
            (tr(Msg::CommonClose), 1008),
        ];
        let buttons: Vec<Button> = items
            .iter()
            .map(|(label, action)| Button::new(&mut text, &st, label, *action))
            .collect();
        let prev = icon_button(&mut tree, Icon::ChevronLeft, 40, 1001);
        let pager = tree.label("", st.small());
        let next = icon_button(&mut tree, Icon::ChevronRight, 40, 1002);
        let foot = sheet::foot(&mut tree, &st, &buttons, available, vec![prev, pager, next]);
        let mut para = Paragraph::plain("mori".to_string(), st.title(), Align::Left);
        para.style.max_lines = Some(1);
        let title = tree.text(para);
        let title = tree.add(Node::column(0).flex(1), [title]);
        let head = tree.add(Node::row(10), [title]);
        let head = tree.fixed(None, Some(aristotle_ui::geom::MIN_TARGET), Some(head));
        let sense = tree.label("The root of the tree.", st.item_regular());
        let body = tree.add(pages(8), [sense]);
        let content = sheet::content(&mut tree, vec![head, body, foot]);
        let content = tree.add(Node::column(0).flex(1), [content]);
        let s = sheet::layer(&mut tree, content, sheet::Place::Bottom, Some(size.h / 3));
        tree.set_root(s.root);
        let mut ui = Ui::with_screen(tree, text, screen);
        ui.layout();
        out.push(("dict-popup".into(), ui));
    }

    // The table of contents, with one group open.
    {
        let mut tree = Tree::new();
        let contents = toc_contents();
        let l = toc_screen::build(&mut tree, &st, &contents, "目录", "这本书没有目录。");
        tree.set_root(l.root);
        let mut ui = Ui::with_screen(tree, engine(), screen);
        ui.layout();
        l.sync(&mut ui.tree);
        ui.layout();
        out.push(("toc".into(), ui));
    }

    out
}

/// A book with three chapters, the second one holding two sections: the
/// reader is in one of the sections, so that group is open.
fn toc_contents() -> toc_screen::Contents {
    let entry = |title: &str, depth: usize, index: usize| toc_screen::Entry {
        title: title.into(),
        depth,
        index,
        page: Some(index * 12),
    };
    toc_screen::Contents::new(
        vec![
            entry("第一章 起始", 0, 0),
            entry("第二章 展开的组", 0, 1),
            entry("第二节 子目录", 1, 2),
            entry("第三节 子目录", 1, 3),
            entry("第三章 折叠的组", 0, 4),
        ],
        Some(2),
    )
}

fn weread_book(title: &str, format: &str, progress: f64) -> weread_screen::WereadBook {
    weread_screen::WereadBook {
        id: title.to_string(),
        title: title.to_string(),
        format: format.to_string(),
        progress: Some((progress * 100.0) as i64),
        finished: false,
        archive: false,
        secret: false,
        download: weread_screen::WereadDownloadState::NotDownloaded,
    }
}

// -----------------------------------------------------------------------------
// The checks
// -----------------------------------------------------------------------------

/// What a node is called in a finding.
fn name(tree: &Tree, id: NodeId) -> String {
    let node = tree.node(id);
    let mut out = match &node.kind {
        Kind::Text { para, .. } => {
            let text: String = para.spans.iter().map(|s| s.text.as_str()).collect();
            let text = text.trim().replace(['\n', '\t'], " ");
            if text.is_empty() {
                "text".to_string()
            } else {
                format!("text {text:?}")
            }
        }
        Kind::Icon { icon, .. } => format!("icon {icon:?}"),
        _ => "box".to_string(),
    };
    if let Some(action) = node.action {
        out.push_str(&format!(" action={action}"));
    }
    out
}

/// True when two rectangles stand on the same line: their vertical spans
/// overlap by more than half of the shorter one.
fn same_line(a: &Rect, b: &Rect) -> bool {
    let top = a.y.max(b.y);
    let bottom = a.bottom().min(b.bottom());
    if bottom <= top {
        return false;
    }
    let overlap = bottom - top;
    let shorter = a.h.min(b.h).max(1);
    overlap * 2 >= shorter
}

/// The nodes that are controls or labels: the things a finger has to hit and
/// the eye has to read.
fn is_control(tree: &Tree, id: NodeId) -> bool {
    let node = tree.node(id);
    node.action.is_some() || matches!(node.kind, Kind::Text { .. })
}

/// Findings for one screen: overlapping controls on one line, and controls
/// that stick out of the box that holds them.
fn findings(ui: &Ui, screen: &str) -> Vec<String> {
    let tree = &ui.tree;
    let mut out = Vec::new();
    let Some(root) = tree.root() else {
        return out;
    };

    // Overlapping siblings.
    fn walk(tree: &Tree, id: NodeId, screen: &str, out: &mut Vec<String>) {
        let node = tree.node(id);
        // A stack puts its children on top of each other on purpose.
        let stacked = matches!(node.kind, Kind::Stack);
        let children: Vec<NodeId> = node
            .children
            .iter()
            .copied()
            .filter(|&c| tree.is_visible(c))
            .collect();
        if !stacked {
            for (i, &a) in children.iter().enumerate() {
                for &b in children.iter().skip(i + 1) {
                    if !is_control(tree, a) || !is_control(tree, b) {
                        continue;
                    }
                    let ra = tree.rect(a);
                    let rb = tree.rect(b);
                    if ra.is_empty() || rb.is_empty() {
                        continue;
                    }
                    if !same_line(&ra, &rb) {
                        continue;
                    }
                    if ra.intersects(&rb) {
                        out.push(format!(
                            "{screen}\toverlap\t{} {ra:?} and {} {rb:?}",
                            name(tree, a),
                            name(tree, b)
                        ));
                    }
                }
            }
        }
        for &child in &children {
            walk(tree, child, screen, out);
        }
    }
    walk(tree, root, screen, &mut out);

    // Controls on the panel.
    let panel = Rect::new(0, 0, ui.bounds().w, ui.bounds().h);
    fn panel_walk(tree: &Tree, id: NodeId, panel: &Rect, screen: &str, out: &mut Vec<String>) {
        let node = tree.node(id);
        let rect = tree.rect(id);
        if node.action.is_some() && !rect.is_empty() && !panel.contains_rect(&rect) {
            out.push(format!(
                "{screen}\tpanel\t{} {rect:?} leaves the panel {panel:?}",
                name(tree, id)
            ));
        }
        for &child in &node.children {
            panel_walk(tree, child, panel, screen, out);
        }
    }
    panel_walk(tree, root, &panel, screen, &mut out);

    // Controls outside the box that holds them.
    fn bounds_walk(
        tree: &Tree,
        id: NodeId,
        parent: Option<Rect>,
        screen: &str,
        out: &mut Vec<String>,
    ) {
        let node = tree.node(id);
        let rect = tree.rect(id);
        if node.action.is_some() && !rect.is_empty() {
            if let Some(parent) = parent {
                if !parent.contains_rect(&rect) {
                    out.push(format!(
                        "{screen}\toutside\t{} {rect:?} leaves its box {parent:?}",
                        name(tree, id)
                    ));
                }
            }
        }
        for &child in &node.children {
            bounds_walk(tree, child, Some(rect), screen, out);
        }
    }
    bounds_walk(tree, root, None, screen, &mut out);

    // Text that does not fit the box it is drawn in. A word longer than its
    // box cannot be broken, so it is drawn over its neighbour.
    fn text_walk(
        tree: &Tree,
        id: NodeId,
        parent: Option<Rect>,
        screen: &str,
        out: &mut Vec<String>,
    ) {
        if !tree.is_visible(id) {
            return;
        }
        let node = tree.node(id);
        let rect = tree.rect(id);
        if let Kind::Text { cache, .. } = &node.kind {
            if let (Some((_, layout)), Some(parent)) = (cache, parent) {
                if !layout.lines.is_empty()
                    && !parent.is_empty()
                    && layout.width > parent.w as f32 + 0.5
                {
                    let text: String = match &node.kind {
                        Kind::Text { para, .. } => {
                            para.spans.iter().map(|s| s.text.as_str()).collect()
                        }
                        _ => String::new(),
                    };
                    out.push(format!(
                        "{screen}\ttext\t{text:?} is {} wide in a box {} wide",
                        layout.width.ceil(),
                        parent.w
                    ));
                }
            }
        }
        for &child in &node.children {
            text_walk(tree, child, Some(rect), screen, out);
        }
    }
    text_walk(tree, root, None, screen, &mut out);

    out
}

#[test]
fn narrow_panels_have_no_overlapping_controls() {
    let mut all = Vec::new();
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in PANELS {
                if !CHECKED.contains(&name) {
                    continue;
                }
                let screen = Screen::new(w, h, dpi).unwrap();
                for (screen_name, ui) in screens(screen) {
                    for finding in findings(&ui, &format!("{name}/{lang}/{screen_name}")) {
                        all.push(finding);
                    }
                }
            }
        });
    }
    assert!(
        all.is_empty(),
        "{} finding(s):\n{}",
        all.len(),
        all.join("\n")
    );
}
