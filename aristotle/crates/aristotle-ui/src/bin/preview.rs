// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desktop preview: renders every screen to PNG, in day and night mode.
//! `--size WIDTHxHEIGHT` and `--dpi DPI` select the screen profile (default: PW4).
//!
//! Every screen goes through the same `Ui` the device runs, including
//! simulated touches, so the damage overlays show the real update
//! rectangles and waveforms. Images are quantized to the 16 gray levels the
//! panel can show. Night images are `Ui::set_night`, the same inversion the
//! device applies.
//!
//! Usage: `aristotle-ui-preview [--lang CODE] [OUT_DIR]`, default
//! `ui-preview/v2` in the cache directory, `$ARISTOTLE_CACHE` (as
//! `tools/env.sh` sets it). `--lang` picks the UI language (`en`, the
//! default, or any embedded locale such as `pl`); `--lang all` renders every
//! language, each into `OUT_DIR/<code>`.
//! `ARISTOTLE_STOCK_HOME=PNG` adds `compare-library.png`, the stock home
//! screen next to ours.

use aristotle_i18n::{fmt_clock_date, fmt_clock_day, fmt_clock_weekday, fmt_date, tf, tr, Msg};
use aristotle_ui::canvas::{Canvas, Painter};
use aristotle_ui::geom::{Insets, Point, Rect, Screen, Size, SCREEN_HEIGHT, SCREEN_WIDTH};
use aristotle_ui::icons::{self, Icon};
use aristotle_ui::input::{Phase, TouchEvent};
use aristotle_ui::keyboard::{InputMode, KeyboardState};
use aristotle_ui::screens::about as about_screen;
use aristotle_ui::screens::book_actions::{self as book_actions, ActionStep, BookActionInfo};
use aristotle_ui::screens::book_stats as book_stats_screen;
use aristotle_ui::screens::calendar as calendar_screen;
use aristotle_ui::screens::cloud::{
    self as cloud_screen, CloudBook, CloudBookState, CloudFilter, CloudView,
};
use aristotle_ui::screens::coming_soon as soon_screen;
use aristotle_ui::screens::home as home_screen;
use aristotle_ui::screens::library::{
    self, sample_items, CoverMode, Filter, Item, Library, LibraryLayout, Place, Shelf, StatusLine,
    View,
};
use aristotle_ui::screens::marks as marks_screen;
use aristotle_ui::screens::mine as mine_screen;
use aristotle_ui::screens::network::{self, CatalogsView, EntryView, FeedView, SyncView};
use aristotle_ui::screens::quote_add as quote_add_screen;
use aristotle_ui::screens::quotes as quotes_screen;
use aristotle_ui::screens::reader::{self, Reader, Settings, Zone};
use aristotle_ui::screens::scan as scan_screen;
use aristotle_ui::screens::search::{build_search_screen, SearchIndex};
use aristotle_ui::screens::settings::{self as settings_screen, BatteryInfo, SystemView, Usb};
use aristotle_ui::screens::sleep;
use aristotle_ui::screens::sleep_images as sleep_images_screen;
use aristotle_ui::screens::wifi as wifi_screen;
use aristotle_ui::status::{self, Loading};
use aristotle_ui::text::{
    layout_paragraph, Align, FaceId, FamilyId, FontData, Fonts, NoHyphenation, Paragraph,
    ParagraphLayout, TextEngine, TextStyle,
};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{Hit, Kind, Node, NodeId, Refresh, Tree};
use aristotle_ui::ui::{Damage, Ui, UiEvent};
use aristotle_ui::widgets::{dashed_rect, Cover, GrayImage};
use std::path::{Path, PathBuf};

fn save(c: &Canvas, path: &Path) {
    let mut q = c.clone();
    q.quantize16();
    let f = std::fs::File::create(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), q.width, q.height);
    e.set_color(png::ColorType::Grayscale);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .and_then(|mut w| w.write_image_data(&q.packed()))
        .expect("png encoding");
    println!("wrote {}", path.display());
}

/// QA-101's layout findings, one line each: screen, check and detail.
/// The whole preview runs on one thread; a set keeps a screen that is
/// rendered twice from reporting the same thing twice.
fn qa_findings() -> &'static std::sync::Mutex<std::collections::BTreeSet<String>> {
    static FINDINGS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    FINDINGS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
}

/// The smallest touch target, in reference pixels: the prototype's rule
/// ("all clickable elements are at least 64 px high").
const MIN_TARGET: i32 = 64;

fn kind_name(kind: &Kind) -> &'static str {
    match kind {
        Kind::Text { .. } => "text",
        Kind::Icon { .. } => "icon",
        Kind::Custom(_) => "custom",
        _ => "box",
    }
}

fn node_label(tree: &Tree, id: NodeId) -> String {
    let node = tree.node(id);
    let mut label = kind_name(&node.kind).to_string();
    if let Kind::Text { para, .. } = &node.kind {
        let text: String = para.spans.iter().map(|s| s.text.as_str()).collect();
        let text = text.trim().replace(['\n', '\t'], " ");
        if !text.is_empty() {
            let snippet: String = text.chars().take(40).collect();
            label.push_str(&format!(" {snippet:?}"));
        }
    }
    if let Some(action) = node.action {
        label.push_str(&format!(" action={action}"));
    }
    label
}

/// Checks one laid-out screen: every visible node on the screen, no text
/// outside the card that holds it, nothing drawn at zero size, and no
/// touch target under 64 px.
fn qa_check(ui: &Ui, screen: &str) {
    let Some(root) = ui.tree.root() else {
        return;
    };
    let bounds = ui.bounds();
    let mut findings = Vec::new();
    fn walk(
        tree: &Tree,
        id: NodeId,
        card: Option<Rect>,
        bounds: Rect,
        screen: &str,
        findings: &mut Vec<String>,
    ) {
        let node = tree.node(id);
        if !tree.is_visible(id) {
            return;
        }
        let rect = tree.rect(id);
        let here = node_label(tree, id);
        if !rect.is_empty() && !bounds.contains_rect(&rect) {
            findings.push(format!(
                "{screen}\tbounds\t{here} at ({}, {}) {}x{} outside {}x{}",
                rect.x, rect.y, rect.w, rect.h, bounds.w, bounds.h
            ));
        }
        if let Kind::Text { .. } = &node.kind {
            if let Some(card) = card {
                if !rect.is_empty() && !card.contains_rect(&rect) {
                    findings.push(format!(
                        "{screen}\toverflow\t{here} at ({}, {}) {}x{} outside card ({}, {}) {}x{}",
                        rect.x, rect.y, rect.w, rect.h, card.x, card.y, card.w, card.h
                    ));
                }
            }
        }
        if node.action.is_some() && (rect.w < MIN_TARGET || rect.h < MIN_TARGET) {
            findings.push(format!(
                "{screen}\ttarget\t{here} at ({}, {}) is {}x{}",
                rect.x, rect.y, rect.w, rect.h
            ));
        }
        // Text, icons and controls with no room at all are not drawn: the
        // layout collapsed somewhere above them (scan-empty, QA-101).
        let has_text = match &node.kind {
            Kind::Text { para, .. } => para.spans.iter().any(|span| !span.text.trim().is_empty()),
            _ => false,
        };
        let drawn = has_text
            || matches!(
                node.kind,
                Kind::Icon { .. } | Kind::Rule { .. } | Kind::Progress { .. }
            )
            || node.action.is_some();
        if drawn && rect.is_empty() {
            findings.push(format!(
                "{screen}\tcollapsed\t{here} at ({}, {}) is {}x{}",
                rect.x, rect.y, rect.w, rect.h
            ));
        }
        let card = if node.bg.is_some() || node.border.is_some() {
            Some(rect)
        } else {
            card
        };
        for &child in &node.children {
            walk(tree, child, card, bounds, screen, findings);
        }
    }
    walk(&ui.tree, root, None, bounds, screen, &mut findings);
    if let Ok(mut all) = qa_findings().lock() {
        all.extend(findings);
    }
}

/// The screens rendered day and night, for the QA script to check its output.
fn qa_screens() -> &'static std::sync::Mutex<std::collections::BTreeSet<String>> {
    static SCREENS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    SCREENS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
}

fn qa_write(out: &Path) {
    std::fs::create_dir_all(out).expect("qa output directory");
    if let Ok(screens) = qa_screens().lock() {
        let path = out.join("qa-screens.txt");
        let text: String = screens.iter().map(|name| format!("{name}\n")).collect();
        std::fs::write(&path, text).expect("qa-screens.txt");
    }
    let all = qa_findings().lock().map(|a| a.clone()).unwrap_or_default();
    let path = out.join("qa-findings.txt");
    let text: String = all.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(&path, &text).expect("qa-findings.txt");
    println!(
        "QA-101: {} layout finding(s), see {}",
        all.len(),
        path.display()
    );
}

/// Side by side with the prototype reference, for the pages that have one.
fn write_compare(ui: &Ui, out: &Path, reference: &str, name: &str) {
    if aristotle_i18n::language() != "zh" {
        return;
    }
    let Some(ref_path) = find_reference(reference) else {
        return;
    };
    let target = if out.ends_with("zh") {
        out.parent()
            .unwrap_or(out)
            .join(format!("compare-{name}.png"))
    } else {
        out.join(format!("compare-{name}.png"))
    };
    side_by_side(&ref_path, ui.screen(), &target);
}

/// The screen as it is, then the same screen in night mode.
fn day_night(ui: &mut Ui, out: &Path, name: &str) {
    if let Ok(mut screens) = qa_screens().lock() {
        screens.insert(name.to_string());
    }
    ui.render();
    qa_check(ui, name);
    save(ui.screen(), &out.join(format!("{name}.png")));
    ui.set_night(true);
    ui.render();
    save(ui.screen(), &out.join(format!("{name}-night.png")));
    ui.set_night(false);
    ui.render();
}

fn invert(c: &Canvas) -> Canvas {
    let mut n = c.clone();
    for p in n.pixels.iter_mut() {
        *p = 255 - *p;
    }
    n
}

fn load_gray(path: &Path) -> Option<Canvas> {
    let d = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    let mut r = d.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let ch = info.color_type.samples();
    let mut c = Canvas::new(info.width, info.height);
    for (i, px) in c.pixels.iter_mut().enumerate() {
        if ch >= 3 {
            let red = buf[i * ch] as u32;
            let green = buf[i * ch + 1] as u32;
            let blue = buf[i * ch + 2] as u32;
            *px = ((red * 299 + green * 587 + blue * 114) / 1000) as u8;
        } else {
            *px = buf[i * ch];
        }
    }
    Some(c)
}

fn find_reference(name: &str) -> Option<PathBuf> {
    let candidates = [
        format!("docs/ui/prototype-v2/reference/{name}"),
        format!("../docs/ui/prototype-v2/reference/{name}"),
        format!("../../docs/ui/prototype-v2/reference/{name}"),
    ];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn side_by_side(ref_path: &Path, screen: &Canvas, out_path: &Path) {
    let Some(reference) = load_gray(ref_path) else {
        println!(
            "warning: could not load reference image at {}",
            ref_path.display()
        );
        return;
    };
    let gap = 24;
    let w = screen.width;
    let h = screen.height;
    let mut both = Canvas::new(w * 2 + gap, h);
    both.fill(Rect::new(0, 0, (w * 2 + gap) as i32, h as i32), 255);
    let mid_x = (w + gap / 2) as i32;
    both.fill(Rect::new(mid_x, 0, 1, h as i32), 200);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let i = (y as u32 * both.stride + x as u32) as usize;
            if x < reference.width as i32 && y < reference.height as i32 {
                // The reference is a screen of the prototype, with its own
                // grays; the panel has 16 levels, so both are put on the
                // same levels before they are put side by side.
                both.pixels[i] = panel_level(reference.get(x, y));
            }
            both.pixels[i + (w + gap) as usize] = screen.get(x, y);
        }
    }
    save(&both, out_path);
}

/// The nearest of the panel's 16 gray levels: 0x00, 0x11, ... 0xFF.
fn panel_level(gray: u8) -> u8 {
    let level = (gray as u32 * 15 + 127) / 255;
    (level * 17) as u8
}

fn touch(ui: &mut Ui, phase: Phase, x: i32, y: i32, ms: u64) -> Vec<UiEvent> {
    ui.handle_touch(TouchEvent {
        phase,
        id: 0,
        x,
        y,
        time_us: ms * 1000,
    })
}

fn tap(ui: &mut Ui, p: Point, ms: u64) -> Vec<UiEvent> {
    let mut ev = touch(ui, Phase::Down, p.x, p.y, ms);
    ev.extend(touch(ui, Phase::Up, p.x, p.y, ms + 60));
    ev
}

fn set(text: &mut TextEngine, s: &str, style: TextStyle, width: f32) -> ParagraphLayout {
    let para = Paragraph::plain(s, style, Align::Left);
    layout_paragraph(text, &para, width, &NoHyphenation)
}

/// The screen with each minimal damage rectangle of the last render
/// outlined (large ones labelled), the coalesced update described in the
/// caption.
fn damage_overlay(ui: &mut Ui, sent: &[Damage], caption: &str) -> Canvas {
    let density = ui.profile().density;
    let (w, h) = (ui.bounds().w as u32, ui.bounds().h as u32);
    let damage = ui.minimal_damage().to_vec();
    let caption = format!(
        "{caption}. Sent as: {}.",
        sent.iter()
            .map(|d| format!("{:?} {}×{}", d.waveform, d.rect.w, d.rect.h))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut c = ui.screen().clone();
    let all = Rect::new(0, 0, w as i32, h as i32);
    let label = TextStyle::new(FamilyId(0), 30.0)
        .at_density(density)
        .bold()
        .gray(255);
    for d in &damage {
        let mut p = Painter::new(&mut c, all);
        dashed_rect(&mut p, d.rect.outset(3), 4, 14, 0);
        if d.rect.area() < 20_000 {
            continue;
        }
        let text = format!("{:?} {}×{}", d.waveform, d.rect.w, d.rect.h);
        let l = set(&mut ui.text, &text, label.clone(), 600.0);
        let tag = Rect::new(d.rect.x, d.rect.y, l.width as i32 + 24, l.height as i32 + 8);
        p.fill_rect(tag, 0);
        l.draw(
            &mut ui.text,
            &mut p,
            tag.x as f32 + 12.0,
            tag.y as f32 + 4.0,
        );
    }
    let style = TextStyle::new(FamilyId(0), 34.0)
        .at_density(density)
        .italic();
    let l = set(&mut ui.text, &caption, style, w as f32 - 80.0);
    let mut p = Painter::new(&mut c, all);
    let band = Rect::new(
        0,
        h as i32 - l.height as i32 - 24,
        w as i32,
        l.height as i32 + 24,
    );
    p.fill_rect(band, 235);
    l.draw(&mut ui.text, &mut p, 40.0, band.y as f32 + 12.0);
    c
}

fn log(step: &str, ui: &Ui, damage: &[Damage]) {
    println!(
        "{step}: {} minimal rect(s), sent:",
        ui.minimal_damage().len()
    );
    if damage.is_empty() {
        println!("  (no damage)");
    }
    for d in damage {
        println!(
            "  {:<5} {:>4}×{:<4} at ({}, {})",
            format!("{:?}", d.waveform),
            d.rect.w,
            d.rect.h,
            d.rect.x,
            d.rect.y
        );
    }
}

/// The fonts the preview draws with: Literata, compiled in, and the
/// Chinese fallback when the render data directory has it. That is the
/// same file the app loads (see the app's `ui_fonts`), so the preview
/// shows Chinese the way the device does.
fn fonts() -> Fonts {
    let mut fonts = Fonts::embedded();
    if let Some(face) = cjk_face(&mut fonts) {
        if let Some(family) = fonts.add_family("Noto Sans SC", [Some(face), None, None, None]) {
            fonts.add_fallback(family);
        }
    }
    fonts
}

/// The Chinese fallback font: `$ARISTOTLE_RENDER_DATA/fonts/DejaVuSans-Symbols.ttf`,
/// the file Plato's engine falls back to for every script the body font
/// does not cover.
fn cjk_face(fonts: &mut Fonts) -> Option<FaceId> {
    let dir = std::env::var_os("ARISTOTLE_RENDER_DATA")?;
    let path = PathBuf::from(dir).join("fonts/DejaVuSans-Symbols.ttf");
    let bytes = std::fs::read(path).ok()?;
    fonts
        .add_face(FontData::Shared(std::sync::Arc::new(bytes)))
        .ok()
}

fn engine(fonts_dir: &Path) -> (TextEngine, [FamilyId; 3]) {
    let mut fonts = fonts();
    let lit = FamilyId(0);
    let charis = fonts
        .load_family("Charis", fonts_dir, "Charis")
        .unwrap_or(lit);
    let gentium = fonts
        .load_family("Gentium", fonts_dir, "GentiumBook")
        .unwrap_or(lit);
    (TextEngine::new(fonts), [lit, charis, gentium])
}

const STATUS: StatusLine<'static> = StatusLine {
    clock: "21:38",
    battery: Some(0.82),
    charging: false,
};

/// A library screen in a fresh `Ui`, laid out and synced.
fn shelf_ui(profile: Screen, shelf: &Shelf) -> (Ui, Library) {
    let size = Size::new(profile.width as i32, profile.height as i32);
    let mut text = TextEngine::new(fonts());
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let (tree, lib) = Library::build_shelf_for_width(&mut text, &styles, shelf, size.w);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    lib.sync(&mut ui.tree);
    (ui, lib)
}

/// A stand-in for a book's own cover in the previews: 200 × 300, a dark
/// band at the top, a light field with two rules of type and a dark foot,
/// on the panel's levels. Real thumbnails arrive the same way, dithered to
/// those levels by the library's cover cache.
fn sample_cover(seed: u8) -> GrayImage {
    sample_cover_at(200, 300, seed)
}

/// The same at another size: the screensaver asks the library for a 560 × 840
/// thumbnail, so its shot must not stretch the small one.
fn sample_cover_at(w: u32, h: u32, seed: u8) -> GrayImage {
    let mut px = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = if y * 5 < h {
                0x22
            } else if y * 4 < h {
                0xff
            } else if y * 10 < h * 3 {
                // Two rules of type on a white field.
                if x * 10 > w && x * 10 < w * 8 && (y * 40) % (h * 10) < 4 {
                    0x00
                } else {
                    0xf2
                }
            } else if y * 4 < h * 3 {
                0xf2
            } else {
                if (y * 40) % (h * 10) < 12 && x * 7 > w * 5 {
                    0xdd
                } else {
                    0x11u8.wrapping_add(seed % 3 * 0x11)
                }
            };
            px.push(v);
        }
    }
    GrayImage::new(w, h, px)
}

/// Gives the books at `indexes` a cover of their own; the others keep the
/// generated card, which is what the previews must show side by side.
fn with_covers(books: &mut [library::Book], indexes: &[usize]) {
    for (i, b) in books.iter_mut().enumerate() {
        if indexes.contains(&i) {
            b.cover = Some(sample_cover(i as u8));
        }
    }
}

/// The books the home screen shows: the ones of the reference screenshot,
/// so the shots can be compared with it pixel by pixel.
fn home_books() -> Vec<library::Book> {
    let mut reading = library::Book::new("红楼梦", "曹雪芹", 0.43);
    reading.chapter = "第七回".into();
    // What the record of the reader's stays estimates for the book being
    // read; the card shows whole hours ("剩余约 12 小时").
    reading.minutes_left = Some(742);
    let mut books = vec![
        reading,
        library::Book::new("西游记", "吴承恩", 0.68),
        library::Book::new("呐喊", "鲁迅", 0.15),
        library::Book::new("傲慢与偏见", "简·奥斯汀", 0.91),
    ];
    // Two books have a cover of their own; the other two keep the generated
    // card, so one shot shows both.
    with_covers(&mut books, &[0, 1]);
    books
}

/// Books matching the prototype-v2 specification for library list and grid screens.
fn prototype_books() -> Vec<library::Book> {
    let mut books = vec![
        library::Book::new("红楼梦", "曹雪芹", 0.43).with_format("EPUB"),
        library::Book::new("西游记", "吴承恩", 0.68).with_format("EPUB"),
        library::Book::new("呐喊", "鲁迅", 0.15).with_format("TXT"),
        library::Book::new("傲慢与偏见", "简·奥斯汀", 1.0).with_format("EPUB"),
        library::Book::new("三国演义", "罗贯中", 0.0).with_format("MOBI"),
        library::Book::new("骆驼祥子", "老舍", 0.07).with_format("EPUB"),
        library::Book::new("朝花夕拾", "鲁迅", 0.0).with_format("PDF"),
    ];
    with_covers(&mut books, &[0, 1, 3]);
    books
}

/// English version of the prototype-v2 books for internationalized preview.
fn prototype_books_en() -> Vec<library::Book> {
    let mut books = vec![
        library::Book::new("Dream of the Red Chamber", "Cao Xueqin", 0.43).with_format("EPUB"),
        library::Book::new("Journey to the West", "Wu Cheng'en", 0.68).with_format("EPUB"),
        library::Book::new("Call to Arms", "Lu Xun", 0.15).with_format("TXT"),
        library::Book::new("Pride and Prejudice", "Jane Austen", 1.0).with_format("EPUB"),
        library::Book::new("Romance of the Three Kingdoms", "Luo Guanzhong", 0.0)
            .with_format("MOBI"),
        library::Book::new("Rickshaw Boy", "Lao She", 0.07).with_format("EPUB"),
        library::Book::new("Dawn Blossoms Plucked at Dusk", "Lu Xun", 0.0).with_format("PDF"),
    ];
    with_covers(&mut books, &[0, 1, 3]);
    books
}

/// The same library with more books, so the block of recent books has a
/// second page and the dots that show it.
fn home_books_paged() -> Vec<library::Book> {
    let mut books = home_books();
    books.push(library::Book::new("三国演义", "罗贯中", 0.52));
    books.push(library::Book::new("水浒传", "施耐庵", 0.34));
    books.push(library::Book::new("聊斋志异", "蒲松龄", 0.77));
    books
}

/// What the clock card says: the day of the reference screenshot.
fn clock_text() -> home_screen::ClockText {
    let lunar = home_screen::lunar::Lunar::of(2026, 11, 4);
    let chinese = aristotle_i18n::language() == "zh";
    home_screen::ClockText {
        time: "21:38".into(),
        date: fmt_clock_date(11, 4, 3),
        month_day: fmt_clock_day(11, 4),
        weekday: fmt_clock_weekday(3),
        lunar: if chinese { lunar.zh() } else { lunar.en() },
        lunar_short: if chinese {
            lunar.short_zh()
        } else {
            lunar.short_en()
        },
        term: lunar.term.map(|t| if chinese { t.zh() } else { t.en() }),
    }
}

/// The statistics of the reference screenshot.
fn home_stats() -> home_screen::Stats {
    home_screen::Stats {
        today_minutes: 23,
        streak_days: 5,
        week_hours: 3.2,
    }
}

/// The home page, the "mine" page and the about page: the three screens
/// around the bottom bar.
fn section_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let books = home_books();
    let (reading, recent) = (&books[0], &books[1..]);
    let clock = clock_text();
    let stats = home_stats();
    let quote = &home_screen::quotes::QUOTES[0];

    // The home page as the prototype has it: the clock card, the book
    // being read with the cat, the statistics, the books opened lately and
    // the quotation of the day.
    let mut text = TextEngine::new(fonts());
    let home = home_screen::Home {
        reading: Some(reading),
        recent,
        status: STATUS,
        clock: home_screen::ClockStyle::BigLunar,
        clock_text: clock.clone(),
        stats: Some(stats),
        quote: Some(quote),
        pet: true,
    };
    let (tree, ids) = home_screen::HomeUi::build(&mut text, &styles, &home, profile.width as i32);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    ids.place_pet(&mut ui.tree);
    day_night(&mut ui, out, "home");
    write_compare(&ui, out, "home.png", "home");

    // The second page of the books opened lately: a library with more
    // books, so the block of recent books has a second page.
    let paged = home_books_paged();
    let mut text = TextEngine::new(fonts());
    let home = home_screen::Home {
        reading: Some(&paged[0]),
        recent: &paged[1..],
        status: STATUS,
        clock: home_screen::ClockStyle::BigLunar,
        clock_text: clock.clone(),
        stats: Some(stats),
        quote: Some(quote),
        pet: true,
    };
    let (tree, ids) = home_screen::HomeUi::build(&mut text, &styles, &home, profile.width as i32);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    ids.place_pet(&mut ui.tree);
    ui.tree.set_page(ids.recent_pages, 1);
    ui.layout();
    ids.sync(&mut ui.tree);
    day_night(&mut ui, out, "home-page2");

    // The four styles of the clock card.
    for (style, name) in [
        (home_screen::ClockStyle::BigLunar, "home-clock-big-lunar"),
        (home_screen::ClockStyle::BigWeek, "home-clock-big-week"),
        (home_screen::ClockStyle::Line, "home-clock-line"),
        (home_screen::ClockStyle::Off, "home-clock-off"),
    ] {
        let mut text = TextEngine::new(fonts());
        let home = home_screen::Home {
            reading: Some(reading),
            recent,
            status: STATUS,
            clock: style,
            clock_text: clock.clone(),
            stats: Some(stats),
            quote: Some(quote),
            pet: true,
        };
        let (tree, ids) =
            home_screen::HomeUi::build(&mut text, &styles, &home, profile.width as i32);
        let mut ui = Ui::with_screen(tree, text, profile);
        ui.layout();
        ids.place_pet(&mut ui.tree);
        day_night(&mut ui, out, name);
    }

    // The screen without the statistics, without the quotation and
    // without the cat.
    for (stats_on, quote_on, pet_on, name) in [
        (false, true, true, "home-no-stats"),
        (true, false, true, "home-no-quote"),
        (true, true, false, "home-no-pet"),
    ] {
        let mut text = TextEngine::new(fonts());
        let home = home_screen::Home {
            reading: Some(reading),
            recent,
            status: STATUS,
            clock: home_screen::ClockStyle::BigLunar,
            clock_text: clock.clone(),
            stats: stats_on.then_some(stats),
            quote: quote_on.then_some(quote),
            pet: pet_on,
        };
        let (tree, ids) =
            home_screen::HomeUi::build(&mut text, &styles, &home, profile.width as i32);
        let mut ui = Ui::with_screen(tree, text, profile);
        ui.layout();
        ids.place_pet(&mut ui.tree);
        day_night(&mut ui, out, name);
    }

    // The home page of a library that has nothing to continue yet.
    let mut text = TextEngine::new(fonts());
    let home = home_screen::Home {
        reading: None,
        recent,
        status: STATUS,
        clock: home_screen::ClockStyle::BigLunar,
        clock_text: clock.clone(),
        stats: Some(stats),
        quote: Some(quote),
        pet: true,
    };
    let (tree, ids) = home_screen::HomeUi::build(&mut text, &styles, &home, profile.width as i32);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    ids.place_pet(&mut ui.tree);
    day_night(&mut ui, out, "home-empty");

    let mut text = TextEngine::new(fonts());
    let mine = mine_screen::sample(STATUS);
    let (tree, _ids) = mine_screen::MineUi::build(&mut text, &styles, &mine);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "mine");
    write_compare(&ui, out, "mine.png", "mine");

    let about = settings_screen::sample_about();
    let mut text = TextEngine::new(fonts());
    let page = about_screen::About {
        status: STATUS,
        facts: &about,
    };
    let (tree, _ids) = about_screen::AboutUi::build(&mut text, &styles, &page);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "about");

    // The page of a feature that is not built yet.
    let mut text = TextEngine::new(fonts());
    let soon = soon_screen::ComingSoon {
        status: STATUS,
        title: tr(Msg::MineEntryCalendar),
    };
    let (tree, _ids) = soon_screen::ComingSoonUi::build(&mut text, &styles, &soon);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "coming-soon");

    // The quotation library screen.
    let mut text = TextEngine::new(fonts());
    let quote_screen = quotes_screen::sample(STATUS);
    let (tree, _ids) = quotes_screen::QuoteUi::build(&mut text, &styles, &quote_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "quotelibrary");
    if aristotle_i18n::language() == "zh" {
        if let Some(ref_path) = find_reference("quotelibrary.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-quotelibrary.png")
            } else {
                out.join("compare-quotelibrary.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }

    // The quote add screen.
    let mut quote_add_state =
        quote_add_screen::QuoteAddState::new(home_screen::quotes::QuoteCategory::Reading);
    quote_add_state.text = "生活不可能像你想象得那么好，但也不会像你想象得那么糟。".to_string();
    quote_add_state.author = "莫泊桑".to_string();
    quote_add_state.source = "一生".to_string();
    quote_add_state.keyboard.committed = quote_add_state.text.clone();
    let (tree, _view) = quote_add_screen::QuoteAddUi::build(&styles, &quote_add_state, &STATUS);
    let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
    ui.layout();
    day_night(&mut ui, out, "quote-add");
    if aristotle_i18n::language() == "zh" {
        if let Some(ref_path) = find_reference("keyboard.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-keyboard.png")
            } else {
                out.join("compare-keyboard.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }
}

/// The status bar with its loading area: the home screen with and without
/// its clock card, the library and the "mine" screen, each of them with a
/// book being laid out.
fn status_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let books = home_books();
    let (reading, recent) = (&books[0], &books[1..]);
    let clock = clock_text();
    let stats = home_stats();
    let quote = &home_screen::quotes::QUOTES[0];
    let loading = Loading::with_percent(
        tf!(Msg::StatusFormatting, title = reading.title.as_str()),
        45,
    );

    // The home page, with its clock card and without it.
    for (style, name) in [
        (home_screen::ClockStyle::BigLunar, "home-loading"),
        (home_screen::ClockStyle::Off, "home-clock-off-loading"),
    ] {
        let mut text = TextEngine::new(fonts());
        let home = home_screen::Home {
            reading: Some(reading),
            recent,
            status: STATUS,
            clock: style,
            clock_text: clock.clone(),
            stats: Some(stats),
            quote: Some(quote),
            pet: true,
        };
        let (tree, ids) =
            home_screen::HomeUi::build(&mut text, &styles, &home, profile.width as i32);
        let mut ui = Ui::with_screen(tree, text, profile);
        status::set_loading(&mut ui.tree, &ids.loading, Some(&loading));
        ui.layout();
        ids.place_pet(&mut ui.tree);
        day_night(&mut ui, out, name);
    }

    // The library, with a book being laid out.
    let items = sample_items(&books[1..], View::Books);
    let shelf = Shelf {
        place: Place::Home {
            view: View::Books,
            filter: Filter::All,
        },
        hero: Some(&books[0]),
        items: &items,
        sort: tr(Msg::LibrarySortRecent),
        status: STATUS,
        covers: CoverMode::None,
        layout: LibraryLayout::List,
        local_books_count: items.len(),
    };
    let (mut ui, lib) = shelf_ui(profile, &shelf);
    status::set_loading(&mut ui.tree, &lib.loading, Some(&loading));
    ui.layout();
    day_night(&mut ui, out, "library-loading");

    // The "mine" page, with a book being laid out.
    let mut text = TextEngine::new(fonts());
    let mine = mine_screen::sample(STATUS);
    let (tree, ids) = mine_screen::MineUi::build(&mut text, &styles, &mine);
    let mut ui = Ui::with_screen(tree, text, profile);
    status::set_loading(&mut ui.tree, &ids.loading, Some(&loading));
    ui.layout();
    day_night(&mut ui, out, "mine-loading");

    // The reader has no status bar: the words stand next to the progress
    // bar at the bottom of the page.
    let text = TextEngine::new(fonts());
    let mut tree = Tree::new();
    let (row, nodes) = reader::progress_row(&mut tree, &styles);
    let row = tree.align(0.5, 1.0, row);
    tree.set_root(row);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.tree.set_text(nodes.text, &loading.text());
    ui.tree.set_hidden(nodes.bar, false);
    if let Some(w) = ui
        .tree
        .widget_mut::<aristotle_ui::widgets::ProgressBar>(nodes.bar)
    {
        w.percent = 45;
    }
    ui.tree.invalidate(nodes.row, Refresh::Transient);
    ui.layout();
    day_night(&mut ui, out, "reader-progress");
}

/// "My highlights and notes": a page of cards, the worst case of the layout
/// (three-line highlights and two-line notes), then the empty state.
fn marks_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let mut text = TextEngine::new(fonts());
    let page = marks_screen::sample(STATUS);
    let (tree, view) = marks_screen::MarksUi::build(&mut text, &styles, &page);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    view.sync(&mut ui.tree);
    ui.layout();
    day_night(&mut ui, out, "marks");

    let mut text = TextEngine::new(fonts());
    let full = marks_screen::worst_case(STATUS);
    let (tree, view) = marks_screen::MarksUi::build(&mut text, &styles, &full);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    view.sync(&mut ui.tree);
    ui.layout();
    day_night(&mut ui, out, "marks-full");

    let empty = marks_screen::MarksPage {
        status: STATUS,
        page: 1,
        entries: Vec::new(),
    };
    let mut text = TextEngine::new(fonts());
    let (tree, view) = marks_screen::MarksUi::build(&mut text, &styles, &empty);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    view.sync(&mut ui.tree);
    ui.layout();
    day_night(&mut ui, out, "marks-empty");
}

/// The reading calendar: the month, the grid of the days, and the day
/// picked with the books read then.
fn calendar_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let mut text = TextEngine::new(fonts());
    let mut cal = calendar_screen::sample(STATUS);
    if let Some(book) = cal.books.first_mut() {
        book.cover = Some(sample_cover(1));
    }
    let (tree, _ids) = calendar_screen::CalendarUi::build(&mut text, &styles, &cal);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "calendar");
    if let Some(ref_path) = find_reference("readingcalendar.png") {
        let target = if out.ends_with("zh") {
            out.parent()
                .unwrap_or(out)
                .join("compare-readingcalendar.png")
        } else {
            out.join("compare-readingcalendar.png")
        };
        side_by_side(&ref_path, ui.screen(), &target);
    }
}

/// The single book reading statistics screen: cover card, 2x3 grid,
/// predicted completion, and the 7-day bar chart.
fn book_stats_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let mut text = TextEngine::new(fonts());
    let mut stats = book_stats_screen::sample(STATUS);
    stats.cover_image = Some(sample_cover(0));
    let (tree, _view) = book_stats_screen::BookStatsUi::build(&mut text, &styles, &stats);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "bookstats");
    if let Some(ref_path) = find_reference("bookstats.png") {
        let target = if out.ends_with("zh") {
            out.parent().unwrap_or(out).join("compare-bookstats.png")
        } else {
            out.join("compare-bookstats.png")
        };
        side_by_side(&ref_path, ui.screen(), &target);
    }
}

/// A cloud shelf in a fresh `Ui`, laid out and synced.
fn cloud_ui(profile: Screen, view: &CloudView) -> (Ui, cloud_screen::CloudUi) {
    let size = Size::new(profile.width as i32, profile.height as i32);
    let mut text = TextEngine::new(fonts());
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let (tree, shelf) = CloudView::build_for_width(&mut text, &styles, view, size.w);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    shelf.sync(&mut ui.tree);
    (ui, shelf)
}

fn cloud_view<'a>(
    signed_in: bool,
    filter: CloudFilter,
    loading: bool,
    message: Option<&'a str>,
    books: &'a [CloudBook],
) -> CloudView<'a> {
    CloudView {
        status: STATUS,
        signed_in,
        book_count: if signed_in { books.len() } else { 0 },
        local_books_count: 128,
        last_sync: signed_in.then_some("21:30"),
        offline: false,
        filter,
        loading,
        message,
        books,
    }
}

fn cloud_screens(profile: Screen, out: &Path) {
    let mut one = CloudBook::new("红楼梦", "曹雪芹", "EPUB");
    one.progress = Some((43, 100));
    one.downloaded = true;
    one.state = CloudBookState::Downloaded;

    one.cover = Some(sample_cover(0));

    let mut two = CloudBook::new("水浒传", "施耐庵", "EPUB");
    two.progress = Some((26, 100));
    two.state = CloudBookState::Downloading;
    two.progress_fraction = 0.62;

    let mut three = CloudBook::new("聊斋志异", "蒲松龄", "EPUB");
    three.progress = Some((58, 100));

    let mut four = CloudBook::new("傲慢与偏见", "简·奥斯汀", "EPUB");
    four.progress = Some((100, 100));

    let mut five = CloudBook::new("儒林外史", "吴敬梓", "PDF");
    five.state = CloudBookState::Failed;

    let six = CloudBook::new("镜花缘", "李汝珍", "EPUB");

    let books = [one, two, three, four, five, six];

    let (mut ui, _) = cloud_ui(
        profile,
        &cloud_view(true, CloudFilter::All, false, None, &books),
    );
    day_night(&mut ui, out, "cloud-books");

    let (mut ui, _) = cloud_ui(
        profile,
        &cloud_view(true, CloudFilter::All, false, None, &[]),
    );
    day_night(&mut ui, out, "cloud-empty");

    let (mut ui, _) = cloud_ui(
        profile,
        &cloud_view(true, CloudFilter::All, true, None, &[]),
    );
    day_night(&mut ui, out, "cloud-loading");

    let (mut ui, _) = cloud_ui(
        profile,
        &cloud_view(
            true,
            CloudFilter::All,
            false,
            Some(tr(Msg::CloudErrNetwork)),
            &books,
        ),
    );
    day_night(&mut ui, out, "cloud-failed");

    let (mut ui, _) = cloud_ui(
        profile,
        &cloud_view(false, CloudFilter::All, false, None, &[]),
    );
    day_night(&mut ui, out, "cloud-logged-out");

    let (mut ui, _) = cloud_ui(
        profile,
        &cloud_view(true, CloudFilter::Downloaded, false, None, &books),
    );
    day_night(&mut ui, out, "cloud-downloaded");
}

/// The library with the book's actions sheet open: the menu, the details
/// of the book, and the question before the local file is deleted.
fn library_actions(profile: Screen, out: &Path) {
    let size = Size::new(profile.width as i32, profile.height as i32);
    let mut books = library::sample_books();
    with_covers(&mut books, &[0, 1, 3]);
    let hero = &books[0];
    let all = sample_items(&books[1..], View::Books);
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let mut text = TextEngine::new(fonts());
    let shelf = Shelf {
        place: Place::Home {
            view: View::Books,
            filter: Filter::All,
        },
        hero: Some(hero),
        items: &all,
        sort: tr(Msg::LibrarySortRecent),
        status: STATUS,
        covers: CoverMode::Cards,
        layout: LibraryLayout::List,
        local_books_count: all.len(),
    };
    let (tree, lib) = Library::build_shelf_for_width(&mut text, &styles, &shelf, size.w);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    ui.render();

    // The first row is held down: the row turns black across the width of
    // the screen, and the sheet comes up from the bottom.
    let held = lib.rows[0];
    let info = sample_action_info(&books[1]);
    let t0 = std::time::Instant::now();
    let sheet = book_actions::build_book_actions(&mut ui.tree, &styles, &info, ActionStep::Menu);
    println!(
        "library: the sheet's tree built in {:.1} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    ui.tree.attach_overlay(sheet.root);
    ui.tree.set_inverted(held, true);
    ui.layout();
    let d = ui.render();
    log("library: the book's actions come up", &ui, &d);
    qa_check(&ui, "library-actions");
    save(ui.screen(), &out.join("library-actions-menu.png"));
    write_compare(&ui, out, "libraryactions.png", "libraryactions");

    // The details of the book.
    book_actions::set_book_actions(&mut ui.tree, &styles, &sheet, &info, ActionStep::Details);
    ui.layout();
    let d = ui.render();
    log("library: the book's details", &ui, &d);
    save(ui.screen(), &out.join("library-actions-details.png"));

    // The question before the local file is deleted.
    book_actions::set_book_actions(
        &mut ui.tree,
        &styles,
        &sheet,
        &info,
        ActionStep::ConfirmDelete,
    );
    ui.layout();
    let d = ui.render();
    log("library: the question before the deletion", &ui, &d);
    save(ui.screen(), &out.join("library-actions-delete.png"));
}

/// What the sheet says about the book in the shots.
fn sample_action_info(b: &library::Book) -> BookActionInfo {
    BookActionInfo {
        id: "sample".into(),
        title: b.title.clone(),
        author: b.author.clone(),
        format: "EPUB".into(),
        size: "2.4 MB".into(),
        path: "中國小說/紅樓夢.epub".into(),
        full_path: "/mnt/us/documents/中國小說/紅樓夢.epub".into(),
        progress: 0.68,
        finished: false,
        unread: false,
        read_secs: 9 * 3600 + 12 * 60,
        start_date: Some(fmt_date(2026, 10, 17, 6)),
        cover: b.cover.clone(),
    }
}

fn library_screens(profile: Screen, out: &Path) {
    let size = Size::new(profile.width as i32, profile.height as i32);
    let (w, h) = (size.w as u32, size.h as u32);
    let books = if aristotle_i18n::language() == "zh" {
        prototype_books()
    } else if aristotle_i18n::language() == "en" {
        prototype_books_en()
    } else {
        let mut books = library::sample_books();
        with_covers(&mut books, &[0, 1, 3]);
        books
    };
    let home = |view, filter, covers, layout, items: &[Item]| -> (Ui, Library) {
        shelf_ui(
            profile,
            &Shelf {
                place: Place::Home { view, filter },
                hero: None,
                items,
                sort: tr(Msg::LibrarySortRecent),
                status: STATUS,
                covers,
                layout,
                local_books_count: 128,
            },
        )
    };

    let all = sample_items(&books, View::Books);
    let (mut ui, mut lib) = home(
        View::Books,
        Filter::All,
        CoverMode::Cards,
        LibraryLayout::List,
        &all,
    );
    {
        let d = ui.render();
        log("library: first frame", &ui, &d)
    };
    day_night(&mut ui, out, "library");
    if aristotle_i18n::language() == "zh" {
        if let Some(ref_path) = find_reference("library.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-library.png")
            } else {
                out.join("compare-library.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }

    // The same shelf with the books' own covers: the setting the library's
    // chips call "photos".
    let (mut ui, _) = home(
        View::Books,
        Filter::All,
        CoverMode::Photos,
        LibraryLayout::List,
        &all,
    );
    day_night(&mut ui, out, "library-photos");

    // Finger down on the second row: an instant A2 outline.
    let c = ui.tree.rect(lib.rows[1]).center();
    touch(&mut ui, Phase::Down, c.x, c.y, 0);
    let d = ui.render();
    log("library: press on a book row", &ui, &d);
    save(ui.screen(), &out.join("library-pressed.png"));
    let o = damage_overlay(
        &mut ui,
        &d,
        "Touch-down on a row: only the outline goes out, with A2",
    );
    save(&o, &out.join("library-pressed-damage.png"));
    touch(&mut ui, Phase::Move, c.x + profile.density.px(200), c.y, 40);
    touch(&mut ui, Phase::Up, c.x + profile.density.px(200), c.y, 80);
    ui.render();

    // Next page of the list.
    let next = ui.tree.rect(lib.next).center();
    for ev in tap(&mut ui, next, 1000) {
        if let UiEvent::Action { id, .. } = ev {
            lib.handle(&mut ui.tree, id);
        }
    }
    ui.layout();
    lib.sync(&mut ui.tree);
    let d = ui.render();
    log("library: page 2", &ui, &d);
    day_night(&mut ui, out, "library-page2");
    let o = damage_overlay(&mut ui, &d, "List page turn: the list and the pager only");
    save(&o, &out.join("library-page2-damage.png"));

    // Page jump dialog over the library.
    lib.handle(&mut ui.tree, library::action::JUMP);
    ui.layout();
    lib.sync(&mut ui.tree);
    let d = ui.render();
    log("library: jump dialog", &ui, &d);
    day_night(&mut ui, out, "library-jump");
    let o = damage_overlay(&mut ui, &d, "Jump dialog opened over library");
    save(&o, &out.join("library-jump-damage.png"));

    let (mut ui, _) = home(
        View::Books,
        Filter::All,
        CoverMode::Cards,
        LibraryLayout::List,
        &all,
    );
    day_night(&mut ui, out, "library-cards");
    let (mut ui, _) = home(
        View::Books,
        Filter::Reading,
        CoverMode::Cards,
        LibraryLayout::List,
        &all,
    );
    day_night(&mut ui, out, "library-reading");
    let (mut ui, _) = home(
        View::Books,
        Filter::New,
        CoverMode::Cards,
        LibraryLayout::List,
        &all,
    );
    day_night(&mut ui, out, "library-new-cards");
    let folders = sample_items(&books, View::Folders);
    let (mut ui, _) = home(
        View::Folders,
        Filter::All,
        CoverMode::None,
        LibraryLayout::List,
        &folders,
    );
    day_night(&mut ui, out, "library-folders");
    let series = sample_items(&books, View::Series);
    let (mut ui, _) = home(
        View::Series,
        Filter::All,
        CoverMode::None,
        LibraryLayout::List,
        &series,
    );
    day_night(&mut ui, out, "library-series");

    // Grid views for Books, Folders, and Series.
    let books_grid = sample_items(&books, View::Books);
    let (mut ui, _) = home(
        View::Books,
        Filter::All,
        CoverMode::Cards,
        LibraryLayout::Grid,
        &books_grid,
    );
    day_night(&mut ui, out, "library-grid");
    if aristotle_i18n::language() == "zh" {
        if let Some(ref_path) = find_reference("librarygrid.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-grid.png")
            } else {
                out.join("compare-grid.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }
    let (mut ui, _) = home(
        View::Books,
        Filter::All,
        CoverMode::Photos,
        LibraryLayout::Grid,
        &books_grid,
    );
    day_night(&mut ui, out, "library-grid-photos");
    let (mut ui, _) = home(
        View::Folders,
        Filter::All,
        CoverMode::Cards,
        LibraryLayout::Grid,
        &folders,
    );
    day_night(&mut ui, out, "library-grid-folders");
    let (mut ui, _) = home(
        View::Series,
        Filter::All,
        CoverMode::Cards,
        LibraryLayout::Grid,
        &series,
    );
    day_night(&mut ui, out, "library-grid-series");

    // One folder and one series as their own shelves.
    let platon: Vec<Item> = books
        .iter()
        .filter(|b| b.folder.as_deref() == Some("Platon"))
        .cloned()
        .map(Item::Book)
        .collect();
    let (mut ui, _) = shelf_ui(
        profile,
        &Shelf {
            place: Place::Folder { name: "Platon" },
            hero: None,
            items: &platon,
            sort: tr(Msg::LibrarySortTitle),
            status: STATUS,
            covers: CoverMode::Cards,
            layout: LibraryLayout::List,
            local_books_count: platon.len(),
        },
    );
    day_night(&mut ui, out, "folder");
    let witcher: Vec<Item> = books
        .iter()
        .filter(|b| b.series.is_some())
        .cloned()
        .map(Item::Book)
        .collect();
    let (mut ui, _) = shelf_ui(
        profile,
        &Shelf {
            place: Place::Series { name: "Trylogia" },
            hero: None,
            items: &witcher,
            sort: "",
            status: STATUS,
            covers: CoverMode::None,
            layout: LibraryLayout::List,
            local_books_count: platon.len(),
        },
    );
    day_night(&mut ui, out, "series");

    // A library with no books yet.
    let (mut ui, _) = shelf_ui(
        profile,
        &Shelf {
            place: Place::Home {
                view: View::Books,
                filter: Filter::All,
            },
            hero: None,
            items: &[],
            sort: tr(Msg::LibrarySortRecent),
            status: STATUS,
            covers: CoverMode::None,
            layout: LibraryLayout::List,
            local_books_count: 0,
        },
    );
    day_night(&mut ui, out, "library-empty");

    // Side by side with the stock home screen, when a capture of it is
    // given: ARISTOTLE_STOCK_HOME=path/to/stock-home.png (1072 x 1448 gray).
    let stock = std::env::var_os("ARISTOTLE_STOCK_HOME")
        .map(PathBuf::from)
        .and_then(|p| load_gray(&p));
    if let Some(s) = stock {
        let (mut ui, _) = home(
            View::Books,
            Filter::All,
            CoverMode::None,
            LibraryLayout::List,
            &all,
        );
        ui.render();
        let gap = 40;
        let mut both = Canvas::new(w * 2 + gap, h);
        both.fill(Rect::new(w as i32, 0, gap as i32, h as i32), 128);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let i = (y as u32 * both.stride + x as u32) as usize;
                if x < s.width as i32 && y < s.height as i32 {
                    both.pixels[i] = s.get(x, y);
                }
                both.pixels[i + (w + gap) as usize] = ui.screen().get(x, y);
            }
        }
        save(&both, &out.join("compare-library-stock.png"));
    }
}

/// The Wi-Fi transfer page, in the state the prototype shows: a file on
/// its way and three that have arrived.
fn wifi_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let is_zh = aristotle_i18n::language() == "zh";
    let (current, received, line) = if is_zh {
        (
            "《西游记》.epub".to_string(),
            vec![
                wifi_screen::ReceivedBook {
                    name: "红楼梦.epub".into(),
                    format: "EPUB".into(),
                    size: 3_251_712,
                },
                wifi_screen::ReceivedBook {
                    name: "呐喊.txt".into(),
                    format: "TXT".into(),
                    size: 419_430,
                },
                wifi_screen::ReceivedBook {
                    name: "朝花夕拾.pdf".into(),
                    format: "PDF".into(),
                    size: 13_004_800,
                },
            ],
            "4.1 MB / 6.6 MB · 还需约 20 秒".to_string(),
        )
    } else {
        (
            "Journey to the West.epub".to_string(),
            vec![
                wifi_screen::ReceivedBook {
                    name: "Dream of the Red Chamber.epub".into(),
                    format: "EPUB".into(),
                    size: 3_251_712,
                },
                wifi_screen::ReceivedBook {
                    name: "The Call of the Wild.txt".into(),
                    format: "TXT".into(),
                    size: 419_430,
                },
                wifi_screen::ReceivedBook {
                    name: "Selected Poems.pdf".into(),
                    format: "PDF".into(),
                    size: 13_004_800,
                },
            ],
            "4.1 MB / 6.6 MB · about 20 s left".to_string(),
        )
    };
    let screen = wifi_screen::Wifi {
        status: STATUS,
        connected: true,
        firewall_open: true,
        address: "http://192.168.1.23:8080".to_string(),
        phase: wifi_screen::Phase::Receiving,
        current_file: current,
        progress: 0.62,
        status_line: line,
        received,
        total_size: 14_562_944,
        running: true,
    };
    let mut text = TextEngine::new(fonts());
    let (tree, _) = wifi_screen::WifiUi::build(&mut text, &styles, &screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "wifitransfer");

    if is_zh {
        if let Some(ref_path) = find_reference("wifitransfer.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-wifitransfer.png")
            } else {
                out.join("compare-wifitransfer.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }
}

fn settings_screens(profile: Screen, out: &Path) {
    let view = SystemView {
        status: STATUS,
        light: Some((20, 24)),
        night: false,
        covers: CoverMode::Cards,
        sleep_minutes: Some(15),
        sleep_screen: 0,
        clippings: true,
        battery: Some(BatteryInfo {
            level: 0.82,
            charging: false,
            state: tr(Msg::SettingsBatteryDischarging),
            voltage_mv: 3980,
            temperature_c: 26,
        }),
        storage: Some((4_100_000_000, 6_200_000_000)),
        usb: Some(Usb::Network),
        sync: Some("anna at http://192.168.15.201:17200"),
        catalogs: 2,
        exit_message: None,
        clock_style: 0,
        home_stats: true,
        home_quote: true,
        home_pet: true,
        font: "系统默认 (Noto Sans)",
        refresh_interval: 1,
        wifi_transfer: "未开启",
        account: None,
        sleep_screen_cover: false,
    };
    let mut text = TextEngine::new(fonts());
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let (tree, ids) = settings_screen::Settings::build(&mut text, &styles, &view);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    ids.sync(&mut ui.tree);
    day_night(&mut ui, out, "settings");
    if aristotle_i18n::language() == "zh" {
        if let Some(ref_path) = find_reference("settings.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-settings.png")
            } else {
                out.join("compare-settings.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }
    let mut page = 1;
    loop {
        let (p, count) = ui.tree.page(ids.list);
        if p + 1 >= count {
            break;
        }
        ids.handle(&mut ui.tree, settings_screen::action::NEXT_PAGE);
        ui.layout();
        ids.sync(&mut ui.tree);
        page += 1;
        let d = ui.render();
        log(&format!("settings: page {page}"), &ui, &d);
        day_night(&mut ui, out, &format!("settings-page{page}"));
    }
    // The screensaver on the reader's own picture: the row offers the
    // way to the page that picks it.
    let mut own = view.clone();
    own.sleep_screen = 4;
    let mut text = TextEngine::new(fonts());
    let (tree, ids) = settings_screen::Settings::build(&mut text, &styles, &own);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    // The second page holds the sleep screen row.
    ui.tree.set_page(ids.list, 1);
    ui.layout();
    ids.sync(&mut ui.tree);
    day_night(&mut ui, out, "settings-sleep-image");
    // Night mode on, as the toggle shows it, and charging in drive mode.
    let view = SystemView {
        night: true,
        light: Some((10, 24)),
        battery: Some(BatteryInfo {
            level: 0.47,
            charging: true,
            state: tr(Msg::SettingsBatteryCharging),
            voltage_mv: 4105,
            temperature_c: 28,
        }),
        status: StatusLine {
            charging: true,
            battery: Some(0.47),
            ..STATUS
        },
        usb: Some(Usb::Storage { minutes_left: 9 }),
        ..view
    };
    let mut text = TextEngine::new(fonts());
    let (tree, ids) = settings_screen::Settings::build(&mut text, &styles, &view);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    ids.sync(&mut ui.tree);
    ui.set_night(true);
    ui.render();
    qa_check(&ui, "settings-night");
    save(ui.screen(), &out.join("settings-on-night.png"));
    ids.handle(&mut ui.tree, settings_screen::action::NEXT_PAGE);
    ui.layout();
    ids.sync(&mut ui.tree);
    ui.render();
    save(ui.screen(), &out.join("settings-page2-charging-night.png"));
}

/// Settings › Network: sync, the catalogs, a feed and a book.
fn network_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let shown = |tree: Tree, text: TextEngine, f: &network::Frame| {
        let mut ui = Ui::with_screen(tree, text, profile);
        ui.layout();
        f.sync(&mut ui.tree);
        ui
    };
    let mut text = TextEngine::new(fonts());
    let v = SyncView {
        status: STATUS,
        server: Some("http://192.168.15.201:17200"),
        username: Some("anna"),
        has_key: true,
        auto: true,
        message: "Logged in as anna.",
        import_path: "/mnt/us/aristotle/sync.json",
    };
    let (tree, f) = network::build_sync(&mut text, &styles, &v);
    let mut ui = shown(tree, text, &f);
    day_night(&mut ui, out, "sync");
    // Flipping the switch and a new message: one partial update.
    let toggled = SyncView {
        auto: false,
        message: "Checking…",
        ..v
    };
    let mut text = TextEngine::new(fonts());
    let (tree, f) = network::build_sync(&mut text, &styles, &toggled);
    ui.set_tree_with(tree, aristotle_ui::tree::Refresh::Auto);
    ui.layout();
    f.sync(&mut ui.tree);
    let d = ui.render();
    log("sync: switch off, checking", &ui, &d);
    save(
        &damage_overlay(&mut ui, &d, "sync: switch off"),
        &out.join("sync-damage.png"),
    );

    let cats = network::sample::catalogs();
    let mut text = TextEngine::new(fonts());
    let v = CatalogsView {
        status: STATUS,
        catalogs: &cats,
        message: "",
        import_path: "/mnt/us/aristotle/catalogs.json",
    };
    let (tree, f) = network::build_catalogs(&mut text, &styles, &v);
    let mut ui = shown(tree, text, &f);
    day_night(&mut ui, out, "catalogs");

    let rows = network::sample::feed_rows();
    let mut text = TextEngine::new(fonts());
    let v = FeedView {
        status: STATUS,
        title: "calibre Library :: Newest",
        rows: &rows,
        searchable: true,
        more_before: false,
        more_after: true,
        message: "",
    };
    let (tree, f) = network::build_feed(&mut text, &styles, &v);
    let mut ui = shown(tree, text, &f);
    day_night(&mut ui, out, "catalog-feed");

    let formats = vec![
        "EPUB · 1.2 MB".to_string(),
        "AZW3".to_string(),
        "PDF".to_string(),
    ];
    let mut text = TextEngine::new(fonts());
    let v = EntryView {
        status: STATUS,
        title: "Lalka",
        authors: "Bolesław Prus",
        summary: "Warszawa, rok 1878. Stanisław Wokulski, kupiec galanteryjny, wraca z wojny bogaty i zakochany.",
        formats: &formats,
        downloaded: true,
        message: "Saved as Calibre/Lalka.epub.",
    };
    let (tree, f) = network::build_entry(&mut text, &styles, &v);
    let mut ui = shown(tree, text, &f);
    day_night(&mut ui, out, "catalog-entry");
}

/// A picture of the screensaver's own: a landscape of the kind the
/// reference shows, so the grid can be compared with it.
///
/// `seed` picks the shape of the hills, the sun and the ground.
fn sample_picture(w: u32, h: u32, seed: u8) -> GrayImage {
    let mut px = vec![0xffu8; (w * h) as usize];
    let sky = [0xf2u8, 0xe6, 0xdd, 0xf7];
    let ground = [0x99u8, 0x88, 0xbb, 0x77];
    let hills = [0x33u8, 0x11, 0x55, 0x22];
    let sun = 0xdd;
    let bg = sky[seed as usize % sky.len()];
    let gh = h * 3 / 5;
    let sun_x = w * (2 + seed as u32 % 5) / 8;
    let sun_y = h / 6;
    let sun_r = (w as f32 * 0.14) as u32;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            px[i] = bg;
            // The sun.
            let dx = x as i64 - sun_x as i64;
            let dy = y as i64 - sun_y as i64;
            if dx * dx + dy * dy <= (sun_r * sun_r) as i64 {
                px[i] = sun;
            }
            // The ground, then the hills that stand on it.
            if y >= gh {
                px[i] = ground[seed as usize % ground.len()];
            }
            // Two hills, of the kind the prototype draws: they stand on
            // the ground line and are tall in the middle.
            let peak = |px_: u32, base: u32, height: u32| -> bool {
                let half = (w / 6) as i64;
                let d = (x as i64 - px_ as i64).abs();
                let top = base as i64 - height as i64 + d * height as i64 / half;
                d < half && (y as i64) >= top && (y as i64) < base as i64
            };
            if peak(w / 4, gh, h / 4) {
                px[i] = hills[seed as usize % hills.len()];
            }
            if peak(w * 3 / 5, gh, h / 3) {
                px[i] = hills[(seed as usize + 1) % hills.len()];
            }
        }
    }
    GrayImage::new(w, h, px)
}

/// The page that picks the screensaver's own picture, and its empty
/// state, and the screensaver with a picture of the reader's own.
fn sleep_images_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let status = STATUS;

    // The file names of the reference picture.
    let names: [&str; 18] = if aristotle_i18n::language() == "zh" {
        [
            "山水.jpg",
            "海边.jpg",
            "星空.jpg",
            "竹林.jpg",
            "落日.jpg",
            "雪景.jpg",
            "湖面.jpg",
            "荷塘.jpg",
            "晨雾.jpg",
            "山谷.jpg",
            "海岸.jpg",
            "原野.jpg",
            "枫林.jpg",
            "夜港.jpg",
            "沙丘.jpg",
            "石桥.jpg",
            "芦苇.jpg",
            "梯田.jpg",
        ]
    } else {
        [
            "lake.jpg",
            "sea.jpg",
            "stars.jpg",
            "bamboo.jpg",
            "sunset.jpg",
            "snow.jpg",
            "bay.jpg",
            "lotus.jpg",
            "mist.jpg",
            "valley.jpg",
            "coast.jpg",
            "field.jpg",
            "maple.jpg",
            "harbour.jpg",
            "dunes.jpg",
            "bridge.jpg",
            "reeds.jpg",
            "terrace.jpg",
        ]
    };
    let pictures: Vec<sleep_images_screen::Picture> = names
        .iter()
        .enumerate()
        .map(|(i, name)| sleep_images_screen::Picture {
            name: (*name).to_string(),
            thumb: Some(sample_picture(270, 360, i as u8)),
            picked: false,
        })
        .collect();

    // The page with the pictures: the rotation is picked, so no picture
    // carries the tick.
    let view = sleep_images_screen::View {
        status,
        pictures: &pictures[..8],
        rotate_all: true,
        rotation_picked: true,
        current: None,
        page: 0,
        pages: 2,
    };
    let (tree, _) = sleep_images_screen::build(&styles, &view, profile.width as i32);
    let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
    ui.layout();
    day_night(&mut ui, out, "sleep-images");
    write_compare(&ui, out, "sleep-images.png", "sleep-images");

    // The same page with a picture picked instead of the rotation. The
    // rotation cell stays in the first cell, without its tick.
    let mut picked = pictures.clone();
    picked[4].picked = true;
    let view = sleep_images_screen::View {
        status,
        pictures: &picked[..8],
        rotate_all: true,
        rotation_picked: false,
        current: Some(names[4]),
        page: 0,
        pages: 2,
    };
    let (tree, _) = sleep_images_screen::build(&styles, &view, profile.width as i32);
    let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
    ui.layout();
    day_night(&mut ui, out, "sleep-images-picked");

    // The empty state: the folder holds no picture. The app's first page
    // still asks for the rotation cell, but there is none to draw.
    let view = sleep_images_screen::View {
        status,
        pictures: &[],
        rotate_all: true,
        rotation_picked: true,
        current: None,
        page: 0,
        pages: 1,
    };
    let (tree, _) = sleep_images_screen::build(&styles, &view, profile.width as i32);
    let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
    ui.layout();
    day_night(&mut ui, out, "sleep-images-empty");
    write_compare(&ui, out, "sleep-images-empty.png", "sleep-images-empty");

    // The screensaver with a picture of the reader's own.
    //
    // The night version is a tree of its own: the picture is not
    // inverted at night, so the tree is built with the night flag and
    // the day/night helper cannot be used here.
    let photo = sample_picture(1264, 1680, 3);
    let mut text = TextEngine::new(fonts());
    for night in [false, true] {
        let tree = sleep::build_image(&mut text, &styles, &photo, night);
        let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
        ui.layout();
        if night {
            ui.set_night(true);
            ui.render();
            qa_check(&ui, "sleep-image-night");
            save(ui.screen(), &out.join("sleep-image-night.png"));
        } else {
            if let Ok(mut screens) = qa_screens().lock() {
                screens.insert("sleep-image".to_string());
            }
            ui.render();
            qa_check(&ui, "sleep-image");
            save(ui.screen(), &out.join("sleep-image.png"));
        }
    }
}

fn sleep_screens(profile: Screen, out: &Path) {
    let books = if aristotle_i18n::language() == "zh" {
        home_books()
    } else {
        prototype_books_en()
    };
    let mut book = books[0].clone();
    book.cover = Some(sample_cover_at(560, 840, 0));
    let book = &book;
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);

    // 1. Classic full cover screensaver
    let mut ui = Ui::with_screen(
        sleep::build(&styles, Some(book)),
        TextEngine::new(fonts()),
        profile,
    );
    day_night(&mut ui, out, "screensaver");

    // 2. Cover + Daily quote
    let filter = sleep::QuoteFilter {
        mode: aristotle_ui::screens::home::quotes::QuoteMode::AllRandom,
        is_favorite: |_| false,
        is_hidden: |_| false,
        fixed_id: None,
    };
    let quote_content = sleep::resolve_sleep_content(
        sleep::SleepQuoteKind::DailyQuote,
        &[],
        &book.title,
        filter,
        &home_screen::quotes::QUOTES,
        0,
    );
    let mut ui_quote = Ui::with_screen(
        sleep::build_quote(&styles, Some(book), &quote_content),
        TextEngine::new(fonts()),
        profile,
    );
    ui_quote.layout();
    day_night(&mut ui_quote, out, "sleep-quote");

    if aristotle_i18n::language() == "zh" {
        if let Some(ref_path) = find_reference("sleep.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-sleep.png")
            } else {
                out.join("compare-sleep.png")
            };
            side_by_side(&ref_path, ui_quote.screen(), &target);
        }
    }

    // 3. Cover + Highlights
    let sample_highlights = ["质本洁来还洁去，强于污淖陷渠沟。"];
    let clip_content = sleep::resolve_sleep_content(
        sleep::SleepQuoteKind::Highlights,
        &sample_highlights,
        &book.title,
        filter,
        &home_screen::quotes::QUOTES,
        0,
    );
    let mut ui_clip = Ui::with_screen(
        sleep::build_quote(&styles, Some(book), &clip_content),
        TextEngine::new(fonts()),
        profile,
    );
    ui_clip.layout();
    day_night(&mut ui_clip, out, "sleep-clip");
}

/// Every icon at 72, 48 and 32 px with its name.
fn icons_sheet(profile: Screen, out: &Path) {
    let d = profile.density;
    let size = Size::new(profile.width as i32, profile.height as i32);
    let (w, h) = (size.w as u32, size.h as u32);
    let mut text = TextEngine::new(fonts());
    let mut c = Canvas::new(w, h);
    let all = Rect::new(0, 0, w as i32, h as i32);
    let cols = 6;
    let cell_w = w as i32 / cols;
    let cell_h = d.px(196);
    let top = d.px(60);
    let label = TextStyle::new(FamilyId(0), 24.0).at_density(d).gray(0x44);
    let head = TextStyle::new(FamilyId(0), 34.0).at_density(d).bold();
    {
        let l = set(
            &mut text,
            "Aristotle icons · 24-unit grid · 72, 48 and 32 px",
            head,
            w as f32,
        );
        let mut p = Painter::new(&mut c, all);
        l.draw(&mut text, &mut p, d.scale(40.0), d.scale(12.0));
    }
    for (i, icon) in Icon::ALL.iter().enumerate() {
        let (col, row) = (i as i32 % cols, i as i32 / cols);
        let x = col * cell_w;
        let y = top + row * cell_h;
        let mut p = Painter::new(&mut c, all);
        icons::draw(
            &mut p,
            *icon,
            Rect::new(x + (cell_w - d.px(72)) / 2, y + d.px(8), d.px(72), d.px(72)),
            0,
            0.0,
        );
        let base = y + d.px(96);
        icons::draw(
            &mut p,
            *icon,
            Rect::new(x + cell_w / 2 - d.px(56), base, d.px(48), d.px(48)),
            0,
            0.0,
        );
        icons::draw(
            &mut p,
            *icon,
            Rect::new(
                x + cell_w / 2 + d.px(16),
                base + d.px(8),
                d.px(32),
                d.px(32),
            ),
            0,
            0.0,
        );
        let l = set(&mut text, icon.name(), label.clone(), cell_w as f32);
        let lx = x as f32 + (cell_w as f32 - l.width) / 2.0;
        l.draw(&mut text, &mut p, lx, (base + d.px(54)) as f32);
    }
    save(&c, &out.join("icons.png"));
    save(&invert(&c), &out.join("icons-night.png"));
}

/// The typographic cards: four bindings, Chinese vertical strips, Western cards, and list sizes.
fn covers_sheet(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let fam = styles.family;
    let books = library::sample_books();
    let mut t = Tree::new();
    let mut large = Vec::new();
    // Pick one book per binding so the sheet shows all four bindings.
    let mut seen = Vec::new();
    for b in &books {
        let cover = Cover::new(&b.title, &b.author, fam).with_mark(b.mark());
        let binding = cover.binding();
        if !seen.contains(&binding) && seen.len() < 4 {
            seen.push(binding);
            let n = t.custom(cover);
            large.push(t.fixed(Some(220), Some(330), Some(n)));
        }
    }
    let large = t.add(Node::row(24), large);

    // Chinese typographic covers demonstrating column rules and bindings:
    // 1. <= 4 chars: 1 col (红楼梦, 3 chars)
    // 2. <= 4 chars: 1 col (三国演义, 4 chars)
    // 3. 5-8 chars: 2 cols split (傲慢与偏见, 5 chars)
    // 4. > 8 chars: 2 cols truncated with ellipsis
    let sample_cjk = [
        ("红楼梦", "曹雪芹"),
        ("三国演义", "罗贯中"),
        ("傲慢与偏见", "简·奥斯汀"),
        ("超长中文书名超出两列截断", "佚名"),
    ];
    let mut cjk_row = Vec::new();
    for (title, author) in sample_cjk {
        let n = t.custom(Cover::new(title, author, fam));
        cjk_row.push(t.fixed(Some(220), Some(330), Some(n)));
    }
    let cjk_row = t.add(Node::row(24), cjk_row);

    // Small cards: 5 Western sample books + 4 Chinese sample books (72x106 thumbnail size)
    let mut small = Vec::new();
    for b in books.iter().take(5) {
        let n = t.custom(Cover::new(&b.title, &b.author, fam).with_mark(b.mark()));
        small.push(t.fixed(Some(72), Some(106), Some(n)));
    }
    for (title, author) in sample_cjk {
        let n = t.custom(Cover::new(title, author, fam));
        small.push(t.fixed(Some(72), Some(106), Some(n)));
    }
    let small = t.add(Node::row(16), small);

    let caption = t.label(
        "Typographic cards: 4 bindings (#000, #333, #f2, white+stroke), vertical CJK strips",
        styles.small(),
    );
    let col = t.add(Node::column(28), [caption, large, cjk_row, small]);
    let root = t.pad(aristotle_ui::Insets::all(28), col);
    t.set_root(root);
    let mut ui = Ui::with_screen(t, TextEngine::new(fonts()), profile);
    day_night(&mut ui, out, "covers");
}

fn reader_screens(profile: Screen, out: &Path, fonts_dir: &Path) {
    let size = Size::new(profile.width as i32, profile.height as i32);
    let (w, h) = (size.w as u32, size.h as u32);
    let (mut text, families) = engine(fonts_dir);
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let settings = Settings {
        light: 0.0,
        ..Settings::default()
    };
    let (tree, mut r) = Reader::build_for_size(&mut text, &styles, families, settings, size);
    let mut ui = Ui::with_screen(tree, text, profile);
    {
        let d = ui.render();
        log("reader: first frame", &ui, &d)
    };
    println!("  chapter set in {} pages", r.page_count());
    let is_zh = aristotle_i18n::language() == "zh";
    day_night(&mut ui, out, "reader");
    if is_zh {
        if let Some(ref_path) = find_reference("reader.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-reader.png")
            } else {
                out.join("compare-reader.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }

    // Progress bar preview at 40%:
    {
        let pb = ui.tree.add(
            aristotle_ui::tree::Node::new(aristotle_ui::tree::Kind::Custom(Box::new(
                aristotle_ui::widgets::ProgressBar::new(40),
            ))),
            [],
        );
        let pb_pad = ui
            .tree
            .pad(Insets::new(0, 0, profile.density.px(24), 0), pb);
        let pb_align = ui.tree.align(0.5, 1.0, pb_pad);
        let old_root = ui.tree.root().expect("root exists");
        let preview_root = ui.tree.add(
            aristotle_ui::tree::Node::new(aristotle_ui::tree::Kind::Stack),
            [old_root, pb_align],
        );
        ui.tree.set_root(preview_root);
        ui.render();
        save(ui.screen(), &out.join("progress_preview_40.png"));
        ui.tree.set_root(old_root);
        ui.render();
    }

    // Tap zones overlay.
    let mut z = ui.screen().clone();
    for (zone, rect) in reader::zones(ui.bounds()) {
        let name = match zone {
            Zone::Prev => "Previous",
            Zone::Next => "Next page",
            Zone::Menu => "Menu",
        };
        let style = TextStyle::new(FamilyId(0), 44.0)
            .at_density(profile.density)
            .bold()
            .gray(255);
        let l = set(&mut ui.text, name, style, 600.0);
        let mut p = Painter::new(&mut z, Rect::new(0, 0, w as i32, h as i32));
        dashed_rect(&mut p, rect.outset(-6), 6, 24, 0);
        let c = rect.center();
        let tag = Rect::new(
            c.x - l.width as i32 / 2 - 24,
            c.y - l.height as i32 / 2 - 10,
            l.width as i32 + 48,
            l.height as i32 + 20,
        );
        p.fill_round_rect(tag, 16.0, 0);
        l.draw(
            &mut ui.text,
            &mut p,
            tag.x as f32 + 24.0,
            tag.y as f32 + 10.0,
        );
    }
    save(&z, &out.join("reader-zones.png"));

    // Turn the page with a tap in the right zone.
    for ev in tap(
        &mut ui,
        Point {
            x: size.w * 4 / 5,
            y: size.h * 3 / 5,
        },
        0,
    ) {
        r.handle(&mut ui, ev);
    }
    let d = ui.render();
    log("reader: page turn", &ui, &d);
    save(ui.screen(), &out.join("reader-page2.png"));
    let o = damage_overlay(
        &mut ui,
        &d,
        "Page turn: text and footer; on the device one full mode-9 update",
    );
    save(&o, &out.join("reader-turn-damage.png"));

    // Back to page 1 with a swipe to the right.
    for (ph, x, ms) in [
        (Phase::Down, size.w / 4, 2000),
        (Phase::Move, size.w / 2, 2050),
        (Phase::Up, size.w * 3 / 4, 2100),
    ] {
        for ev in touch(&mut ui, ph, x, size.h / 2, ms) {
            r.handle(&mut ui, ev);
        }
    }
    ui.render();

    // Open the menu from the top band.
    for ev in tap(
        &mut ui,
        Point {
            x: size.w / 2,
            y: profile.density.px(70),
        },
        3000,
    ) {
        r.handle(&mut ui, ev);
    }
    let d = ui.render();
    log("reader: open menu", &ui, &d);
    day_night(&mut ui, out, "reader-menu");
    if is_zh {
        if let Some(ref_path) = find_reference("readermenu.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-readermenu.png")
            } else {
                out.join("compare-readermenu.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }
    let o = damage_overlay(
        &mut ui,
        &d,
        "Opening the menu repaints the two bars; the page between them stays",
    );
    save(&o, &out.join("reader-menu-damage.png"));

    // Press feedback inside the menu, on the larger-text button.
    let size_row = ui.tree.rect(r.night_toggle);
    let probe = Point {
        x: w as i32 - profile.density.px(110),
        y: size_row.y - profile.density.px(410),
    };
    if let Hit::Target(node) = ui.tree.hit(probe) {
        let c = ui.tree.rect(node).center();
        touch(&mut ui, Phase::Down, c.x, c.y, 4000);
        let d = ui.render();
        log("reader menu: press", &ui, &d);
        save(ui.screen(), &out.join("reader-menu-pressed.png"));
        touch(
            &mut ui,
            Phase::Move,
            c.x,
            c.y + profile.density.px(300),
            4040,
        );
        touch(&mut ui, Phase::Up, c.x, c.y + profile.density.px(300), 4080);
        ui.render();
    }

    // Night mode with the menu toggle, then close the menu.
    let night = ui.tree.rect(r.night_toggle).center();
    for ev in tap(&mut ui, night, 5000) {
        r.handle(&mut ui, ev);
    }
    {
        let d = ui.render();
        log("reader: night mode from the menu", &ui, &d)
    };
    save(ui.screen(), &out.join("reader-menu-night-on.png"));
    let children = &ui.tree.node(r.menu).children;
    let gap = ui.tree.rect(children[1]).center();
    for ev in tap(&mut ui, gap, 6000) {
        r.handle(&mut ui, ev);
    }
    ui.render();

    // Alternative typography: Charis, one size up, loose spacing.
    let (mut text, families) = engine(fonts_dir);
    let s = Settings {
        family: 1,
        size: 4,
        spacing: 2,
        ..Default::default()
    };
    let (tree, _) = Reader::build_for_size(&mut text, &styles, families, s, size);
    let mut ui = Ui::with_screen(tree, text, profile);
    day_night(&mut ui, out, "reader-charis");
}

fn typeset_screens(profile: Screen, out: &Path, fonts_dir: &Path) {
    use aristotle_ui::screens::typeset::{self, FontOption, TypesetView};
    let is_zh = aristotle_i18n::language() == "zh";
    let (text, [lit, charis, gentium]) = engine(fonts_dir);
    let styles = Styles::new(lit).with_density(profile.density);

    let view = TypesetView {
        title: "红楼梦".into(),
        author: "曹雪芹".into(),
        cjk_fonts: vec![FontOption {
            name: "思源黑体".into(),
            sample_font: lit,
        }],
        selected_cjk_font: 0,
        cjk_page: 0,
        latin_fonts: vec![
            FontOption {
                name: "Literata".into(),
                sample_font: lit,
            },
            FontOption {
                name: "Charis".into(),
                sample_font: charis,
            },
            FontOption {
                name: "Gentium".into(),
                sample_font: gentium,
            },
        ],
        selected_latin_font: 0,
        latin_page: 0,
        size_index: 3,      // 44 px (prototype default)
        spacing_index: 1,   // 标准 (1.7)
        margin_index: 1,    // 标准 (88 px)
        indent_index: 0,    // 2 字
        script_index: 0,    // 简体
        gap_index: 2,       // 大 (0.6)
        underline_index: 0, // 无
        trim_index: 0,      // 开
        anim_enabled: false,
        anim_speed: 1, // 标准
    };

    let mut tree = Tree::new();
    let (root, _) = typeset::build_typeset_screen(&mut tree, &styles, &view);
    tree.set_root(root);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "readertypeset");

    if is_zh {
        if let Some(ref_path) = find_reference("readertypeset.png") {
            let target = if out.ends_with("zh") {
                out.parent()
                    .unwrap_or(out)
                    .join("compare-readertypeset.png")
            } else {
                out.join("compare-readertypeset.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }
}

/// The centre of every visible control of `ui`, as `screen action x y w h`
/// lines, for writing device scenarios with real coordinates.
fn dump_targets(ui: &mut Ui, screen: &str, out: &mut String) {
    ui.layout();
    qa_check(ui, &format!("targets-{screen}"));
    for i in 0..ui.tree.len() {
        let id = aristotle_ui::tree::NodeId(i as u32);
        let Some(action) = ui.tree.node(id).action else {
            continue;
        };
        let r = ui.tree.rect(id);
        if r.is_empty() {
            continue;
        }
        let c = r.center();
        out.push_str(&format!(
            "{screen} {action} {} {} {} {}\n",
            c.x, c.y, r.w, r.h
        ));
    }
}

/// Control coordinates of the app's screens with the sample library: home
/// (books, folders, series), a folder, settings pages and the app's reader
/// menu (no font row, night row on).
fn targets(profile: Screen, out: &Path) {
    use aristotle_ui::screens::reader::{build_menu, MenuSpec};
    let mut s = String::from("# screen action-id centre-x centre-y width height\n");
    let books = library::sample_books();
    for view in View::ALL {
        let items = sample_items(
            if view == View::Books {
                &books[1..]
            } else {
                &books
            },
            view,
        );
        let (mut ui, _) = shelf_ui(
            profile,
            &Shelf {
                place: Place::Home {
                    view,
                    filter: Filter::All,
                },
                hero: Some(&books[0]),
                items: &items,
                sort: tr(Msg::LibrarySortRecent),
                status: STATUS,
                covers: CoverMode::None,
                layout: LibraryLayout::List,
                local_books_count: items.len(),
            },
        );
        dump_targets(&mut ui, &format!("library-{view:?}"), &mut s);
    }
    let platon: Vec<Item> = books
        .iter()
        .filter(|b| b.folder.as_deref() == Some("Platon"))
        .cloned()
        .map(Item::Book)
        .collect();
    let (mut ui, _) = shelf_ui(
        profile,
        &Shelf {
            place: Place::Folder { name: "Platon" },
            hero: None,
            items: &platon,
            sort: tr(Msg::LibrarySortTitle),
            status: STATUS,
            covers: CoverMode::None,
            layout: LibraryLayout::List,
            local_books_count: platon.len(),
        },
    );
    dump_targets(&mut ui, "folder", &mut s);
    let view = SystemView {
        status: STATUS,
        light: Some((20, 24)),
        night: false,
        covers: CoverMode::Cards,
        sleep_minutes: Some(15),
        sleep_screen: 0,
        clippings: true,
        battery: Some(BatteryInfo {
            level: 0.82,
            charging: false,
            state: tr(Msg::SettingsBatteryDischarging),
            voltage_mv: 3980,
            temperature_c: 26,
        }),
        storage: Some((4_100_000_000, 6_200_000_000)),
        usb: Some(Usb::Network),
        sync: Some("anna at http://192.168.15.201:17200"),
        catalogs: 2,
        exit_message: None,
        clock_style: 0,
        home_stats: true,
        home_quote: true,
        home_pet: true,
        font: "系统默认 (Noto Sans)",
        refresh_interval: 1,
        wifi_transfer: "未开启",
        account: None,
        sleep_screen_cover: false,
    };
    let mut text = TextEngine::new(fonts());
    let (tree, ids) = settings_screen::Settings::build(
        &mut text,
        &Styles::new(FamilyId(0)).with_density(profile.density),
        &view,
    );
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    dump_targets(&mut ui, "settings-1", &mut s);
    ids.handle(&mut ui.tree, settings_screen::action::NEXT_PAGE);
    dump_targets(&mut ui, "settings-2", &mut s);
    ids.handle(&mut ui.tree, settings_screen::action::NEXT_PAGE);
    dump_targets(&mut ui, "settings-3", &mut s);
    // The network screens, with a sync account and two catalogs.
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let mut text = TextEngine::new(fonts());
    let v = SyncView {
        status: STATUS,
        server: Some("http://192.168.15.201:17200"),
        username: Some("anna"),
        has_key: true,
        auto: true,
        message: "",
        import_path: "/mnt/us/aristotle/sync.json",
    };
    let (tree, _) = network::build_sync(&mut text, &styles, &v);
    let mut ui = Ui::with_screen(tree, text, profile);
    dump_targets(&mut ui, "sync", &mut s);
    let cats = network::sample::catalogs();
    let mut text = TextEngine::new(fonts());
    let v = CatalogsView {
        status: STATUS,
        catalogs: &cats,
        message: "",
        import_path: "/mnt/us/aristotle/catalogs.json",
    };
    let (tree, _) = network::build_catalogs(&mut text, &styles, &v);
    let mut ui = Ui::with_screen(tree, text, profile);
    dump_targets(&mut ui, "catalogs", &mut s);
    let rows = network::sample::feed_rows();
    let mut text = TextEngine::new(fonts());
    let v = FeedView {
        status: STATUS,
        title: "Feed",
        rows: &rows,
        searchable: true,
        more_before: false,
        more_after: true,
        message: "",
    };
    let (tree, _) = network::build_feed(&mut text, &styles, &v);
    let mut ui = Ui::with_screen(tree, text, profile);
    dump_targets(&mut ui, "catalog-feed", &mut s);
    let formats = vec!["EPUB · 1.2 MB".to_string(), "PDF".to_string()];
    let mut text = TextEngine::new(fonts());
    let v = EntryView {
        status: STATUS,
        title: "Apology",
        authors: "Plato",
        summary: "A public-domain test book.",
        formats: &formats,
        downloaded: false,
        message: "",
    };
    let (tree, _) = network::build_entry(&mut text, &styles, &v);
    let mut ui = Ui::with_screen(tree, text, profile);
    dump_targets(&mut ui, "catalog-entry", &mut s);
    let mut t = Tree::new();
    let page = t.spacer(1);
    let spec = MenuSpec {
        title: "Fedon",
        families: None,
        font_list: true,
        night: true,
        sync: true,
        top_extra: 0,
        bottom_extra: 0,
    };
    let menu = build_menu(
        &mut t,
        &Styles::new(FamilyId(0)).with_density(profile.density),
        &spec,
        &Settings::default(),
    );
    let root = t.add(
        Node::new(aristotle_ui::tree::Kind::Stack),
        [page, menu.root],
    );
    t.set_root(root);
    t.set_hidden(menu.root, false);
    let mut ui = Ui::with_screen(t, TextEngine::new(fonts()), profile);
    dump_targets(&mut ui, "reader-menu", &mut s);
    let path = out.join("targets.txt");
    std::fs::write(&path, s).expect("targets.txt");
    println!("wrote {}", path.display());
}

/// `$ARISTOTLE_CACHE`, defaulting as `tools/env.sh` does.
fn cache_dir() -> PathBuf {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    var("ARISTOTLE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            var("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(var("HOME").expect("HOME is set")).join(".cache"))
                .join("aristotle")
        })
}

fn keyboard_search_screens(profile: Screen, out: &Path) {
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let mut books = library::sample_books();
    with_covers(&mut books, &[0, 1, 3]);
    let index = SearchIndex::build(&books);

    // 1. Chinese mode letters page with uncommitted pinyin and candidates,
    // and live search results matching 《紅樓夢》 (hong)
    {
        let mut state = KeyboardState::new();
        state.set_mode(InputMode::Chinese);
        for c in "hong".chars() {
            state.press_key(c);
        }
        let mut tree = Tree::new();
        let (_root, _ui) = build_search_screen(
            &mut tree,
            &styles,
            &books,
            &mut |i| books.get(i).and_then(|b| b.cover.clone()),
            &index,
            &state,
            &STATUS,
        );
        let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
        ui.layout();
        day_night(&mut ui, out, "keyboard-zh-letters");
    }

    // 2. English mode letters page with committed text
    {
        let mut state = KeyboardState::new();
        state.set_mode(InputMode::English);
        state.committed = "Tolstoy".to_string();
        let mut tree = Tree::new();
        let (_root, _ui) = build_search_screen(
            &mut tree,
            &styles,
            &books,
            &mut |i| books.get(i).and_then(|b| b.cover.clone()),
            &index,
            &state,
            &STATUS,
        );
        let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
        ui.layout();
        day_night(&mut ui, out, "keyboard-en-letters");
    }

    // 3. Number/Symbols page
    {
        let mut state = KeyboardState::new();
        state.toggle_page();
        state.committed = "123".to_string();
        let mut tree = Tree::new();
        let (_root, _ui) = build_search_screen(
            &mut tree,
            &styles,
            &books,
            &mut |i| books.get(i).and_then(|b| b.cover.clone()),
            &index,
            &state,
            &STATUS,
        );
        let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
        ui.layout();
        day_night(&mut ui, out, "keyboard-symbols");
    }

    // 4. Candidate pagination (page 2 of candidates)
    {
        let mut state = KeyboardState::new();
        state.set_mode(InputMode::Chinese);
        for c in "hong".chars() {
            state.press_key(c);
        }
        state.next_cand_page();
        let mut tree = Tree::new();
        let (_root, _ui) = build_search_screen(
            &mut tree,
            &styles,
            &books,
            &mut |i| books.get(i).and_then(|b| b.cover.clone()),
            &index,
            &state,
            &STATUS,
        );
        let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
        ui.layout();
        day_night(&mut ui, out, "keyboard-zh-cand-page2");
    }

    // 5. Search results when selecting simplified "红" matching traditional 《紅樓夢》
    {
        let mut state = KeyboardState::new();
        state.committed = "红".to_string();
        let mut tree = Tree::new();
        let (_root, _ui) = build_search_screen(
            &mut tree,
            &styles,
            &books,
            &mut |i| books.get(i).and_then(|b| b.cover.clone()),
            &index,
            &state,
            &STATUS,
        );
        let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
        ui.layout();
        day_night(&mut ui, out, "search-results");
    }

    // 6. Search results with uncommitted pinyin (e.g. typing "hong" matching 《紅樓夢》)
    {
        let mut state = KeyboardState::new();
        state.set_mode(InputMode::Chinese);
        for c in "hong".chars() {
            state.press_key(c);
        }
        let mut tree = Tree::new();
        let (_root, _ui) = build_search_screen(
            &mut tree,
            &styles,
            &books,
            &mut |i| books.get(i).and_then(|b| b.cover.clone()),
            &index,
            &state,
            &STATUS,
        );
        let mut ui = Ui::with_screen(tree, TextEngine::new(fonts()), profile);
        ui.layout();
        day_night(&mut ui, out, "search-pinyin-uncommitted");
    }
}

/// Writes the page the list actually paginated to into its pager label; the
/// preview does not go through the app's screen builder.
fn scan_pagers(ui: &mut Ui, view: &scan_screen::ScanUi) {
    for (list, pager) in [
        (view.folder_list, view.folder_pager),
        (view.review_list, view.review_pager),
    ] {
        if let (Some(list), Some(pager)) = (list, pager) {
            let (page, count) = ui.tree.page(list);
            let text = tf!(Msg::LibraryPagerPageOf, page = page + 1, count = count);
            ui.tree.set_text(pager, &text);
        }
    }
    ui.layout();
}

fn scan_screens(profile: Screen, out: &Path) {
    let status = STATUS;
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);

    // 1. Empty state
    let empty_screen = scan_screen::ScanScreen {
        status,
        state: scan_screen::ScanState::Empty,
        local_books_count: 0,
    };
    let mut text = TextEngine::new(fonts());
    let (tree, _) = scan_screen::ScanUi::build(&mut text, &styles, &empty_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "scan-empty");

    // 2. Scanning state
    let is_zh = aristotle_i18n::language() == "zh";
    let scanned_books = if is_zh {
        vec![
            scan_screen::ScannedBook {
                title: "儒林外史".into(),
                format: "MOBI".into(),
            },
            scan_screen::ScannedBook {
                title: "浮生六记".into(),
                format: "EPUB".into(),
            },
            scan_screen::ScannedBook {
                title: "聊斋志异".into(),
                format: "PDF".into(),
            },
            scan_screen::ScannedBook {
                title: "世说新语".into(),
                format: "TXT".into(),
            },
            scan_screen::ScannedBook {
                title: "老残游记".into(),
                format: "EPUB".into(),
            },
        ]
    } else {
        vec![
            scan_screen::ScannedBook {
                title: "Pride and Prejudice".into(),
                format: "EPUB".into(),
            },
            scan_screen::ScannedBook {
                title: "Moby Dick".into(),
                format: "MOBI".into(),
            },
            scan_screen::ScannedBook {
                title: "Frankenstein".into(),
                format: "PDF".into(),
            },
            scan_screen::ScannedBook {
                title: "Dracula".into(),
                format: "TXT".into(),
            },
            scan_screen::ScannedBook {
                title: "The Time Machine".into(),
                format: "EPUB".into(),
            },
        ]
    };
    let current_file = if is_zh {
        "镜花缘.epub".into()
    } else {
        "pride_and_prejudice.epub".into()
    };
    let scan_status = if is_zh {
        "正在读取封面与阅读进度".to_string()
    } else {
        tr(Msg::LibraryScanReadingMeta).to_string()
    };

    let scanning_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Scanning {
            path: "/mnt/us".into(),
            subdirs: true,
            found_count: 5,
            current_file,
            progress: 0.58,
            status: scan_status,
            scanned_books,
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, _) = scan_screen::ScanUi::build(&mut text, &styles, &scanning_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "scan-scanning");

    if is_zh {
        if let Some(ref_path) = find_reference("libraryscan.png") {
            let target = if out.ends_with("zh") {
                out.parent().unwrap_or(out).join("compare-libraryscan.png")
            } else {
                out.join("compare-libraryscan.png")
            };
            side_by_side(&ref_path, ui.screen(), &target);
        }
    }

    // 3. The source picker: several folders, or the whole store.
    let folder_names: Vec<(&str, &str, usize)> = if is_zh {
        vec![
            ("小说", "documents/小说", 24),
            ("诗歌", "documents/诗歌", 3),
            ("历史", "documents/历史", 7),
            ("未分类", "documents/未分类", 1),
        ]
    } else {
        vec![
            ("Novels", "documents/Novels", 24),
            ("Poetry", "documents/Poetry", 3),
            ("History", "documents/History", 7),
            ("Unsorted", "documents/Unsorted", 1),
        ]
    };
    let folders: Vec<scan_screen::ScopeFolder> = folder_names
        .iter()
        .map(|(name, relative, books)| scan_screen::ScopeFolder {
            name: (*name).into(),
            relative: (*relative).into(),
            books: *books,
        })
        .collect();
    let destination = if is_zh {
        "/mnt/us/documents/墨阅书库"
    } else {
        "/mnt/us/documents/My Library"
    };
    let sources_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Sources {
            mode: scan_screen::PickerMode::Sources,
            path: "documents".into(),
            folders: folders.clone(),
            can_go_up: true,
            selected: if is_zh {
                vec!["documents/小说".into(), "documents/诗歌".into()]
            } else {
                vec!["documents/Novels".into(), "documents/Poetry".into()]
            },
            whole: false,
            destination: destination.into(),
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, view) = scan_screen::ScanUi::build(&mut text, &styles, &sources_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    scan_pagers(&mut ui, &view);
    day_night(&mut ui, out, "scan-sources");

    // The destination picker: one folder, the same list without checkboxes.
    let destination_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Sources {
            mode: scan_screen::PickerMode::Destination,
            path: "documents".into(),
            folders,
            can_go_up: true,
            selected: Vec::new(),
            whole: false,
            destination: destination.into(),
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, view) = scan_screen::ScanUi::build(&mut text, &styles, &destination_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    scan_pagers(&mut ui, &view);
    day_night(&mut ui, out, "scan-destination");

    // 4. The combined review: nothing is checked until the reader checks it.
    let review_books = if is_zh {
        vec![
            scan_screen::ReviewBook {
                title: "儒林外史".into(),
                path: "documents/小说/儒林外史.txt".into(),
                format: "TXT".into(),
                id: None,
                in_destination: false,
            },
            scan_screen::ReviewBook {
                title: "浮生六记".into(),
                path: "documents/小说/浮生六记.epub".into(),
                format: "EPUB".into(),
                id: None,
                in_destination: false,
            },
            scan_screen::ReviewBook {
                title: "聊斋志异".into(),
                path: "documents/历史/聊斋志异.txt".into(),
                format: "TXT".into(),
                id: Some("indexed".into()),
                in_destination: false,
            },
            scan_screen::ReviewBook {
                title: "世说新语".into(),
                path: "documents/墨阅书库/世说新语.epub".into(),
                format: "EPUB".into(),
                id: Some("indexed-in-place".into()),
                in_destination: true,
            },
            scan_screen::ReviewBook {
                title: "老残游记".into(),
                path: "documents/诗歌/老残游记.pdf".into(),
                format: "PDF".into(),
                id: None,
                in_destination: false,
            },
        ]
    } else {
        vec![
            scan_screen::ReviewBook {
                title: "Pride and Prejudice".into(),
                path: "documents/Novels/pride.txt".into(),
                format: "TXT".into(),
                id: None,
                in_destination: false,
            },
            scan_screen::ReviewBook {
                title: "Moby Dick".into(),
                path: "documents/Novels/moby.epub".into(),
                format: "EPUB".into(),
                id: None,
                in_destination: false,
            },
            scan_screen::ReviewBook {
                title: "Frankenstein".into(),
                path: "documents/History/frankenstein.txt".into(),
                format: "TXT".into(),
                id: Some("indexed".into()),
                in_destination: false,
            },
            scan_screen::ReviewBook {
                title: "Dracula".into(),
                path: "documents/My Library/dracula.epub".into(),
                format: "EPUB".into(),
                id: Some("indexed-in-place".into()),
                in_destination: true,
            },
            scan_screen::ReviewBook {
                title: "The Time Machine".into(),
                path: "documents/Poetry/time.pdf".into(),
                format: "PDF".into(),
                id: None,
                in_destination: false,
            },
        ]
    };
    let selected = vec![true, false, true, false, false];
    let review_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Review {
            sources: if is_zh {
                "documents/小说, documents/诗歌".into()
            } else {
                "documents/Novels, documents/Poetry".into()
            },
            destination: destination.into(),
            books: review_books,
            selected,
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, view) = scan_screen::ScanUi::build(&mut text, &styles, &review_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    scan_pagers(&mut ui, &view);
    day_night(&mut ui, out, "scan-review");

    // The same review with nothing checked: the confirmation is disabled.
    let empty_review = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Review {
            sources: if is_zh {
                "documents/小说, documents/诗歌".into()
            } else {
                "documents/Novels, documents/Poetry".into()
            },
            destination: destination.into(),
            books: if is_zh {
                vec![
                    scan_screen::ReviewBook {
                        title: "儒林外史".into(),
                        path: "documents/小说/儒林外史.txt".into(),
                        format: "TXT".into(),
                        id: None,
                        in_destination: false,
                    },
                    scan_screen::ReviewBook {
                        title: "浮生六记".into(),
                        path: "documents/小说/浮生六记.epub".into(),
                        format: "EPUB".into(),
                        id: None,
                        in_destination: false,
                    },
                ]
            } else {
                vec![
                    scan_screen::ReviewBook {
                        title: "Pride and Prejudice".into(),
                        path: "documents/Novels/pride.txt".into(),
                        format: "TXT".into(),
                        id: None,
                        in_destination: false,
                    },
                    scan_screen::ReviewBook {
                        title: "Moby Dick".into(),
                        path: "documents/Novels/moby.epub".into(),
                        format: "EPUB".into(),
                        id: None,
                        in_destination: false,
                    },
                ]
            },
            selected: vec![false, false],
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, view) = scan_screen::ScanUi::build(&mut text, &styles, &empty_review);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    scan_pagers(&mut ui, &view);
    day_night(&mut ui, out, "scan-review-none");

    // 5. Importing, and the report.
    let importing_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Importing {
            destination: destination.into(),
            done: 2,
            total: 5,
            status: tr(Msg::LibraryScanImportingStatus).to_string(),
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, _) = scan_screen::ScanUi::build(&mut text, &styles, &importing_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "scan-importing");

    let imported_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Imported {
            destination: destination.into(),
            migrated: 4,
            failed: 1,
            cancelled: false,
            errors: if is_zh {
                vec!["documents/诗歌/老残游记.pdf: 文件已变化".into()]
            } else {
                vec!["documents/Poetry/time.pdf: the file changed".into()]
            },
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, _) = scan_screen::ScanUi::build(&mut text, &styles, &imported_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "scan-imported");

    // 3. Done state
    let (tiles, error, added_books) = if is_zh {
        let tiles = [
            scan_screen::ScanTile {
                label: "新增".into(),
                value: "12 本".into(),
            },
            scan_screen::ScanTile {
                label: "更新".into(),
                value: "2 本".into(),
            },
            scan_screen::ScanTile {
                label: "移除".into(),
                value: "1 本".into(),
            },
            scan_screen::ScanTile {
                label: "忽略非书文件".into(),
                value: "5 个".into(),
            },
            scan_screen::ScanTile {
                label: "导入原厂阅读进度".into(),
                value: "3 本".into(),
            },
            scan_screen::ScanTile {
                label: "耗时".into(),
                value: "8 秒".into(),
            },
        ];
        let error = Some(scan_screen::ScanError {
            count: 1,
            filename: "老照片集.cbz".into(),
            reason: "损坏的压缩包".into(),
        });
        let added_books = vec![
            scan_screen::AddedBook {
                title: "儒林外史".into(),
                author: "吴敬梓".into(),
                format: "MOBI".into(),
                cover: Some(sample_cover(0)),
            },
            scan_screen::AddedBook {
                title: "浮生六记".into(),
                author: "沈复".into(),
                format: "EPUB".into(),
                cover: None,
            },
            scan_screen::AddedBook {
                title: "聊斋志异".into(),
                author: "蒲松龄".into(),
                format: "PDF".into(),
                cover: Some(sample_cover(2)),
            },
            scan_screen::AddedBook {
                title: "世说新语".into(),
                author: "刘义庆".into(),
                format: "TXT".into(),
                cover: None,
            },
        ];
        (tiles, error, added_books)
    } else {
        let tiles = [
            scan_screen::ScanTile {
                label: "Added".into(),
                value: "12 books".into(),
            },
            scan_screen::ScanTile {
                label: "Updated".into(),
                value: "2 books".into(),
            },
            scan_screen::ScanTile {
                label: "Removed".into(),
                value: "1 book".into(),
            },
            scan_screen::ScanTile {
                label: "Ignored non-books".into(),
                value: "5 files".into(),
            },
            scan_screen::ScanTile {
                label: "Imported progress".into(),
                value: "3 books".into(),
            },
            scan_screen::ScanTile {
                label: "Time".into(),
                value: "8 s".into(),
            },
        ];
        let error = Some(scan_screen::ScanError {
            count: 1,
            filename: "old_photos.cbz".into(),
            reason: "corrupted archive".into(),
        });
        let added_books = vec![
            scan_screen::AddedBook {
                title: "Pride and Prejudice".into(),
                author: "Jane Austen".into(),
                format: "EPUB".into(),
                cover: Some(sample_cover(0)),
            },
            scan_screen::AddedBook {
                title: "Moby Dick".into(),
                author: "Herman Melville".into(),
                format: "MOBI".into(),
                cover: None,
            },
            scan_screen::AddedBook {
                title: "Frankenstein".into(),
                author: "Mary Shelley".into(),
                format: "PDF".into(),
                cover: None,
            },
            scan_screen::AddedBook {
                title: "Dracula".into(),
                author: "Bram Stoker".into(),
                format: "TXT".into(),
                cover: None,
            },
        ];
        (tiles, error, added_books)
    };

    let done_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Done {
            tiles,
            error,
            added_books,
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, _) = scan_screen::ScanUi::build(&mut text, &styles, &done_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "scan-done");

    let failed_screen = scan_screen::ScanScreen {
        status,
        local_books_count: 0,
        state: scan_screen::ScanState::Done {
            tiles: scan_screen::failed_tiles(),
            error: Some(scan_screen::ScanError {
                count: 1,
                filename: tr(Msg::LibraryScanFailed).to_string(),
                reason: tr(Msg::LibraryScanStopped).to_string(),
            }),
            added_books: Vec::new(),
        },
    };
    let mut text = TextEngine::new(fonts());
    let (tree, _) = scan_screen::ScanUi::build(&mut text, &styles, &failed_screen);
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    day_night(&mut ui, out, "scan-failed");
}

fn render_all(profile: Screen, out: &Path) {
    std::fs::create_dir_all(out).expect("output directory");
    let fonts_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    icons_sheet(profile, out);
    covers_sheet(profile, out);
    section_screens(profile, out);
    status_screens(profile, out);
    marks_screens(profile, out);
    calendar_screens(profile, out);
    book_stats_screens(profile, out);
    library_screens(profile, out);
    cloud_screens(profile, out);
    library_actions(profile, out);
    scan_screens(profile, out);
    wifi_screens(profile, out);
    settings_screens(profile, out);
    network_screens(profile, out);
    sleep_screens(profile, out);
    sleep_images_screens(profile, out);
    reader_screens(profile, out, &fonts_dir);
    typeset_screens(profile, out, &fonts_dir);
    keyboard_search_screens(profile, out);
    targets(profile, out);
    qa_write(out);
}

fn parse_size(value: &str) -> Result<Size, &'static str> {
    let (w, h) = value.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let w = w.parse::<i32>().map_err(|_| "invalid width")?;
    let h = h.parse::<i32>().map_err(|_| "invalid height")?;
    if !(600..=4096).contains(&w) || !(800..=4096).contains(&h) {
        return Err("width must be 600..4096, height must be 800..4096");
    }
    Ok(Size::new(w, h))
}

fn main() {
    let mut profile = Screen::new(
        SCREEN_WIDTH as u32,
        SCREEN_HEIGHT as u32,
        aristotle_screen::REFERENCE_DPI,
    )
    .unwrap();
    let mut lang = String::from("en");
    let mut out = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        if a == "--size" {
            let value = args.next().expect("--size needs WIDTHxHEIGHT");
            let size = parse_size(&value.to_string_lossy()).expect("invalid --size");
            profile = Screen::new(size.w as u32, size.h as u32, profile.density.dpi()).unwrap();
        } else if a == "--dpi" {
            let value = args.next().expect("--dpi needs a density");
            let dpi = value.to_string_lossy().parse().expect("invalid --dpi");
            profile = Screen::new(profile.width, profile.height, dpi).expect("invalid --dpi");
        } else if a == "--lang" {
            let v = args.next().expect("--lang needs a language code or `all`");
            lang = v.to_string_lossy().into_owned();
        } else {
            out = Some(PathBuf::from(a));
        }
    }
    let out = out.unwrap_or_else(|| cache_dir().join("ui-preview/v2"));
    if lang == "all" {
        for l in aristotle_i18n::locales() {
            aristotle_i18n::set_language(l.code);
            render_all(profile, &out.join(l.code));
        }
    } else {
        let codes: Vec<&str> = aristotle_i18n::locales().iter().map(|l| l.code).collect();
        assert!(
            aristotle_i18n::set_language(&lang),
            "unknown language {lang:?}; embedded: {codes:?}"
        );
        render_all(profile, &out);
    }
}
