// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aristotle`: the reader app for the Kindle Paperwhite 4.
//!
//! The library from `aristotle-library` (scanned incrementally on start,
//! folders, series, the book to continue), pages from `aristotle-render`
//! (Plato's engine, Polish hyphenation), the menu, settings and screensaver
//! from `aristotle-ui`, on the panel, touchscreen, frontlight and battery
//! through `aristotle-hal`.
//!
//! ```text
//! aristotle [--data DIR] [--library DIR] [--root DIR] [--fonts DIR] [--turn-mode N]
//!           [--night-method software|hardware] [--no-scan] [--bench-open TITLE]
//!           [--open TITLE] [--dict DIR]... [--index-dicts] [--test-clippings PATH]
//!           [--preview OUT BOOK] [--preview-layout OUT BOOK WIDTH HEIGHT]
//!
//!   --data DIR      renderer data (fonts/, css/, hyphenation-patterns/,
//!                   serif/); default: data/ next to the binary
//!   --library DIR   library index, covers, page cache and preferences;
//!                   default: $ARISTOTLE_DATA or /mnt/us/.aristotle
//!   --root DIR      the Kindle documents folder, the base for downloads,
//!                   clippings and notes; scanning covers the whole user
//!                   store beside it; default: /mnt/us/documents
//!   --fonts DIR     the reader's own TTF and OTF fonts; default: /mnt/us/fonts
//!   --turn-mode N   page-turn waveform: 9 (stock, default) or 7 (DU4);
//!                   saved in reader.json
//!   --night-method  how night mode inverts: software (default) or
//!                   hardware (EPDC inversion flag); saved in reader.json
//!   --no-scan       don't look for new or changed books on start
//!   --bench-open T  time opening the first book whose title contains T,
//!                   without panel or touchscreen, and exit (see open.rs)
//!   --open TITLE    open the first book whose title contains TITLE after
//!                   start (device scenarios)
//!   --dict DIR      look for StarDict dictionaries here (repeatable);
//!                   default: /mnt/us/dictionaries, /mnt/us/koreader/data/dict
//!   --index-dicts   build the dictionaries' lookup indexes into
//!                   LIBRARY/dict-index and exit (on the host, to copy to
//!                   the device instead of indexing there)
//!   --test-clippings PATH  export highlights to PATH instead of
//!                   ROOT/My Clippings.txt, and Markdown next to it (test
//!                   runs must not reach the user's own clippings); PATH
//!                   is written even when the setting "Save highlights to
//!                   My Clippings.txt" is off
//!   --preview OUT BOOK  drive the reading interactions on BOOK with
//!                   synthetic touches on an in-memory panel and write PNGs
//!                   to OUT (host; see preview.rs)
//!   --preview-layout OUT BOOK WIDTH HEIGHT  check reading at a pixel size (default: 300 DPI)
//!   --dpi DPI       density for --preview-layout (default: 300)
//! ```
//!
//! Run it with the stock GUI stopped: `device/app/run-aristotle.sh`.
//! Hold the "Biblioteka" title for a second to quit. Timings go to stderr
//! as `[perf ...]` lines; see docs/reader.md.

mod bookdelete;
mod catalogs;
mod cloud;
mod covers;
mod custom_quotes;
mod device;
mod dict;
mod fonts;
mod launcher;
mod marks;
mod net;
mod open;
mod overlay;
mod pagecache;
mod panel;
mod powerd;
mod prefs;
mod preview;
mod prewarm;
mod progress;
mod readest;
mod reading;
mod restyle;
mod scan_scope;
mod screensaver;
mod selection;
mod session;
mod shelf;
mod snapshot;
mod speed;
mod stats;
mod sync;
mod turns;
mod usb_watch;
mod wake;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use aristotle_hal::input::{monotonic_us, Input, Key, Phase as HalPhase};
use aristotle_hal::platform::Platform;
use aristotle_hal::power::{Battery, Frontlight, LIGHT_STEPS};
use aristotle_hal::system;
use aristotle_library::library::{resolve_library_destination, scan_root, ScanAdmission};
use aristotle_library::{Library, ScanOptions, Status};
use aristotle_net::transfer;
use aristotle_render::formatting_progress::ProgressAction;
use aristotle_render::page::Page;
use aristotle_render::plato_core::anyhow::{bail, format_err, Context, Error};
use aristotle_render::style::Style;
use aristotle_screen::usb::{handle_usb_transition, reconcile, UsbAction, UsbEvent, UsbMode};
use aristotle_ui::geom::{Density, Insets, Rect, Screen as ScreenProfile, Size};
use aristotle_ui::input::{Direction, Phase, TouchEvent};
use aristotle_ui::keyboard::KeyboardState;
use aristotle_ui::nav as ui_nav;
use aristotle_ui::screens::about as ui_about;
use aristotle_ui::screens::book_actions::{
    self as ui_actions, ActionStep, ActionsSheet, BookActionInfo,
};
use aristotle_ui::screens::book_stats as ui_book_stats;
use aristotle_ui::screens::calendar as ui_cal;
use aristotle_ui::screens::cloud as ui_cloud;
use aristotle_ui::screens::coming_soon as ui_soon;
use aristotle_ui::screens::home as ui_home;
use aristotle_ui::screens::library::{self as ui_lib, CoverMode, Filter, View};
use aristotle_ui::screens::marks as ui_marks;
use aristotle_ui::screens::mine as ui_mine;
use aristotle_ui::screens::quote_add as ui_quote_add;
use aristotle_ui::screens::quotes as ui_quotes;
use aristotle_ui::screens::reader as ui_reader;
use aristotle_ui::screens::reader::{
    action as menu_action, build_menu, Menu, MenuSpec, ProgressNodes, Settings,
};
use aristotle_ui::screens::readest_login as ui_readest_login;
use aristotle_ui::screens::scan as ui_scan;
use aristotle_ui::screens::search as ui_search;
use aristotle_ui::screens::settings::{self as set_ui, SystemView, LIGHT_PRESETS, SLEEP_MINUTES};
use aristotle_ui::screens::sleep;
use aristotle_ui::screens::sleep_images as ui_sleep_images;
use aristotle_ui::screens::typeset as ui_typeset;
use aristotle_ui::screens::wifi as ui_wifi;
use aristotle_ui::status::{Loading, LoadingNodes};
use aristotle_ui::text::{FamilyId, FontData, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{Constraints, Kind, Node, NodeId, PaintCx, Refresh, Tree, Widget};
use aristotle_ui::ui::{Ui, UiEvent};
use aristotle_ui::widgets::{BookmarkRibbon, Slider, Steps, Toggle};

use crate::custom_quotes::QuotePool;
use crate::device::Facts;
use crate::dict::service::Service;
use crate::marks::ExportPaths;
use crate::open::{Opening, Pending};
use crate::panel::{Content, Panel};
use crate::prefs::{Covers as CoverPref, Layout, NightMethod, Prefs};
use crate::prewarm::Prewarm;
use crate::progress::{Debounce, Saver};
use crate::reading::{Overlay, OverlayNodes};
use crate::session::{Prepared, Session, Source};
use crate::shelf::{Covers, HomeModel, Nav, ShelfModel, Target, SORTS};
use crate::speed::Speed;
use crate::stats::Stats;
use crate::turns::{Intent, Turn, TurnGestures};
use aristotle_i18n::{
    fmt_clock_date, fmt_clock_day, fmt_clock_weekday, fmt_time, tf, tr, trn, Msg, Plural,
};

static START: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the process started.
pub fn uptime_ms() -> f64 {
    ms(START.get_or_init(Instant::now).elapsed())
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A timing line on stderr: `[perf   1234.5] what happened`.
#[macro_export]
macro_rules! perf {
    ($($arg:tt)*) => {
        if $crate::perf_enabled() {
            eprintln!("[perf {:>9.1}] {}", $crate::uptime_ms(), format!($($arg)*))
        }
    };
}

fn perf_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ARISTOTLE_PERF").map_or(cfg!(debug_assertions), |v| v == "1")
    })
}

/// Set by SIGTERM and SIGINT so the loop saves progress and leaves cleanly.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
    wake::signal();
}

#[derive(Clone)]
struct Args {
    data: PathBuf,
    library: PathBuf,
    root: PathBuf,
    turn_mode: Option<u32>,
    night_method: Option<NightMethod>,
    scan: bool,
    bench_open: Option<String>,
    dicts: Vec<PathBuf>,
    preview: Option<(PathBuf, PathBuf)>,
    preview_size: Option<Size>,
    preview_density: Density,
    open: Option<String>,
    index_dicts: bool,
    test_clippings: Option<PathBuf>,
    /// `--lang CODE`: the UI language, stored in the preferences.
    lang: Option<String>,
    /// The folder of the binary, where `sync.json` and `catalogs.json`
    /// are read from.
    bundle: PathBuf,
    /// The private folder for passwords (`net::accounts_dir`).
    accounts: PathBuf,
    /// The user's fonts folder (`fonts.rs`).
    fonts: PathBuf,
}

fn usage() -> &'static str {
    "usage: aristotle [--data DIR] [--library DIR] [--root DIR] [--fonts DIR] [--turn-mode 9|7] \
     [--night-method software|hardware] [--no-scan] [--bench-open TITLE] [--open TITLE] \
     [--dict DIR]... [--index-dicts] [--test-clippings PATH] [--lang en|pl] \
     [--preview OUT BOOK] [--preview-layout OUT BOOK WIDTH HEIGHT] [--dpi DPI]"
}

fn parse_args() -> Result<Args, Error> {
    let exe_dir = std::env::current_exe()?
        .parent()
        .ok_or_else(|| format_err!("binary has no parent directory"))?
        .to_path_buf();
    let cwd = std::env::current_dir()?;
    let mut a = Args {
        data: exe_dir.join("data"),
        library: std::env::var_os("ARISTOTLE_DATA")
            .map_or_else(|| PathBuf::from("/mnt/us/.aristotle"), PathBuf::from),
        root: PathBuf::from("/mnt/us/documents"),
        turn_mode: None,
        night_method: None,
        scan: true,
        bench_open: None,
        dicts: Vec::new(),
        preview: None,
        preview_size: None,
        preview_density: Density::default(),
        open: None,
        index_dicts: false,
        test_clippings: None,
        lang: None,
        bundle: exe_dir.clone(),
        accounts: net::accounts_dir(),
        fonts: PathBuf::from(fonts::DEFAULT_DIR),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format_err!("{name} needs a value"));
        match arg.as_str() {
            "--data" => a.data = cwd.join(value("--data")?),
            "--library" => a.library = cwd.join(value("--library")?),
            "--root" => a.root = cwd.join(value("--root")?),
            "--fonts" => a.fonts = cwd.join(value("--fonts")?),
            "--turn-mode" => a.turn_mode = Some(value("--turn-mode")?.parse()?),
            "--night-method" => {
                a.night_method = Some(match value("--night-method")?.as_str() {
                    "software" => NightMethod::Software,
                    "hardware" => NightMethod::Hardware,
                    other => bail!("unknown night method {other}\n{}", usage()),
                })
            }
            "--no-scan" => a.scan = false,
            "--bench-open" => a.bench_open = Some(value("--bench-open")?),
            "--open" => a.open = Some(value("--open")?),
            "--dict" => a.dicts.push(cwd.join(value("--dict")?)),
            "--index-dicts" => a.index_dicts = true,
            "--test-clippings" => a.test_clippings = Some(cwd.join(value("--test-clippings")?)),
            "--preview" | "--preview-layout" => {
                let out = cwd.join(value(&arg)?);
                let book = cwd.join(value(&arg)?);
                a.preview_size = if arg == "--preview-layout" {
                    let w = value(&arg)?.parse::<i32>()?;
                    let h = value(&arg)?.parse::<i32>()?;
                    if !(600..=4096).contains(&w) || !(800..=4096).contains(&h) {
                        bail!("preview width must be 600..4096, height must be 800..4096");
                    }
                    Some(Size::new(w, h))
                } else {
                    None
                };
                a.preview = Some((out, book));
            }
            "--lang" => {
                let code = value("--lang")?;
                if !aristotle_i18n::locales().iter().any(|l| l.code == code) {
                    bail!("unknown language {code}\n{}", usage());
                }
                a.lang = Some(code);
            }
            "--dpi" => {
                a.preview_density =
                    Density::new(value("--dpi")?.parse()?).map_err(|e| format_err!("{e}"))?
            }
            "-h" | "--help" => bail!("{}", usage()),
            other => bail!("unknown argument {other}\n{}", usage()),
        }
    }
    if a.preview_density != Density::default() && a.preview_size.is_none() {
        bail!("--dpi requires --preview-layout; device density stays at 300 DPI");
    }
    Ok(a)
}

fn main() -> ExitCode {
    aristotle_hal::system::init_timezone();
    START.get_or_init(Instant::now);
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("aristotle: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, Error> {
    let args = parse_args()?;
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
    // Plato opens its fonts and patterns relative to the current directory.
    aristotle_render::use_data_dir(&args.data)?;
    std::fs::create_dir_all(&args.library)
        .with_context(|| format!("can't create {}", args.library.display()))?;
    // crengine's document cache, under the library's data directory: opening a
    // book again does not have to parse it a second time. It is a cache, not
    // a requirement: if it cannot be set up the reader lays books out the
    // slow way.
    #[cfg(feature = "crengine")]
    if let Err(e) = aristotle_render::crengine::configure_cache(&args.library) {
        eprintln!("aristotle: crengine cache: {e:#}");
    }

    if let Some(title) = &args.bench_open {
        let root = args.scan.then_some(args.root.as_path());
        return open::bench(&args.library, root, title).map(|()| 0);
    }
    if let Some((out, book)) = &args.preview {
        return preview::run(&args, out, book).map(|()| 0);
    }
    if args.index_dicts {
        let cache = args.library.join("dict-index");
        let mut d = dict::Dictionaries::discover(&dict_dirs(&args), &cache, None);
        let missing = d.missing_indexes();
        dict::build_indexes(&missing);
        perf!(
            "dict: {} indexes built in {}",
            missing.len(),
            cache.display()
        );
        return Ok(0);
    }

    let mut prefs = Prefs::load(&args.library);
    if let Some(mode) = args.turn_mode {
        if mode != prefs::STOCK_TURN && mode != prefs::DU4_TURN {
            eprintln!(
                "aristotle: turn mode {mode} is untested; 9 (stock) and 7 (DU4) were measured"
            );
        }
        prefs.turn_mode = mode;
        let _ = prefs.save(&args.library);
    }
    if let Some(m) = args.night_method {
        prefs.night_method = m;
        let _ = prefs.save(&args.library);
    }
    if let Some(code) = &args.lang {
        prefs.language = code.clone();
        let _ = prefs.save(&args.library);
    }
    // Which Kindle this is decides the EPDC update ABI, the touchscreen's
    // orientation and the page-turn buttons. It is read from the device's
    // serial number, not guessed.
    let model = Platform::model().context("can't read the device model")?;
    let panel = Panel::open(prefs.turn_mode).context("can't open the framebuffer")?;
    let (w, h) = panel.size();
    // The framebuffer's rotation, read once at startup: on the Oasis 2 it
    // follows the accelerometer, and the touchscreen and the page-turn
    // buttons have to follow it, because the digitizer reports the panel's
    // own coordinates.
    let rotate = panel.rotate();
    if rotate == 1 || rotate == 3 {
        perf!("input: framebuffer rotate {rotate} is landscape and not supported; read as 0");
    }
    let mut input = Input::open(model, w, h, rotate).context("can't open the input devices")?;
    input.touch_mut().grab(true)?;
    perf!(
        "input: touch {} ({}) rotate {rotate} transform {:?}",
        input.touch().name(),
        input.touch().path(),
        input.touch().transform(),
    );
    match (input.keys(), input.keys_error()) {
        (Some(keys), _) => perf!(
            "input: page-turn keys on {} ({}) forward {} back {}",
            keys.name(),
            keys.path(),
            keys.codes().forward,
            keys.codes().back,
        ),
        (None, Some(e)) => perf!("input: no page-turn buttons: {e}"),
        (None, None) => perf!("input: this model has no page-turn buttons"),
    }
    // Our screensaver replaces the stock one, and powerd's events drive it
    // (powerd.rs). Put back as found on exit.
    let sleep_ctl = powerd::Sleep::start().context("can't acquire the screensaver")?;
    let mut app = build_app(&args, panel, prefs, start_dictionaries(&args), sleep_ctl)?;
    perf!("startup: first frame submitted");
    open::warm_up();

    if args.scan {
        if let Err(e) = app.scan(&args.root) {
            eprintln!("aristotle: startup scan: {e}");
            app.show_scan_failed(Refresh::Full, e.to_string())?;
        }
    }
    if let Some(title) = &args.open {
        app.open_by_title(title)?;
    }
    // The takeover returns before the stock GUI has finished stopping, and
    // the GUI's last splash can land over the first frame after this point
    // (STARTUP-001). Nothing can be drawn over the reader while the book
    // screen is up, so this watch is for the home and library screens only.
    if args.open.is_none() {
        app.startup_watch = Some(StartupWatch {
            until: Instant::now() + Duration::from_secs(15),
            repairs: 2,
        });
    }
    let result = app.run(&mut input);
    let code = app.exit_target.exit_code();
    app.leave()?;
    result.map(|()| code)
}

/// Where to look for dictionaries: `--dict`, else the device defaults.
fn dict_dirs(args: &Args) -> Vec<PathBuf> {
    if args.dicts.is_empty() {
        dict::SEARCH_DIRS.iter().map(PathBuf::from).collect()
    } else {
        args.dicts.clone()
    }
}

/// Find the dictionaries and start the lookup thread (and the index
/// builder if some dictionary has no index yet).
fn start_dictionaries(args: &Args) -> Service {
    let t = Instant::now();
    let s = Service::start(
        dict_dirs(args),
        args.library.join("dict-index"),
        args.library.join("dictionaries.json"),
    );
    perf!(
        "startup: {} dictionaries ({}) in {:.0} ms",
        s.dicts.len(),
        s.dicts
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        ms(t.elapsed())
    );
    s
}

/// Where highlights go: the library root's `My Clippings.txt` and
/// `aristotle-notes/` beside the root, or `--test-clippings` and
/// `aristotle-notes/` beside that file.
fn export_paths(args: &Args) -> ExportPaths {
    match &args.test_clippings {
        Some(path) => ExportPaths {
            clippings: path.clone(),
            notes: path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("aristotle-notes"),
            test: true,
        },
        None => ExportPaths::for_root(&args.root),
    }
}

/// The app on `panel`, showing the library.
fn build_app(
    args: &Args,
    panel: Panel,
    prefs: Prefs,
    dict: Service,
    sleep_ctl: powerd::Sleep,
) -> Result<App, Error> {
    let (w, h) = panel.size();
    aristotle_i18n::set_language(&prefs.language);
    let profile =
        ScreenProfile::new(w, h, args.preview_density.dpi()).map_err(|e| format_err!("{e}"))?;
    let lib = Library::open(&args.library)?;
    let saver = Saver::new(&args.library);
    perf!(
        "startup: devices and library index ready ({} books, turn mode {}, night {} ({:?}))",
        lib.books().len(),
        prefs.turn_mode,
        prefs.night,
        prefs.night_method
    );

    let nav = Nav {
        view: prefs.view(),
        sort: prefs.sort,
        ..Nav::default()
    };
    // The data directory is the current directory by now.
    let ui = Ui::with_screen(
        Tree::new(),
        TextEngine::new(ui_fonts(Path::new("serif"), Path::new("fonts"))?),
        profile,
    );
    perf!("startup: UI fonts ready");
    let light = Frontlight::open().ok();
    // When powerd owns sleep it owns the light too, on its own 0..=24 scale
    // (the stock reader's); otherwise the app sets the hardware itself.
    let light_wanted = match sleep_ctl.light() {
        Some(step) => step.min(LIGHT_STEPS),
        None => light.as_ref().and_then(|l| l.step().ok()).unwrap_or(0),
    };
    let quotes = QuotePool::load(lib.data_dir(), &args.root);
    let home_quote_id = {
        let mode: ui_home::quotes::QuoteMode = prefs.quote_mode.into();
        let day = crate::stats::day_number(aristotle_library::library::now());
        let q = ui_home::quotes::quote_for_day(
            mode,
            |id| prefs.favorite_quotes.contains(&id),
            |id| prefs.hidden_quotes.contains(&id),
            prefs.fixed_quote_id,
            quotes.all(),
            day,
        );
        q.id
    };
    let usb_mode = match (
        aristotle_hal::powerd::drive_mode_state(),
        aristotle_hal::powerd::is_userstore_available(),
    ) {
        (Ok(state), Ok(available)) if state == 1 || !available => {
            perf!("usb: startup detected USB drive mode (state={state}, available={available})");
            UsbMode::DriveMode
        }
        (Ok(state), _) if state == 1 => {
            perf!("usb: startup detected USB drive mode (state={state})");
            UsbMode::DriveMode
        }
        (_, Ok(available)) if !available => {
            perf!("usb: startup detected userstore unavailable");
            UsbMode::DriveMode
        }
        _ => UsbMode::Disconnected,
    };
    let mut app = App {
        panel,
        light,
        light_wanted: std::cell::Cell::new(light_wanted),
        battery: Battery::open().ok(),
        mobi_cache: args.library.join("epub-cache"),
        root: args.root.clone(),
        lib,
        saver,
        snapshots: snapshot::Writer::new(&args.library),
        opening: None,
        pending: None,
        prewarm: Prewarm::default(),
        debounce: Debounce::default(),
        prefs,
        ui,
        styles: Styles::new(FamilyId(0)).with_density(profile.density),
        covers: Covers::default(),
        sleep_pictures: screensaver::Pictures::new(args.library.join("screensaver-covers")),
        screen: Screen::Settings { ids: None, page: 0 },
        nav,
        section: ui_nav::Tab::Library,
        asleep: None,
        quotes,
        home_quote_id,
        pending_delete_quote: None,
        home_clock: clock_minute(),
        startup_watch: None,
        light_off_by_app: false,
        last_touch: Instant::now(),
        sleep_ctl,
        swallow: false,
        dict: Some(dict),
        speed: Speed::load(&args.library),
        stats: Stats::new(&args.library),
        exports: export_paths(args),
        net: net::Net::new(net::Paths {
            state: args.accounts.clone(),
            import: args.bundle.clone(),
        }),
        sync: sync::SyncState::default(),
        readest: readest::ReadestState::new(),
        cloud: cloud::CloudState::default(),
        catalogs: catalogs::CatalogState::default(),
        fonts_dir: args.fonts.clone(),
        font_faces: HashMap::new(),
        exit_target: launcher::Target::Kindle,
        settings_message: None,
        kual_available: launcher::kual_available(),
        actions: None,
        restyler: restyle::Restyler::default(),
        formatting_progress: aristotle_render::formatting_progress::FormattingProgress::default(),
        loading: LoadingState::default(),
        font_load_ms: 0,
        loading_progress: aristotle_render::formatting_progress::FormattingProgress::default(),
        scan_task: None,
        scan_sources: Vec::new(),
        scan_whole: false,
        scan_picker_path: String::new(),
        covers_fill: None,
        covers_fill_started: false,
        covers_pending: 0,
        wifi: None,
        wifi_firewall: None,
        wifi_back: SoonBack::Mine,
        usb_mode,
        usb_watch: usb_watch::UsbWatch::new(usb_mode),
        saved_reading: None,
    };
    app.load_font();
    app.apply_night();
    // The home page is where the reader is expected to land: it holds the
    // book being read. Without such a book there is nothing to continue,
    // and the library is the better first screen.
    if crate::shelf::current_book(&app.lib).is_some() {
        app.show_home(Refresh::Full)?;
    } else {
        app.show_library(Refresh::Full)?;
    }
    Ok(app)
}

/// A Plato page shown as the reader screen's background.
struct PageView {
    page: Rc<Page>,
}

impl Widget for PageView {
    fn measure(&mut self, c: Constraints, _: &mut TextEngine) -> Size {
        c.constrain(c.max)
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &self.page;
        cx.painter
            .blit_gray(r.x, r.y, p.width, p.height, p.stride, &p.data);
        // As panel::night_page does for pages sent directly.
        if NIGHT.with(Cell::get) {
            for b in &p.images {
                cx.painter
                    .invert_rect(Rect::new(r.x + b.x0, r.y + b.y0, b.x1 - b.x0, b.y1 - b.y0));
            }
        }
    }
}

thread_local! {
    /// Night mode, either method: [`PageView`] then inverts the book's
    /// images, so the inversion of the whole screen shows them as by day.
    static NIGHT: Cell<bool> = const { Cell::new(false) };
}

/// Where the word in the status bar comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadingSource {
    /// The fonts being registered before a layout.
    Fonts,
    /// A book opening in the background.
    Opening,
    /// The library being scanned.
    Scan,
    /// The network at work.
    Network,
}

/// What the status bar shows in its middle: the work in progress. There is
/// one word at a time, but a book can open while the network is busy, so
/// every source keeps its own.
#[derive(Default)]
struct LoadingState {
    fonts: Option<Loading>,
    opening: Option<Loading>,
    scan: Option<Loading>,
    network: Option<Loading>,
}

impl LoadingState {
    /// What the bar shows: the first source with something to say.
    fn current(&self) -> Option<&Loading> {
        self.fonts
            .as_ref()
            .or(self.opening.as_ref())
            .or(self.scan.as_ref())
            .or(self.network.as_ref())
    }
}

struct Reading {
    session: Session,
    menu: Menu,
    page_node: NodeId,
    /// The row of reading actions shown with the menu.
    nav: NodeId,
    /// The progress bar shown during background formatting (open, restyle),
    /// and the line of text beside it.
    progress: ProgressNodes,
    menu_open: bool,
    gestures: TurnGestures,
    settings: Settings,
    /// What covers the page (selection, popups, lists); see reading.rs.
    overlay: Overlay,
    nodes: OverlayNodes,
    /// Page turns since the last full flash, for the refresh interval.
    turns_since_flash: u32,
}

// One screen exists at a time and it is never moved in a hot path, so the
// size difference between the variants costs nothing.
#[allow(clippy::large_enum_variant)]
enum Screen {
    /// The first section: the book being read and the books opened lately.
    Home {
        model: HomeModel,
        view: ui_home::HomeUi,
    },
    Library {
        model: ShelfModel,
        view: ui_lib::Library,
    },
    /// The last section: the settings, the way out, and the about page.
    Mine,
    /// The reading calendar: the month on the screen, the day picked in
    /// it, and the stays the month was built from.
    Calendar {
        /// The month on the screen: the year and the month.
        month: (i32, u8),
        /// The day picked in the month.
        day: u8,
        /// The stays of the library, read when the calendar opened.
        records: Vec<crate::stats::PageStay>,
    },
    /// The single book reading statistics screen.
    BookStats {
        book_hash: String,
        return_month: (i32, u8),
        return_day: u8,
        records: Vec<crate::stats::PageStay>,
    },
    /// What the program is, and what the device is.
    About,
    /// A feature that is not built yet: the page says what is coming.
    ComingSoon {
        /// The name of the feature, as the entry showed it.
        title: String,
        /// Where the back button leads.
        back: SoonBack,
    },
    Reader(Box<Reading>),
    /// A book shown from its snapshot while it opens; see `App::opening`.
    Opening,
    /// `ids` is `None` only before the first screen is built.
    Settings {
        ids: Option<set_ui::Settings>,
        page: usize,
    },
    /// The page that picks the screensaver's own picture.
    SleepImages {
        /// The pictures of the folder, in the order they are shown.
        pictures: Vec<screensaver::Picture>,
        /// The page on the screen.
        page: usize,
    },
    /// Progress sync and the catalogs (catalogs.rs).
    Network(Box<catalogs::NetScreen>),
    /// The reader's typesetting panel, opened from the settings: the same
    /// controls and preview, changing the preferences the next book uses.
    Typeset {
        view: Box<ui_typeset::TypesetView>,
        /// The settings page the way back leads to.
        settings_page: usize,
    },
    /// Library search screen.
    Search {
        view: ui_search::SearchUi,
        books: Vec<ui_lib::Book>,
        book_ids: Vec<String>,
    },
    /// Local library scan screen (empty library guide, active progress, or summary report).
    Scan {
        state: ui_scan::ScanState,
        #[allow(dead_code)]
        view: ui_scan::ScanUi,
    },
    /// "My highlights and notes": the library-wide list, newest first.
    Marks {
        /// The page on the screen.
        page: usize,
        /// How many pages the measured cards take.
        total_pages: usize,
        /// The loading area in the middle of the status line.
        loading: LoadingNodes,
    },
    /// The quotation library.
    QuoteLibrary {
        tab: ui_home::quotes::QuoteCategory,
        page: usize,
    },
    /// Add custom quotation screen.
    QuoteAdd {
        state: ui_quote_add::QuoteAddState,
        return_tab: ui_home::quotes::QuoteCategory,
        return_page: usize,
    },
    /// The Wi-Fi transfer page: books come in over the network.
    WifiTransfer {
        /// Where the back button leads.
        back: SoonBack,
        /// The nodes that change while a file is on its way.
        view: ui_wifi::WifiUi,
        /// The stage the page was built with; another stage means the page
        /// is built again.
        stage: transfer::state::Stage,
        /// How many files had arrived when the page was built.
        received: usize,
    },
    /// Readest account login screen.
    ReadestLogin(Box<ui_readest_login::ReadestLoginState>),
    /// Readest sync status and account management.
    ReadestStatus,
    /// The cloud shelf: the books of the Readest account.
    Cloud(Box<ui_cloud::CloudUi>),
    /// Editing one Readest endpoint.
    ReadestEdit(Box<readest::ReadestEditScreen>),
}

/// Where the page of a feature that is not built yet leads back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SoonBack {
    /// The "mine" screen, where the entry that opened the page sits.
    Mine,
    /// The library, where a book's sheet opened the page, on the page it
    /// was on.
    Library { page: usize },
    /// The settings, on the page the entry sits on.
    Settings { page: usize },
    /// The page that picks the screensaver's own picture.
    SleepImages { page: usize },
    /// The scan screen (empty library guide).
    Scan,
    /// The quotation library.
    #[allow(dead_code)]
    Quotes,
    /// The Wi-Fi transfer page.
    #[allow(dead_code)]
    Wifi,
    /// The reader screen.
    Reader,
}

/// What the Wi-Fi page shows, read from the service.
fn wifi_model<'a>(
    facts: &'a Facts,
    address: Option<std::net::Ipv4Addr>,
    state: &transfer::state::State,
    running: bool,
    firewall_open: bool,
) -> ui_wifi::Wifi<'a> {
    let phase = match state.stage {
        transfer::state::Stage::Waiting => ui_wifi::Phase::Waiting,
        transfer::state::Stage::Receiving => ui_wifi::Phase::Receiving,
        transfer::state::Stage::Done => ui_wifi::Phase::Done,
    };
    let current = state.current.as_ref();
    // A file that has arrived in full leaves `current`, but the bar must
    // read 100% rather than fall back to the empty one.
    let progress = if state.stage == transfer::state::Stage::Done {
        1.0
    } else {
        current.map_or(0.0, |c| c.fraction())
    };
    let status_line = if !running {
        // The service is not running: the browser's connection is gone.
        aristotle_i18n::tr(aristotle_i18n::Msg::WifiStatusStopped).to_string()
    } else {
        match (state.stage, current) {
            (transfer::state::Stage::Receiving, Some(c)) => {
                let seconds = transfer::state::seconds_left(c.remaining()).unwrap_or(0);
                aristotle_i18n::tf!(
                    aristotle_i18n::Msg::WifiStatusTransferring,
                    done = ui_wifi::size_label(c.received),
                    total = ui_wifi::size_label(c.total),
                    seconds = seconds,
                )
            }
            (transfer::state::Stage::Receiving, None) => {
                aristotle_i18n::tr(aristotle_i18n::Msg::WifiStatusWaiting).to_string()
            }
            (transfer::state::Stage::Done, _) => {
                aristotle_i18n::tr(aristotle_i18n::Msg::WifiStatusDone).to_string()
            }
            (transfer::state::Stage::Waiting, _) => {
                aristotle_i18n::tr(aristotle_i18n::Msg::WifiStatusWaiting).to_string()
            }
        }
    };
    let received = state
        .received
        .iter()
        .rev()
        .take(ui_wifi::MAX_RECEIVED)
        .map(|r| ui_wifi::ReceivedBook {
            name: r.name.clone(),
            format: format_of(&r.name),
            size: r.size,
        })
        .collect();
    ui_wifi::Wifi {
        status: facts.status_line(),
        connected: address.is_some(),
        firewall_open,
        address: address
            .map(|ip| format!("http://{ip}:{}", transfer::PORT))
            .unwrap_or_default(),
        phase,
        current_file: current.map_or(String::new(), |c| c.name.clone()),
        progress,
        status_line,
        received,
        total_size: state.total_bytes,
        running,
    }
}

/// The kind of a file, as the list of arrived files shows it.
fn format_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_uppercase())
        .unwrap_or_default()
}

/// The book whose actions are on the screen.
#[derive(Clone)]
struct BookActions {
    /// The book's id in the library index.
    book_id: String,
    /// What the sheet shows.
    step: ActionStep,
    /// The sheet's nodes.
    sheet: ActionsSheet,
}

/// The clock card's words, from the local time.
fn clock_text() -> ui_home::ClockText {
    let mut text = ui_home::ClockText::default();
    let Some(t) = system::local_time() else {
        return text;
    };
    text.time = fmt_time(t.hour, t.minute);
    text.date = fmt_clock_date(t.month, t.day, t.weekday);
    text.month_day = fmt_clock_day(t.month, t.day);
    text.weekday = fmt_clock_weekday(t.weekday);
    let lunar = ui_home::lunar::Lunar::of(t.year, t.month as u8, t.day as u8);
    // The calendar is Chinese; the other languages say it in their own
    // words.
    let chinese = aristotle_i18n::language() == "zh";
    text.lunar = if chinese { lunar.zh() } else { lunar.en() };
    text.lunar_short = if chinese {
        lunar.short_zh()
    } else {
        lunar.short_en()
    };
    text.term = lunar
        .term
        .map(|term| if chinese { term.zh() } else { term.en() });
    text
}

/// The hour and minute the clock shows now.
fn clock_minute() -> (u32, u32) {
    match system::local_time() {
        Some(t) => (t.hour, t.minute),
        None => (0, 0),
    }
}

/// The three numbers of the statistics row of the home screen.
fn home_stats(store: &crate::stats::Store) -> ui_home::Stats {
    let now = aristotle_library::library::now();
    let today = store.seconds_today(now);
    let week = store.seconds_this_week(now);
    ui_home::Stats {
        today_minutes: ((today + 59) / 60) as u32,
        streak_days: store.current_streak(now),
        // One decimal, as the prototype shows it.
        week_hours: (week as f32 / 3600.0 * 10.0).round() / 10.0,
    }
}

/// The stock GUI keeps tearing down for 10 to 13 s after the takeover's
/// non-blocking stop, and can draw its splash over our first frame while it
/// goes (measured 5.2 s after the tap: `STARTUP-001`). The app watches the
/// panel for `until` and repaints when the frame is no longer there.
#[derive(Clone, Copy)]
struct StartupWatch {
    until: Instant,
    /// Repaints left; a second splash after one repair is known to happen,
    /// a third is not, so this stops a redraw fight with a GUI that is not
    /// really stopping.
    repairs: u8,
}

struct App {
    panel: Panel,
    light: Option<Frontlight>,
    /// The frontlight step the reader chose. Sleep switches the light off
    /// and wake restores this, not a step read back from the hardware:
    /// powerd ramps the light down on its screensaver at the same time, so
    /// a read then returns a partly dimmed step, and repeated presses of
    /// the power button ratcheted the light down to off (H-008).
    light_wanted: std::cell::Cell<u32>,
    battery: Option<Battery>,
    mobi_cache: PathBuf,
    root: PathBuf,
    lib: Library,
    saver: Saver,
    snapshots: snapshot::Writer,
    /// The book being opened in the background ([`Screen::Opening`]).
    opening: Option<Box<Opening>>,
    /// A book without a snapshot opening on a worker, the library still
    /// on screen.
    pending: Option<Box<Pending>>,
    /// Snapshots made ahead of time while the library is idle.
    prewarm: Prewarm,
    debounce: Debounce,
    prefs: Prefs,
    ui: Ui,
    styles: Styles,
    covers: Covers,
    /// The screensaver's own pictures, with the ones already decoded.
    sleep_pictures: screensaver::Pictures,
    screen: Screen,
    nav: Nav,
    /// The section of the app the reader is in, so the reader comes back
    /// to the screen she left.
    section: ui_nav::Tab,
    /// The saved light step while asleep.
    asleep: Option<u32>,
    /// The quotation pool managing built-in, custom, and file quotes.
    quotes: QuotePool,
    /// The quotation the home screen shows: the one of the day, or the
    /// next one the reader asked for.
    home_quote_id: u32,
    /// Quotation pending deletion confirmation.
    pending_delete_quote: Option<u32>,
    /// The hour and minute the home screen's clock shows.
    home_clock: (u32, u32),
    /// Watches the first frame while the stock GUI finishes shutting down
    /// (`STARTUP-001`).
    startup_watch: Option<StartupWatch>,
    /// The app switched the light off for its own screensaver (powerd's
    /// screensaver switches it off and back on by itself).
    light_off_by_app: bool,
    last_touch: Instant,
    /// The time the status line shows, to refresh it each minute.
    /// powerd: its screensaver and suspend events, the idle timer.
    sleep_ctl: powerd::Sleep,
    /// The touch that woke the screensaver: ignored until it lifts.
    swallow: bool,
    /// Dictionary lookups (a thread of their own).
    dict: Option<Service>,
    speed: Speed,
    /// Page stays for the statistics screens and the sync (stats.rs).
    stats: Stats,
    /// Where highlights are exported.
    exports: ExportPaths,
    /// Network settings and requests (net.rs).
    net: net::Net,
    /// Progress sync (sync.rs).
    sync: sync::SyncState,
    /// Readest sync (readest.rs).
    readest: readest::ReadestState,
    /// The cloud shelf's book list and download (cloud.rs).
    cloud: cloud::CloudState,
    /// The catalog screens' requests (catalogs.rs).
    catalogs: catalogs::CatalogState,
    /// The user's fonts folder (fonts.rs).
    fonts_dir: PathBuf,
    /// UI families made for showing font names, by regular face file.
    font_faces: HashMap<PathBuf, FamilyId>,
    exit_target: launcher::Target,
    settings_message: Option<String>,
    kual_available: bool,
    /// The book whose actions are on the screen, if any.
    actions: Option<BookActions>,
    /// The book being laid out again after a change of typography
    /// (restyle.rs).
    restyler: restyle::Restyler,
    /// Policy and display state machine for layout progress.
    formatting_progress: aristotle_render::formatting_progress::FormattingProgress,
    /// What the status bar shows in its middle: the work in progress.
    loading: LoadingState,
    /// How long the fonts took to register, in milliseconds: a load slower
    /// than [`FONT_LOAD_SLOW_MS`] is worth a word in the status bar.
    font_load_ms: u64,
    /// The throttle of the loading area: the same policy as the reader's
    /// progress bar (nothing before 500 ms, then every 10%).
    loading_progress: aristotle_render::formatting_progress::FormattingProgress,
    /// In-progress local library scan or import task on a background worker.
    scan_task: Option<ScanTask>,
    /// The folders chosen as scan sources, relative to the scan root. Empty
    /// with [`App::scan_whole`] means the whole user store.
    scan_sources: Vec<String>,
    /// The whole user store is the chosen scan source.
    scan_whole: bool,
    /// The folder the source/destination picker shows, relative to the root.
    scan_picker_path: String,
    /// Covers of books scanned before the library kept them (covers.rs).
    covers_fill: Option<covers::Fill>,
    /// Whether the fill was started for this index; a scan starts it again.
    covers_fill_started: bool,
    /// Covers applied since the index was last written.
    covers_pending: usize,
    /// The Wi-Fi transfer service, while its page is open.
    wifi: Option<transfer::Service>,
    /// The rule that lets a browser in, while the service runs.
    wifi_firewall: Option<transfer::firewall::Firewall>,
    /// Where the back button of the Wi-Fi page leads.
    wifi_back: SoonBack,
    /// USB mass storage drive mode state (USB-001).
    usb_mode: UsbMode,
    /// Polls volumd for drive mode changes the powerd events missed, on a
    /// thread of its own (see `usb_watch`).
    usb_watch: usb_watch::UsbWatch,
    /// Saved reading state when temporarily opening a full-screen overlay (e.g. ComingSoon).
    saved_reading: Option<Box<Reading>>,
}

/// The main thread's durable decision for one bounded import batch.
enum ImportDecision {
    Saved(Vec<String>),
    Cancelled,
    Failed(String),
}

enum ScanMsg {
    Progress(aristotle_library::ScanProgress),
    /// How many selected books the import worker has copied so far.
    ImportProgress {
        done: usize,
        total: usize,
    },
    /// Copied and verified books waiting for the index to be saved durably.
    /// The main thread merges and saves, then answers `true` before the
    /// worker may remove the originals.
    Prepared(aristotle_library::transfer::PreparedBatch),
    Done(Result<ScanDone, String>),
}

/// What a worker finished.
enum ScanDone {
    /// A scan: the report and the index it built on its own copy.
    Report(
        aristotle_library::ScanReport,
        aristotle_library::index::Index,
    ),
    /// An import: what was copied, after the originals were cleaned up.
    Import(aristotle_library::transfer::PreparedBatch),
}

/// What a scan worker does.
#[derive(Clone, Debug)]
enum ScanPurpose {
    /// A start-up or background refresh. Known books may be updated, but
    /// only the given paths (explicit downloads and uploads) are admitted as
    /// new books; everything else waits for the reader's review.
    Background { admit: Vec<String> },
    /// A manual scan: find books in the chosen sources and stage them, with
    /// no change to the library.
    Review { sources: Vec<PathBuf> },
    /// An explicit import: add the checked new books, then copy every checked
    /// book into the physical library folder and remove its original only
    /// after the index was saved.
    Import {
        sources: Vec<PathBuf>,
        new_paths: Vec<String>,
        indexed: Vec<String>,
        destination: PathBuf,
    },
}

struct ScanTask {
    /// Removed records and their path base when the user confirmed the import.
    confirmed_removed: aristotle_library::index::Index,
    cancel: Arc<AtomicBool>,
    rx: std::sync::mpsc::Receiver<ScanMsg>,
    purpose: ScanPurpose,
    /// The worker waits on this before removing the originals. It carries
    /// the IDs the live index actually admits; every other prepared source
    /// is kept.
    ack: Option<std::sync::mpsc::Sender<ImportDecision>>,
    _handle: Option<std::thread::JoinHandle<()>>,
}

/// How long the main thread may take to write the index before an import
/// keeps the originals and reports a failure.
const IMPORT_SAVE_TIMEOUT: Duration = Duration::from_secs(20);
/// Commit copied books after 16 MiB to limit temporary disk usage.
/// A single larger book forms its own batch.
const IMPORT_BATCH_BYTES: u64 = 16 * 1024 * 1024;
/// Checkpoint small-book collections regularly as well.
const IMPORT_BATCH_BOOKS: usize = 64;

/// What the scan/import worker does, off the UI thread.
///
/// Nothing here writes the index: the main thread folds a scan's copy in
/// (`Library::absorb_scan`), or merges an import's prepared records and saves
/// them durably before answering the handshake. The worker removes originals
/// only after that answer is `true`.
fn run_scan_worker(
    purpose: &ScanPurpose,
    root: &Path,
    data_dir: &Path,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    tx: &crate::wake::Sender<ScanMsg>,
    ack: Option<std::sync::mpsc::Receiver<ImportDecision>>,
) -> std::result::Result<ScanDone, String> {
    let mut lib = Library::open(data_dir).map_err(|e| e.to_string())?;
    match purpose {
        ScanPurpose::Background { admit } => {
            let options = ScanOptions {
                admission: ScanAdmission::Confirmed(
                    admit.iter().cloned().collect::<HashSet<String>>(),
                ),
                ..opts.clone()
            };
            let report = lib.scan(root, &options).map_err(|e| e.to_string())?;
            Ok(ScanDone::Report(report, lib.index().clone()))
        }
        ScanPurpose::Review { sources } => {
            // Discovery only: no parsing, no covers, no books added.
            let options = ScanOptions {
                admission: ScanAdmission::Staged,
                covers: false,
                ..opts.clone()
            };
            let report = lib
                .scan_sources(root, sources, &options)
                .map_err(|e| e.to_string())?;
            Ok(ScanDone::Report(report, lib.index().clone()))
        }
        ScanPurpose::Import {
            sources,
            new_paths,
            indexed,
            destination,
        } => {
            let options = ScanOptions {
                admission: ScanAdmission::Confirmed(
                    new_paths.iter().cloned().collect::<HashSet<String>>(),
                ),
                ..opts.clone()
            };
            let report = lib
                .scan_sources(root, sources, &options)
                .map_err(|e| e.to_string())?;
            if report.cancelled {
                return Ok(ScanDone::Import(
                    aristotle_library::transfer::PreparedBatch {
                        cancelled: true,
                        ..Default::default()
                    },
                ));
            }
            let mut records = Vec::new();
            let mut seen = HashSet::new();
            let mut missing: Vec<(String, String)> = Vec::new();
            for path in new_paths {
                match lib.books().iter().find(|b| &b.file.path == path) {
                    Some(book) => {
                        if seen.insert(book.id.clone()) {
                            records.push(book.clone());
                        }
                    }
                    None => missing.push((
                        path.clone(),
                        "not imported: not found as a supported book after the scan".into(),
                    )),
                }
            }
            for id in indexed {
                match lib.book(id) {
                    Some(book) => {
                        if seen.insert(book.id.clone()) {
                            records.push(book.clone());
                        }
                    }
                    None => missing.push((
                        id.clone(),
                        "not imported: the book is no longer in the library".into(),
                    )),
                }
            }
            // Release the scan snapshot before the file-copy phase.
            drop(report);
            drop(lib);
            drop(seen);
            let total = records.len();
            let mut result = aristotle_library::transfer::PreparedBatch {
                errors: missing,
                ..Default::default()
            };
            let mut start = 0;
            while start < records.len() {
                if cancel.load(Ordering::SeqCst) {
                    result.cancelled = true;
                    break;
                }
                let mut end = start + 1;
                let mut bytes = records[start].file.size;
                while end < records.len()
                    && end - start < IMPORT_BATCH_BOOKS
                    && bytes.saturating_add(records[end].file.size) <= IMPORT_BATCH_BYTES
                {
                    bytes += records[end].file.size;
                    end += 1;
                }
                let _ = tx.send(ScanMsg::ImportProgress { done: start, total });
                let mut prepared = match aristotle_library::transfer::prepare(
                    root,
                    destination,
                    &records[start..end],
                    cancel,
                ) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        result.errors.extend(
                            records[start..end]
                                .iter()
                                .map(|record| (record.file.path.clone(), error.to_string())),
                        );
                        start = end;
                        continue;
                    }
                };
                result.errors.append(&mut prepared.errors);
                if prepared.cancelled || cancel.load(Ordering::SeqCst) {
                    result.cancelled = true;
                    break;
                }
                if prepared.books.is_empty() {
                    start = end;
                    continue;
                }
                if tx.send(ScanMsg::Prepared(prepared.clone())).is_err() {
                    result.cancelled = true;
                    break;
                }
                let decision = ack
                    .as_ref()
                    .and_then(|receiver| receiver.recv().ok())
                    .unwrap_or(ImportDecision::Cancelled);
                let approved = match decision {
                    ImportDecision::Saved(ids) => ids,
                    ImportDecision::Cancelled => {
                        result.cancelled = true;
                        break;
                    }
                    ImportDecision::Failed(reason) => {
                        result.errors.extend(records[start..].iter().map(|record| {
                            (
                                record.file.path.clone(),
                                format!("index save failed; original kept: {reason}"),
                            )
                        }));
                        break;
                    }
                };
                for book in prepared.books {
                    if cancel.load(Ordering::SeqCst) {
                        result.cancelled = true;
                        break;
                    }
                    if !approved.contains(&book.record.id) {
                        result.errors.push((
                            book.source.display().to_string(),
                            "not imported: the book was removed; original kept".into(),
                        ));
                        continue;
                    }
                    match aristotle_library::transfer::finish(&book) {
                        Ok(()) => result.books.push(book),
                        Err(error) => result.errors.push((
                            book.source.display().to_string(),
                            format!("imported, but the original was kept: {error}"),
                        )),
                    }
                }
                let _ = tx.send(ScanMsg::ImportProgress { done: end, total });
                if result.cancelled {
                    break;
                }
                start = end;
            }
            Ok(ScanDone::Import(result))
        }
    }
}

/// What a handler wants the loop to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

fn ui_event(ev: &aristotle_hal::input::TouchEvent) -> TouchEvent {
    TouchEvent {
        phase: match ev.phase {
            HalPhase::Down => Phase::Down,
            HalPhase::Move => Phase::Move,
            HalPhase::Up => Phase::Up,
        },
        id: ev.id,
        x: ev.x as i32,
        y: ev.y as i32,
        time_us: ev.time_us,
    }
}

/// The UI fonts: the renderer's Literata in `dir` (`serif/` of the data
/// directory) and, when the file is there, the Chinese fallback in
/// `fallback` (`fonts/` of the data directory). Compiling them in would
/// ship the same megabytes twice. The files are mapped rather than read,
/// so their pages stay file-backed like the binary's own.
///
/// The fallback is Plato's own fallback font: its engine reads
/// `data/fonts/DejaVuSans-Symbols.ttf` for every script the body font
/// does not cover, and that file holds Noto Sans SC next to DejaVu Sans's
/// symbols. The Chinese in the interface needs the same one.
fn ui_fonts(dir: &Path, fallback: &Path) -> Result<Fonts, Error> {
    let mut fonts = Fonts::new();
    let mut faces = [None; 4];
    for (face, style) in faces
        .iter_mut()
        .zip(["Regular", "Italic", "Bold", "BoldItalic"])
    {
        let path = dir.join(format!("Literata-{style}.ttf"));
        let bytes = map_file(&path).with_context(|| format!("can't map {}", path.display()))?;
        *face = Some(
            fonts
                .add_face(FontData::Static(bytes))
                .with_context(|| path.display().to_string())?,
        );
    }
    fonts
        .add_family("Literata", faces)
        .ok_or_else(|| format_err!("no regular Literata"))?;
    let path = fallback.join("DejaVuSans-Symbols.ttf");
    match map_file(&path) {
        Ok(bytes) => {
            let face = fonts
                .add_face(FontData::Static(bytes))
                .with_context(|| path.display().to_string())?;
            let family = fonts
                .add_family("Noto Sans SC", [Some(face), None, None, None])
                .ok_or_else(|| format_err!("no regular face in {}", path.display()))?;
            fonts.add_fallback(family);
        }
        // A data directory without the fallback font still shows Latin:
        // only Chinese is missing.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("can't map {}", path.display())),
    }
    Ok(fonts)
}

/// Maps a whole file read-only for the rest of the process.
fn map_file(path: &Path) -> std::io::Result<&'static [u8]> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len() as usize;
    if len == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty file",
        ));
    }
    // SAFETY: a private read-only mapping of the whole file, never unmapped,
    // so the slice is valid for 'static. The data directory is not changed
    // while the app runs (bundles are unpacked with the app stopped).
    unsafe {
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(std::slice::from_raw_parts(ptr as *const u8, len))
    }
}

/// The quit target: the library title, hit-tested at least 9 mm high.
fn quit_zone(ui: &Ui, title: NodeId) -> Rect {
    ui.tree
        .rect(title)
        .expand_to(aristotle_ui::geom::min_target(ui.profile().density))
}

/// The size of a file as it reads on the screen: "2.4 MB".
fn format_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

/// The index of `id` in `first..first + n`, if it is there.
fn in_range(id: u32, first: u32, n: usize) -> Option<usize> {
    (first..first + n as u32)
        .contains(&id)
        .then(|| (id - first) as usize)
}

/// How often the loop reads the progress of a layout. The progress policy
/// limits the refreshes of the panel; this only has to notice them in time.
const LAYOUT_POLL: Duration = Duration::from_millis(200);

/// How many covers the background fill applies before the index is written
/// (covers.rs). The thumbnails are on disk already; this only bounds how
/// much of that work a power cut can throw away.
const COVERS_SAVE_EVERY: usize = 25;

/// How often the loop reads how far a file on its way has come. The
/// screen of the device is drawn from this, so it cannot be too slow.
const WIFI_POLL: Duration = Duration::from_millis(500);

/// Whether the engine that lays a book of this format out says how far it
/// has come. Only crengine-ng does; Plato's own engine and MuPDF lay the
/// book out in silence.
fn reports_progress(format: aristotle_library::Format) -> bool {
    format == aristotle_library::Format::Epub && aristotle_render::book::epub_engine_is_crengine()
}

/// The line of the status bar for a book being laid out.
fn formatting_label(title: &str) -> String {
    tf!(Msg::StatusFormatting, title = title)
}

/// The line of the status bar for a book being opened by an engine that
/// says nothing about how far it has come.
fn opening_label(title: &str) -> String {
    tf!(Msg::StatusOpening, title = title)
}

/// The line of the status bar for a book laid out again after a change of
/// typography.
fn restyle_label() -> String {
    tr(Msg::StatusRestyle).to_string()
}

impl App {
    fn page_style(&self) -> Style {
        self.prefs
            .style()
            .with_screen(self.ui.profile(), self.panel.stride())
    }

    /// Render and send UI chrome changes.
    fn render_ui(&mut self) -> Result<(), Error> {
        self.render_ui_with(Content::Chrome)
    }

    fn render_ui_with(&mut self, content: Content) -> Result<(), Error> {
        if self.asleep.is_some() {
            return Ok(());
        }
        self.submit_ui(content)
    }

    fn submit_ui(&mut self, content: Content) -> Result<(), Error> {
        // The loading area of the status bar says what is going on; the
        // screens that rebuild their tree pick it up here.
        self.write_loading();
        let damage = self.ui.render();
        self.panel.present(&self.ui, &damage, content)?;
        Ok(())
    }

    /// Shows what a source is loading in the status bar of the current
    /// screen, or takes its word away.
    fn set_loading(&mut self, source: LoadingSource, loading: Option<Loading>) {
        match source {
            LoadingSource::Fonts => self.loading.fonts = loading,
            LoadingSource::Opening => self.loading.opening = loading,
            LoadingSource::Scan => self.loading.scan = loading,
            LoadingSource::Network => self.loading.network = loading,
        }
        self.write_loading();
    }

    /// Writes the current loading into the status bar of the screen, if it
    /// has one. Writing the same loading twice changes nothing.
    fn write_loading(&mut self) {
        // Asleep, the tree is the screensaver's, not the screen's: the
        // screen's loading nodes would point into another tree. Wake
        // rebuilds the screen and writes it then.
        if self.asleep.is_some() {
            return;
        }
        let Some(nodes) = self.loading_nodes() else {
            return;
        };
        let App { loading, ui, .. } = self;
        aristotle_ui::status::set_loading(&mut ui.tree, &nodes, loading.current());
    }

    /// The loading area of the status bar of the current screen.
    fn loading_nodes(&self) -> Option<LoadingNodes> {
        match &self.screen {
            Screen::Home { view, .. } => Some(view.loading),
            Screen::Library { view, .. } => Some(view.loading),
            Screen::Scan { view, .. } => Some(view.loading),
            Screen::Settings { ids, .. } => ids.as_ref().map(|s| s.loading),
            Screen::Network(ns) => ns.frame.as_ref().map(|f| f.loading),
            Screen::WifiTransfer { view, .. } => Some(view.loading),
            Screen::Marks { loading, .. } => Some(*loading),
            _ => None,
        }
    }

    fn save_prefs(&self) {
        if let Err(e) = self.prefs.save(self.lib.data_dir()) {
            eprintln!("aristotle: can't save preferences: {e}");
        }
    }

    // --- Night mode and light -------------------------------------------

    /// Bring the panel and the UI in line with `prefs.night`; the next
    /// render is a full flash that shows the switch everywhere at once.
    fn apply_night(&mut self) {
        let method = self.prefs.night.then_some(self.prefs.night_method);
        NIGHT.with(|n| n.set(method.is_some()));
        self.panel.set_night(method);
        self.ui.set_night(method == Some(NightMethod::Software));
        if let Screen::Reader(r) = &mut self.screen {
            r.session.book().set_night(self.prefs.night);
            r.session.clear_pages();
            if let Ok(page) = r.session.page(r.session.current) {
                if let Some(w) = self.ui.tree.widget_mut::<PageView>(r.page_node) {
                    w.page = page;
                }
                self.ui.tree.invalidate(r.page_node, Refresh::Auto);
            }
            self.save_snapshot();
        }
        self.ui.request_full_refresh();
    }

    fn set_night(&mut self, on: bool) {
        self.prefs.night = on;
        self.save_prefs();
        self.apply_night();
        perf!("night mode {}", if on { "on" } else { "off" });
    }

    /// The step the reader chose (see [`App::light_wanted`]).
    fn light_step(&self) -> u32 {
        self.light_wanted.get()
    }

    /// Set the light to `step` as the reader's choice. When powerd owns
    /// sleep it gets the step and sets the light itself, so its ramps
    /// around the screensaver end where the reader left the light.
    fn set_light_step(&self, step: u32) {
        let step = step.min(LIGHT_STEPS);
        self.light_wanted.set(step);
        if self.sleep_ctl.managed() {
            self.sleep_ctl.set_light(step);
        } else {
            self.apply_light_step(step);
        }
    }

    /// Set the light without changing the reader's choice (sleep and wake).
    fn apply_light_step(&self, step: u32) {
        if let Some(l) = &self.light {
            if let Err(e) = l.set_step(step.min(LIGHT_STEPS)) {
                eprintln!("aristotle: frontlight: {e}");
            }
        }
    }

    /// The style of the home screen's clock card.
    fn clock_style(&self) -> ui_home::ClockStyle {
        ui_home::ClockStyle::ALL
            .get(self.prefs.clock_style as usize)
            .copied()
            .unwrap_or_default()
    }

    /// The light as the reader menu's slider shows it, 0 to 1.
    fn light_level(&self) -> f32 {
        self.light_step() as f32 / LIGHT_STEPS as f32
    }

    // --- Sections ---------------------------------------------------------

    /// Switches to another section of the app.
    fn switch_section(&mut self, tab: ui_nav::Tab) -> Result<(), Error> {
        self.cancel_cloud_on_leave();
        match tab {
            ui_nav::Tab::Home => self.show_home(Refresh::Full),
            ui_nav::Tab::Library => self.show_library(Refresh::Full),
            ui_nav::Tab::Mine => self.show_mine(Refresh::Full),
        }
    }

    /// The home screen: the book being read and the books opened lately.
    fn show_home(&mut self, refresh: Refresh) -> Result<(), Error> {
        self.show_home_at(refresh, 0)
    }

    /// The home screen, on the page of the books opened lately.
    fn show_home_at(&mut self, refresh: Refresh, page: usize) -> Result<(), Error> {
        let t0 = Instant::now();
        let mut model = HomeModel::build(&self.lib, &mut self.covers);
        let facts = Facts::status(&self.battery);
        // The statistics of today are read once, here, from the file of
        // this month. Nothing on the home screen keeps them alive.
        let store = crate::stats::Store::new(self.lib.data_dir());
        // What is left of the book being read, at the pace recorded in it.
        // Without records the card says nothing.
        if let Some((id, book)) = model.reading.as_mut() {
            book.minutes_left = store.estimate_minutes_left(id, book.progress);
        }
        let reading = model.reading.as_ref().map(|(_, b)| b);
        let mut recent: Vec<ui_lib::Book> = model.recent.iter().map(|(_, b)| b.clone()).collect();
        recent.truncate(ui_home::RECENT_BOOKS);
        let clock_text = clock_text();
        let t_stats = Instant::now();
        let stats = self.prefs.home_stats.then(|| home_stats(&store));
        perf!("home: stats read in {:.0} ms", ms(t_stats.elapsed()));
        if self.prefs.home_quote {
            let mode: ui_home::quotes::QuoteMode = self.prefs.quote_mode.into();
            let all_quotes = self.quotes.all();
            let pool = ui_home::quotes::active_pool(
                mode,
                |id| self.prefs.favorite_quotes.contains(&id),
                |id| self.prefs.hidden_quotes.contains(&id),
                self.prefs.fixed_quote_id,
                all_quotes,
            );
            if !pool.iter().any(|q| q.id == self.home_quote_id) {
                let day = crate::stats::day_number(aristotle_library::library::now());
                let q = ui_home::quotes::quote_for_day(
                    mode,
                    |id| self.prefs.favorite_quotes.contains(&id),
                    |id| self.prefs.hidden_quotes.contains(&id),
                    self.prefs.fixed_quote_id,
                    all_quotes,
                    day,
                );
                self.home_quote_id = q.id;
            }
        }
        let quote = self
            .prefs
            .home_quote
            .then(|| self.quotes.find(self.home_quote_id))
            .flatten();
        let home = ui_home::Home {
            reading,
            recent: &recent,
            status: facts.status_line(),
            clock: self.clock_style(),
            clock_text,
            stats,
            quote,
            pet: self.prefs.home_pet,
        };
        let screen_w = self.ui.bounds().w;
        let (tree, view) = ui_home::HomeUi::build(&mut self.ui.text, &self.styles, &home, screen_w);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        if page > 0 {
            self.ui.tree.set_page(view.recent_pages, page);
            self.ui.layout();
        }
        // The books opened lately are held as long as the reader's own
        // long press, so that the book's actions open as fast there.
        self.ui.gesture_config_mut().long_press_us = 500_000;
        // The cat hangs from the reading card, so it needs the laid-out
        // screen to find its place.
        view.place_pet(&mut self.ui.tree);
        self.home_clock = clock_minute();
        self.section = ui_nav::Tab::Home;
        let old = std::mem::replace(&mut self.screen, Screen::Home { model, view });
        let submitted = self.render_ui();
        perf!("home: built in {:.0} ms", ms(t0.elapsed()));
        drop(old);
        submitted
    }

    /// The last section: the account, the reading, and the way out.
    fn show_mine(&mut self, refresh: Refresh) -> Result<(), Error> {
        let t0 = Instant::now();
        let facts = Facts::status(&self.battery);
        let (stats, streak) = self.reading_figures();
        let mine = ui_mine::Mine {
            status: facts.status_line(),
            account: match self.net.accounts.readest.as_ref() {
                Some(r) => ui_mine::Account::SignedIn {
                    name: r.email.clone(),
                    last_sync: r
                        .last_sync_time
                        .map(|ms| crate::marks::local_time(ms / 1000))
                        .or_else(|| self.readest.last_sync_display())
                        .unwrap_or_else(|| {
                            aristotle_i18n::tr(aristotle_i18n::Msg::ReadestNeverSynced).to_string()
                        }),
                },
                None => ui_mine::Account::Guest,
            },
            stats,
            streak,
            entries: self.library_entries(),
        };
        let (tree, _view) = ui_mine::MineUi::build(&mut self.ui.text, &self.styles, &mine);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.section = ui_nav::Tab::Mine;
        let old = std::mem::replace(&mut self.screen, Screen::Mine);
        let submitted = self.render_ui();
        perf!("mine: built in {:.0} ms", ms(t0.elapsed()));
        drop(old);
        submitted
    }

    /// The reading calendar: the month of today, with today picked.
    ///
    /// The stays are read from the store of the statistics, once, here;
    /// picking a day and changing the month work on the stays already in
    /// memory.
    fn show_calendar(&mut self, refresh: Refresh) -> Result<(), Error> {
        let now = aristotle_library::library::now();
        let today = crate::stats::date_of(now);
        let records = self.reading_records();
        self.show_calendar_at(refresh, (today.year, today.month), today.day, records)
    }

    /// The reading calendar for `month`, with the day `day` picked in it.
    fn show_calendar_at(
        &mut self,
        refresh: Refresh,
        month: (i32, u8),
        day: u8,
        records: Vec<crate::stats::PageStay>,
    ) -> Result<(), Error> {
        let t0 = Instant::now();
        let (year, number) = month;
        let of = |ts: i64| crate::stats::date_of(ts);
        let now = aristotle_library::library::now();
        let today = crate::stats::date_of(now);
        let daily = crate::stats::month_daily_seconds(&records, year, number, &of);
        let totals = crate::stats::month_totals(&daily);
        let length = crate::stats::days_in_month(year, number);
        let lead = crate::stats::month_lead(year, number);
        // The days of the month, the empty cells of the week first.
        let mut days = Vec::with_capacity(lead as usize + length as usize);
        for _ in 0..lead {
            days.push(ui_cal::Day {
                day: 0,
                minutes: 0,
                future: false,
            });
        }
        for number_of_day in 1..=length {
            let date = crate::stats::LocalDate {
                year,
                month: number,
                day: number_of_day,
            };
            let seconds = daily.get(&date).copied().unwrap_or(0);
            days.push(ui_cal::Day {
                day: number_of_day,
                minutes: ((seconds + 59) / 60) as u32,
                future: date > today,
            });
        }
        let date = crate::stats::LocalDate {
            year,
            month: number,
            day,
        };
        let day_seconds = daily.get(&date).copied().unwrap_or(0);
        let books = self.day_books(&records, date, &of);
        // The month before the first reading, and the month after this
        // one, are not on the screen.
        let current = (today.year, today.month);
        let earliest = crate::stats::earliest_month(&records, &of).unwrap_or(current);
        let facts = Facts::status(&self.battery);
        let cal = ui_cal::Calendar {
            status: facts.status_line(),
            year,
            month: number,
            days,
            selected: day,
            weekday: crate::stats::weekday_of(date) as u8,
            month_minutes: totals.minutes,
            month_days: totals.days,
            day_minutes: ((day_seconds + 59) / 60) as u32,
            books,
            can_prev: month > earliest,
            can_next: month < current,
        };
        let (tree, _view) = ui_cal::CalendarUi::build(&mut self.ui.text, &self.styles, &cal);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        let old = std::mem::replace(
            &mut self.screen,
            Screen::Calendar {
                month,
                day,
                records,
            },
        );
        let submitted = self.render_ui();
        perf!("calendar: built in {:.0} ms", ms(t0.elapsed()));
        drop(old);
        submitted
    }

    /// The books read on `date`, with the minutes read in each. A book that
    /// is no longer in the library is left out.
    fn day_books(
        &mut self,
        records: &[crate::stats::PageStay],
        date: crate::stats::LocalDate,
        of: &dyn Fn(i64) -> crate::stats::LocalDate,
    ) -> Vec<ui_cal::Book> {
        let days = crate::stats::day_book_seconds(records, date, of);
        days.into_iter()
            .filter_map(|(hash, seconds)| {
                let book = self.lib.book(&hash).cloned()?;
                let author = book
                    .meta
                    .authors
                    .first()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let cover = self.covers.get(&self.lib, &book);
                Some(ui_cal::Book {
                    hash: hash.clone(),
                    title: book.meta.title.clone(),
                    author,
                    minutes: ((seconds + 59) / 60) as u32,
                    cover,
                })
            })
            .collect()
    }

    /// Shows the single book reading statistics screen.
    fn show_book_stats(
        &mut self,
        refresh: Refresh,
        book_hash: &str,
        return_month: (i32, u8),
        return_day: u8,
        records: Vec<crate::stats::PageStay>,
    ) -> Result<(), Error> {
        let t0 = Instant::now();
        let of = |ts: i64| crate::stats::date_of(ts);
        let now = aristotle_library::library::now();
        let today = crate::stats::date_of(now);

        let Some(book) = self.lib.book(book_hash).cloned() else {
            return self.show_calendar_at(refresh, return_month, return_day, records);
        };

        let book_totals = crate::stats::book_totals_by(&records, &of);
        let totals = book_totals.get(book_hash).copied().unwrap_or_default();
        let total_hours = (totals.duration / 3600) as u32;
        let total_minutes = ((totals.duration % 3600) / 60) as u32;

        let read_days = totals.days;
        let read_pages = totals.pages;
        let start_date =
            crate::stats::book_start_date(&records, book_hash, &of).map(|d| (d.month, d.day));
        let avg_time_per_page = crate::stats::book_avg_seconds_per_page(&totals);

        let highlights_count = book
            .annotations
            .iter()
            .filter(|a| a.kind == aristotle_library::index::AnnotationKind::Highlight)
            .count() as u32;
        let notes_count = book
            .annotations
            .iter()
            .filter(|a| a.kind == aristotle_library::index::AnnotationKind::Note)
            .count() as u32;

        let progress_percent = book
            .progress
            .as_ref()
            .map(|p| (p.fraction * 100.0).round() as u32)
            .unwrap_or(0);

        let total_pages = book.meta.pages.or_else(|| {
            book.progress.as_ref().and_then(|p| {
                if p.fraction > 0.05 && read_pages > 0 {
                    Some((read_pages as f32 / p.fraction).round() as u32)
                } else {
                    None
                }
            })
        });

        let status_chip = match book.status {
            aristotle_library::index::Status::Unread => ui_book_stats::BookStatus::Unread,
            aristotle_library::index::Status::Reading => ui_book_stats::BookStatus::Reading,
            aristotle_library::index::Status::Finished => ui_book_stats::BookStatus::Finished,
        };

        let prediction =
            if status_chip == ui_book_stats::BookStatus::Finished || progress_percent >= 100 {
                ui_book_stats::Prediction::Finished
            } else if let Some(total) = total_pages {
                let remaining_pages = total.saturating_sub(read_pages);
                if let Some((target_date, days_left)) = crate::stats::book_estimate_completion(
                    &records,
                    book_hash,
                    today,
                    remaining_pages,
                    &of,
                ) {
                    ui_book_stats::Prediction::Estimated {
                        month: target_date.month,
                        day: target_date.day,
                        days_left,
                    }
                } else {
                    ui_book_stats::Prediction::None
                }
            } else {
                ui_book_stats::Prediction::None
            };

        let recent = crate::stats::book_recent_daily_seconds(&records, book_hash, today, &of);
        let bars: Vec<ui_book_stats::DayBar> = recent
            .into_iter()
            .map(|(date, secs)| ui_book_stats::DayBar {
                minutes: ((secs + 59) / 60) as u32,
                label: format!("{}/{}", date.month, date.day),
            })
            .collect();
        let week_total_minutes = bars.iter().map(|b| b.minutes).sum();

        let cover_image = self.covers.get(&self.lib, &book);

        let author = book
            .meta
            .authors
            .first()
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let format = book.format.label().to_string();

        let facts = Facts::status(&self.battery);
        let stats_data = ui_book_stats::BookStats {
            status: facts.status_line(),
            title: book.meta.title.clone(),
            author,
            format,
            chapter: None,
            status_chip,
            read_pages,
            total_pages,
            progress_percent,
            cover_image,
            total_hours,
            total_minutes,
            read_days,
            start_date,
            avg_time_per_page,
            highlights_count,
            notes_count,
            prediction,
            bars,
            week_total_minutes,
        };

        let (tree, _view) =
            ui_book_stats::BookStatsUi::build(&mut self.ui.text, &self.styles, &stats_data);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();

        let old = std::mem::replace(
            &mut self.screen,
            Screen::BookStats {
                book_hash: book_hash.to_string(),
                return_month,
                return_day,
                records,
            },
        );
        let submitted = self.render_ui();
        perf!("book_stats: built in {:.0} ms", ms(t0.elapsed()));
        drop(old);
        submitted
    }

    /// The stays of the library, read from the store of the statistics.
    fn reading_records(&self) -> Vec<crate::stats::PageStay> {
        let store = crate::stats::Store::new(self.lib.data_dir());
        store.records()
    }

    /// The figures of the reading and the streak: the minutes of today,
    /// the hours of the week and of all the time, the books finished, and
    /// the days read in a row.
    ///
    /// The stays are read from the store of the statistics, which the
    /// screen does once, here; the books finished come from the index of
    /// the library.
    fn reading_figures(&self) -> (ui_mine::Stats, ui_mine::Streak) {
        let store = crate::stats::Store::new(self.lib.data_dir());
        let records = store.records();
        let now = aristotle_library::library::now();
        let today = crate::stats::date_of(now);
        let of = |ts: i64| crate::stats::date_of(ts);
        let totals = crate::stats::daily_totals_by(&records, &of);
        let today_seconds = crate::stats::seconds_on(&records, today, &of);
        let week_seconds = crate::stats::seconds_this_week_by(&records, today, &of);
        let total_seconds: u64 = totals.values().sum();
        let streak_days = crate::stats::current_streak_by(&records, today, &of);
        // The last seven days, the oldest first: today is the last.
        let mut week = [false; 7];
        for (i, day) in week.iter_mut().enumerate() {
            let date = crate::stats::days_before(today, 6 - i as i32);
            *day = totals.get(&date).copied().unwrap_or(0) > 0;
        }
        let finished = self
            .lib
            .index()
            .books
            .iter()
            .filter(|b| b.status == Status::Finished)
            .count();
        (
            ui_mine::Stats {
                today_minutes: ((today_seconds + 59) / 60) as u32,
                goal_minutes: self.prefs.daily_goal_minutes,
                week_hours: (week_seconds as f32 / 3600.0 * 10.0).round() / 10.0,
                total_hours: ((total_seconds + 1800) / 3600) as u32,
                finished: finished as u32,
            },
            ui_mine::Streak {
                days: streak_days,
                week,
            },
        )
    }

    /// The values the entries of the list show: the highlights and the
    /// notes of the library, and the version of the program.
    fn library_entries(&self) -> ui_mine::Entries {
        // The counts follow the list of "My highlights and notes": a
        // highlight with a note is counted as a note too.
        let (highlights, notes) = aristotle_library::marks::counts(self.lib.index());
        ui_mine::Entries {
            highlights: highlights as u32,
            notes: notes as u32,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// A feature that is not built yet: the page says what is coming.
    fn show_coming_soon(
        &mut self,
        refresh: Refresh,
        title: &str,
        back: SoonBack,
    ) -> Result<(), Error> {
        let facts = Facts::status(&self.battery);
        let soon = ui_soon::ComingSoon {
            status: facts.status_line(),
            title,
        };
        let (tree, _view) = ui_soon::ComingSoonUi::build(&mut self.ui.text, &self.styles, &soon);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        let old = std::mem::replace(
            &mut self.screen,
            Screen::ComingSoon {
                title: title.to_string(),
                back,
            },
        );
        if back == SoonBack::Reader {
            if let Screen::Reader(r) = old {
                self.saved_reading = Some(r);
            }
        }
        self.render_ui()
    }

    /// The Wi-Fi transfer page: where to go in a browser, and what has
    /// arrived so far.
    ///
    /// The service runs while the page is open: coming here starts it, and
    /// leaving the page stops it.
    fn show_wifi_transfer(&mut self, refresh: Refresh, back: SoonBack) -> Result<(), Error> {
        self.start_wifi_service();
        self.build_wifi_transfer(refresh, back)
    }

    /// Builds the page again, without starting the service: the page may
    /// be shown while the service is stopped on purpose.
    fn build_wifi_transfer(&mut self, refresh: Refresh, back: SoonBack) -> Result<(), Error> {
        self.wifi_back = back;
        let facts = Facts::status(&self.battery);
        let address = transfer::local::address_of(transfer::local::WLAN);
        let state = self.wifi.as_ref().map(|s| s.state()).unwrap_or_default();
        let firewall_open = self
            .wifi_firewall
            .as_ref()
            .is_some_and(|firewall| firewall.is_open());
        let model = wifi_model(&facts, address, &state, self.wifi.is_some(), firewall_open);
        let (tree, view) = ui_wifi::WifiUi::build(&mut self.ui.text, &self.styles, &model);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        let old = std::mem::replace(
            &mut self.screen,
            Screen::WifiTransfer {
                back,
                view,
                stage: state.stage,
                received: state.received.len(),
            },
        );
        // The page can be left to the reader (the font list sends the reader
        // here to add a font): the reading is kept, so the reader comes back
        // to the same book and the same page.
        if back == SoonBack::Reader {
            if let Screen::Reader(r) = old {
                self.saved_reading = Some(r);
            }
        }
        self.render_ui()
    }

    /// Starts the transfer service, if the device is on Wi-Fi.
    fn start_wifi_service(&mut self) {
        if self.wifi.is_some() {
            return;
        }
        if transfer::local::address_of(transfer::local::WLAN).is_none() {
            // No Wi-Fi: there is nobody to reach the device.
            return;
        }
        let config = transfer::Config::new(self.root.clone(), self.fonts_dir.clone());
        match transfer::Service::start(config) {
            Ok(service) => {
                let mut firewall =
                    transfer::firewall::Firewall::new(service.port, transfer::local::WLAN);
                if let Err(e) = firewall.open() {
                    // The device's firewall may not be ours to change; the
                    // transfer may still work without the rule.
                    eprintln!("aristotle: wifi firewall: {e}");
                }
                self.wifi = Some(service);
                self.wifi_firewall = Some(firewall);
            }
            Err(e) => eprintln!("aristotle: wifi transfer: {e}"),
        }
    }

    /// Stops the transfer service and takes the firewall rule away.
    fn stop_wifi(&mut self) {
        self.wifi = None;
        self.wifi_firewall = None;
    }

    /// Reads the transfer's state and shows what changed.
    fn poll_wifi(&mut self) -> Result<(), Error> {
        let Some(service) = &self.wifi else {
            return Ok(());
        };
        let state = service.state();
        let (stage, received, back) = match &self.screen {
            Screen::WifiTransfer {
                stage,
                received,
                back,
                ..
            } => (*stage, *received, *back),
            _ => return Ok(()),
        };
        if state.stage != stage || state.received.len() != received {
            if state.received.len() > received {
                // A book that just arrived is admitted by name, together
                // with a refresh of the known books. Nothing else nearby is
                // picked up by this background walk (UX-112).
                let root = scan_root(&self.root);
                let arrived: Vec<String> = state
                    .received
                    .iter()
                    .skip(received)
                    .map(|book| scan_scope::relative_path(&root, &self.root.join(&book.name)))
                    .filter(|path| !path.is_empty())
                    .collect();
                self.scan_in_background(arrived);
            }
            self.show_wifi_transfer(Refresh::Full, back)?;
            return Ok(());
        }
        // Only the progress moved: the bar and the words beside it are
        // written into the page that is already there.
        let facts = Facts::status(&self.battery);
        let address = transfer::local::address_of(transfer::local::WLAN);
        let firewall_open = self
            .wifi_firewall
            .as_ref()
            .is_some_and(|firewall| firewall.is_open());
        let model = wifi_model(&facts, address, &state, true, firewall_open);
        let Screen::WifiTransfer { view, .. } = &mut self.screen else {
            return Ok(());
        };
        self.ui.tree.set_text(view.file_name, &model.current_file);
        self.ui
            .tree
            .set_text(view.percent, &ui_wifi::percent_text(model.progress));
        self.ui.tree.set_text(view.status_line, &model.status_line);
        if let Some(bar) = self
            .ui
            .tree
            .widget_mut::<ui_lib::RoundedBar>(view.progress_bar)
        {
            bar.value = model.progress;
        }
        self.ui.tree.invalidate(view.progress_bar, Refresh::Auto);
        self.ui.layout();
        self.render_ui()?;
        Ok(())
    }

    /// The library is scanned while the reader stays where they are: the
    /// status bar says what is going on. Only the given paths, files the
    /// reader just received, are admitted as new books; a background walk
    /// never imports anything else.
    fn scan_in_background(&mut self, admit: Vec<String>) {
        if let Err(e) = self.spawn_scan(ScanPurpose::Background { admit }) {
            eprintln!("aristotle: background scan: {e}");
            self.set_loading(LoadingSource::Scan, None);
            return;
        }
        self.set_loading(
            LoadingSource::Scan,
            Some(Loading::with_percent(
                aristotle_i18n::tr(aristotle_i18n::Msg::LibraryScanInProgress).to_string(),
                0,
            )),
        );
    }

    /// Admits exactly one file the reader explicitly downloaded (cloud or
    /// catalogue), and refreshes the rest of the index without importing
    /// anything else nearby.
    fn admit_explicit_file(&mut self, path: &Path) {
        let relative = scan_scope::relative_path(&scan_root(&self.root), path);
        if relative.is_empty() {
            return;
        }
        self.scan_in_background(vec![relative]);
    }

    /// "My highlights and notes": every highlight and note in the library,
    /// newest first, pages filled with as many measured cards as fit.
    fn show_marks(&mut self, refresh: Refresh, page: usize) -> Result<(), Error> {
        let t0 = Instant::now();
        let facts = Facts::status(&self.battery);
        let all = aristotle_library::marks::entries(self.lib.index());
        let entries = all
            .iter()
            .map(|e| ui_marks::Mark {
                text: &e.text,
                note: e.note.as_deref(),
                book: &e.book_title,
                time: aristotle_library::marks::display_time(
                    e.annotation.created.as_deref().unwrap_or(""),
                ),
                missing: e.missing,
            })
            .collect();
        let screen_data = ui_marks::MarksPage {
            status: facts.status_line(),
            page: page.max(1),
            entries,
        };
        let (tree, view) = ui_marks::MarksUi::build(&mut self.ui.text, &self.styles, &screen_data);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        // The page count comes from the measured cards, so the pager and
        // the page the screen reports are known only after layout.
        view.sync(&mut self.ui.tree);
        self.ui.layout();
        let (page, total_pages) = self.ui.tree.page(view.list);
        self.section = ui_nav::Tab::Mine;
        let old = std::mem::replace(
            &mut self.screen,
            Screen::Marks {
                page: page + 1,
                total_pages,
                loading: view.loading,
            },
        );
        let submitted = self.render_ui();
        perf!("marks: built in {:.0} ms", ms(t0.elapsed()));
        drop(old);
        submitted
    }

    /// The quotation library: browse, favorite, hide, and select display modes.
    fn show_quote_library(&mut self, refresh: Refresh) -> Result<(), Error> {
        self.quotes.reload_file_quotes(&self.root);
        let (tab, page) = match self.screen {
            Screen::QuoteLibrary { tab, page } => (tab, page),
            Screen::QuoteAdd {
                return_tab,
                return_page,
                ..
            } => (return_tab, return_page),
            _ => (ui_home::quotes::QuoteCategory::Reading, 1),
        };
        self.show_quote_library_page(refresh, tab, page)
    }

    fn show_quote_library_page(
        &mut self,
        refresh: Refresh,
        tab: ui_home::quotes::QuoteCategory,
        page: usize,
    ) -> Result<(), Error> {
        let t0 = Instant::now();
        let facts = Facts::status(&self.battery);

        let all_quotes = self.quotes.all();
        let day = crate::stats::day_number(aristotle_library::library::now());
        let mode: ui_home::quotes::QuoteMode = self.prefs.quote_mode.into();

        let daily_quote = ui_home::quotes::quote_for_day(
            mode,
            |id| self.prefs.favorite_quotes.contains(&id),
            |id| self.prefs.hidden_quotes.contains(&id),
            self.prefs.fixed_quote_id,
            all_quotes,
            day,
        );

        let pool = ui_home::quotes::active_pool(
            mode,
            |id| self.prefs.favorite_quotes.contains(&id),
            |id| self.prefs.hidden_quotes.contains(&id),
            self.prefs.fixed_quote_id,
            all_quotes,
        );
        if !pool.iter().any(|q| q.id == self.home_quote_id) {
            self.home_quote_id = daily_quote.id;
        }

        let filtered: Vec<&ui_home::quotes::QuoteItem> =
            all_quotes.iter().filter(|q| q.category == tab).collect();
        let total_pages = (filtered.len() + 3) / 4;
        let page = page.clamp(1, total_pages.max(1));
        let start = (page - 1) * 4;
        let end = (start + 4).min(filtered.len());

        let mut page_quotes = Vec::new();
        for &item in &filtered[start..end] {
            page_quotes.push(ui_quotes::QuoteCardItem {
                item,
                is_favorite: self.prefs.favorite_quotes.contains(&item.id),
                is_hidden: self.prefs.hidden_quotes.contains(&item.id),
                can_delete: self.quotes.is_custom(item.id),
            });
        }

        let confirm_delete = self
            .pending_delete_quote
            .and_then(|id| self.quotes.find(id));

        let screen_data = ui_quotes::QuoteScreen {
            status: facts.status_line(),
            tab,
            mode,
            page,
            total_pages,
            reading_count: self.quotes.reading_count(),
            mood_count: self.quotes.mood_count(),
            daily_quote,
            page_quotes,
            confirm_delete,
        };

        let (tree, _view) =
            ui_quotes::QuoteUi::build(&mut self.ui.text, &self.styles, &screen_data);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        let old = std::mem::replace(&mut self.screen, Screen::QuoteLibrary { tab, page });
        let submitted = self.render_ui();
        perf!("quote_library: built in {:.0} ms", ms(t0.elapsed()));
        drop(old);
        submitted
    }

    /// Deletes a custom quotation, updating preferences and the home quote if necessary.
    fn delete_custom_quote(&mut self, id: u32) -> Result<(), Error> {
        let _ = self.quotes.delete_custom(self.lib.data_dir(), id);
        self.prefs.favorite_quotes.remove(&id);
        self.prefs.hidden_quotes.remove(&id);
        if self.prefs.fixed_quote_id == Some(id) {
            self.prefs.fixed_quote_id = None;
        }
        self.save_prefs();
        if self.home_quote_id == id {
            let mode: ui_home::quotes::QuoteMode = self.prefs.quote_mode.into();
            let day = crate::stats::day_number(aristotle_library::library::now());
            let q = ui_home::quotes::quote_for_day(
                mode,
                |id| self.prefs.favorite_quotes.contains(&id),
                |id| self.prefs.hidden_quotes.contains(&id),
                self.prefs.fixed_quote_id,
                self.quotes.all(),
                day,
            );
            self.home_quote_id = q.id;
        }
        Ok(())
    }

    /// Opens the quote addition screen.
    fn show_quote_add(
        &mut self,
        refresh: Refresh,
        return_tab: ui_home::quotes::QuoteCategory,
        return_page: usize,
    ) -> Result<(), Error> {
        let state = ui_quote_add::QuoteAddState::new(return_tab);
        self.show_quote_add_with_state(refresh, state, return_tab, return_page)
    }

    fn show_quote_add_with_state(
        &mut self,
        refresh: Refresh,
        state: ui_quote_add::QuoteAddState,
        return_tab: ui_home::quotes::QuoteCategory,
        return_page: usize,
    ) -> Result<(), Error> {
        let t0 = Instant::now();
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();

        let (tree, _view) = ui_quote_add::QuoteAddUi::build(&self.styles, &state, &status);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        let old = std::mem::replace(
            &mut self.screen,
            Screen::QuoteAdd {
                state,
                return_tab,
                return_page,
            },
        );
        let submitted = self.render_ui();
        perf!("quote_add: built in {:.0} ms", ms(t0.elapsed()));
        drop(old);
        submitted
    }

    /// What the program is, and what the device it runs on is.
    fn show_about(&mut self, refresh: Refresh) -> Result<(), Error> {
        let facts = Facts::read(&self.battery, &self.root);
        let about = ui_about::About {
            status: facts.status_line(),
            facts: &facts.about,
        };
        let (tree, _view) = ui_about::AboutUi::build(&mut self.ui.text, &self.styles, &about);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        let old = std::mem::replace(&mut self.screen, Screen::About);
        let submitted = self.render_ui();
        drop(old);
        submitted
    }

    // --- Library ----------------------------------------------------------

    fn show_library(&mut self, refresh: Refresh) -> Result<(), Error> {
        self.show_library_at(refresh, 0)
    }

    /// Rebuild the library screen for `self.nav`, on list page `page`.
    fn show_library_at(&mut self, refresh: Refresh, page: usize) -> Result<(), Error> {
        self.rebuild_library_at(refresh, page, false)
    }

    fn rebuild_library_at(
        &mut self,
        refresh: Refresh,
        page: usize,
        deferred: bool,
    ) -> Result<(), Error> {
        if self.lib.books().is_empty() {
            return self.show_scan_empty(refresh);
        }
        let t0 = Instant::now();
        let mode = self.prefs.covers.mode();
        let layout = self.prefs.library_layout.mode();
        let model = ShelfModel::build(&self.lib, &self.nav, layout);
        let facts = Facts::status(&self.battery);
        let width = self.ui.bounds().w;
        let (tree, view) = ui_lib::Library::build_shelf_for_width(
            &mut self.ui.text,
            &self.styles,
            &model.shelf(facts.status_line(), mode),
            width,
        );
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        if page > 0 {
            self.ui.tree.set_page(view.list, page);
            self.ui.layout();
        }
        view.sync(&mut self.ui.tree);
        model.load_covers(&self.lib, &mut self.covers, &view, &mut self.ui.tree);
        // Quitting is drastic: ask for a full second's hold.
        self.ui.gesture_config_mut().long_press_us = 1_000_000;
        self.section = ui_nav::Tab::Library;
        let old = std::mem::replace(&mut self.screen, Screen::Library { model, view });
        let submitted = if deferred { Ok(()) } else { self.render_ui() };
        perf!("library: built in {:.0} ms", ms(t0.elapsed()));
        // A closed book is freed only now: the display lists of a laid-out
        // chapter are many allocations, and the library should not wait.
        drop(old);
        submitted
    }

    fn list_page(&self) -> usize {
        match &self.screen {
            Screen::Library { view, .. } => self.ui.tree.page(view.list).0,
            Screen::Settings { ids: Some(ids), .. } => self.ui.tree.page(ids.list).0,
            Screen::Network(ns) => ns.frame.as_ref().map_or(0, |f| self.ui.tree.page(f.list).0),
            Screen::Cloud(view) => self.ui.tree.page(view.list).0,
            _ => 0,
        }
    }

    /// Brings the index in line with the books on the device at start-up.
    ///
    /// Known books may be refreshed, but a new local file is only reported:
    /// the start scan never admits one, so a restart can neither import a
    /// configuration file nor bypass the reader's review (UX-112).
    fn scan(&mut self, root: &std::path::Path) -> Result<(), Error> {
        let root = scan_root(root);
        if !root.is_dir() {
            eprintln!("aristotle: no library folder {}", root.display());
            return Ok(());
        }
        let r = self.lib.scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(HashSet::new()),
                ..ScanOptions::default()
            },
        )?;
        perf!(
            "scan: {:.0} ms ({} unchanged, {} added, {} updated, {} moved, {} removed, {} thumbnails, {} sidecars imported, {} errors, {} waiting)",
            ms(r.total_time),
            r.unchanged,
            r.added,
            r.updated,
            r.moved,
            r.removed,
            r.thumbnails,
            r.sidecars_imported,
            r.errors.len(),
            r.candidates.len()
        );
        for (path, err) in &r.errors {
            eprintln!("aristotle: scan: {path}: {err}");
        }
        let lib = &self.lib;
        let pruned = snapshot::prune(lib.data_dir(), |id| lib.book(id).is_some());
        if pruned > 0 {
            perf!("scan: {pruned} snapshots of books no longer in the library deleted");
        }
        if r.migrated
            || r.added + r.updated + r.moved + r.removed + r.restored + r.sidecars_imported > 0
        {
            self.saver.save(&self.lib);
            if matches!(self.screen, Screen::Library { .. }) {
                let page = self.list_page();
                self.show_library_at(Refresh::Auto, page)?;
            }
        }
        Ok(())
    }

    /// The physical library folder: the reader's choice while it is still
    /// valid, else `墨阅书库` beside the app's own library folder. The folder
    /// is created only by a confirmed import, never by asking for its path.
    fn library_destination(&self) -> PathBuf {
        let root = scan_root(&self.root);
        let default = self.root.join("墨阅书库");
        let chosen = self
            .prefs
            .library_destination
            .clone()
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| default.clone());
        resolve_library_destination(&root, &chosen)
            .or_else(|_| resolve_library_destination(&root, &default))
            .unwrap_or(default)
    }

    /// The sources the next scan covers: the whole store when the reader
    /// chose it (or chose nothing yet), else the checked folders that are
    /// still there.
    fn chosen_sources(&self) -> Vec<PathBuf> {
        if self.scan_whole || self.scan_sources.is_empty() {
            return Vec::new();
        }
        scan_scope::scan_sources(&scan_root(&self.root), &self.scan_sources)
    }

    /// The chosen sources as one line for the review screen.
    fn scan_sources_display(&self) -> String {
        if self.scan_whole || self.scan_sources.is_empty() {
            tr(Msg::LibraryScanWhole).to_string()
        } else {
            self.scan_sources.join(", ")
        }
    }

    fn show_scan_empty(&mut self, refresh: Refresh) -> Result<(), Error> {
        self.show_scan(refresh, ui_scan::ScanState::Empty)
    }

    /// The source picker (several folders, or the whole store) and the
    /// destination picker (one folder). `path` is what the picker shows;
    /// `None` means the scan root itself.
    fn show_scan_sources(
        &mut self,
        refresh: Refresh,
        mode: ui_scan::PickerMode,
        path: Option<&Path>,
    ) -> Result<(), Error> {
        let root = scan_root(&self.root);
        let scope = match path {
            Some(p) => scan_scope::Scope::new(&root, p),
            None => scan_scope::Scope::whole(&root),
        };
        let Some(scope) = scope else {
            // The folder is gone or points out of the store: the scan root
            // is the only thing that can be shown.
            if path.is_some() {
                return self.show_scan_sources(refresh, mode, None);
            }
            return self.show_scan_failed(
                refresh,
                format!("{} ({})", tr(Msg::LibraryScanFailed), root.display()),
            );
        };
        let books: Vec<String> = self
            .lib
            .books()
            .iter()
            .map(|b| b.file.path.clone())
            .collect();
        let folders = scope
            .folders(&books)
            .into_iter()
            .map(|f| ui_scan::ScopeFolder {
                name: f.name,
                relative: f.relative,
                books: f.books,
            })
            .collect();
        self.scan_picker_path = scope.relative();
        let state = ui_scan::ScanState::Sources {
            mode,
            path: scope.relative(),
            folders,
            can_go_up: scope.parent().is_some(),
            selected: self.scan_sources.clone(),
            whole: self.scan_whole,
            destination: self.library_destination().display().to_string(),
        };
        self.show_scan(refresh, state)
    }

    fn show_scan(&mut self, refresh: Refresh, state: ui_scan::ScanState) -> Result<(), Error> {
        let facts = Facts::status(&self.battery);
        let screen_model = ui_scan::ScanScreen {
            status: facts.status_line(),
            state: state.clone(),
            local_books_count: self.lib.books().len(),
        };
        let (tree, view) = ui_scan::ScanUi::build(&mut self.ui.text, &self.styles, &screen_model);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.section = ui_nav::Tab::Library;
        self.screen = Screen::Scan { state, view };
        // The row lists paginate by the height the layout gave them, so the
        // real page count is known only now: write it into the pager label.
        let pagers = match &self.screen {
            Screen::Scan { view, .. } => [
                (view.folder_list, view.folder_pager),
                (view.review_list, view.review_pager),
            ],
            _ => [(None, None), (None, None)],
        };
        let mut changed = false;
        for (list, pager) in pagers {
            if let (Some(list), Some(pager)) = (list, pager) {
                let (page, count) = self.ui.tree.page(list);
                let text = tf!(Msg::LibraryPagerPageOf, page = page + 1, count = count);
                self.ui.tree.set_text(pager, &text);
                changed = true;
            }
        }
        if changed {
            self.ui.layout();
        }
        self.render_ui()
    }

    /// Shows a review list on `page`, so a checkbox tap or a page turn keeps
    /// the reader where they were.
    fn show_review(&mut self, state: ui_scan::ScanState, page: usize) -> Result<(), Error> {
        self.show_scan(Refresh::Full, state)?;
        let (list, pager) = match &self.screen {
            Screen::Scan { view, .. } => (view.review_list, view.review_pager),
            _ => (None, None),
        };
        let Some(list) = list else {
            return Ok(());
        };
        self.ui.tree.set_page(list, page);
        self.ui.layout();
        let (shown, count) = self.ui.tree.page(list);
        if let Some(pager) = pager {
            let text = tf!(Msg::LibraryPagerPageOf, page = shown + 1, count = count);
            self.ui.tree.set_text(pager, &text);
        }
        self.render_ui()
    }

    /// The report for a scan that could not run: the error card says what
    /// went wrong and "Scan again" retries. Without this, a worker error
    /// would leave the "scanning" screen up forever (LIB-006).
    fn show_scan_failed(&mut self, refresh: Refresh, message: String) -> Result<(), Error> {
        self.set_loading(LoadingSource::Scan, None);
        self.show_scan(
            refresh,
            ui_scan::ScanState::Done {
                tiles: ui_scan::failed_tiles(),
                error: Some(ui_scan::ScanError {
                    count: 1,
                    filename: tr(Msg::LibraryScanFailed).to_string(),
                    reason: message,
                }),
                added_books: Vec::new(),
            },
        )
    }

    /// The review state for a staged scan: every found book, unchecked, with
    /// its path, format and whether the index already knows it.
    fn review_state(
        &self,
        report: &aristotle_library::ScanReport,
        destination: &Path,
    ) -> ui_scan::ScanState {
        let root = scan_root(&self.root);
        let destination_relative = scan_scope::relative_path(&root, destination);
        let in_destination = |path: &str| {
            !destination_relative.is_empty()
                && (path == destination_relative
                    || path
                        .strip_prefix(&destination_relative)
                        .is_some_and(|rest| rest.starts_with('/')))
        };
        let books: Vec<ui_scan::ReviewBook> = report
            .candidates
            .iter()
            .map(|c| ui_scan::ReviewBook {
                title: c.title.clone(),
                path: c.path.clone(),
                format: c.format.clone(),
                id: c.indexed.clone(),
                in_destination: in_destination(&c.path),
            })
            .collect();
        let selected = vec![false; books.len()];
        ui_scan::ScanState::Review {
            sources: self.scan_sources_display(),
            destination: destination.display().to_string(),
            books,
            selected,
        }
    }

    /// Scans the chosen sources and stages the result for review. Nothing is
    /// added to the library here; the confirmation does that.
    fn start_scan(&mut self) -> Result<(), Error> {
        if self.scan_task.is_some() {
            return Ok(());
        }
        if !self.scan_whole && self.scan_sources.is_empty() {
            return self.show_scan_sources(
                Refresh::Full,
                ui_scan::PickerMode::Sources,
                Some(&scan_root(&self.root)),
            );
        }
        let sources = self.chosen_sources();
        if !self.scan_whole && sources.is_empty() {
            return self
                .show_scan_failed(Refresh::Full, tr(Msg::LibraryScanSourceGone).to_string());
        }
        let path = self.scan_sources_display();
        let purpose = ScanPurpose::Review { sources };
        if let Err(e) = self.spawn_scan(purpose) {
            return self.show_scan_failed(Refresh::Full, e);
        }
        self.show_scan(
            Refresh::Full,
            ui_scan::ScanState::Scanning {
                path,
                subdirs: true,
                found_count: 0,
                current_file: String::new(),
                progress: 0.0,
                status: aristotle_i18n::tr(aristotle_i18n::Msg::LibraryScanInProgress).to_string(),
                scanned_books: Vec::new(),
            },
        )
    }

    /// Copies the checked books into the library folder on a worker. The
    /// originals go away only after the index was saved durably.
    fn confirm_import(&mut self) -> Result<(), Error> {
        let Screen::Scan {
            state: ui_scan::ScanState::Review {
                books, selected, ..
            },
            ..
        } = &self.screen
        else {
            return Ok(());
        };
        let mut new_paths = Vec::new();
        let mut indexed = Vec::new();
        for (i, book) in books.iter().enumerate() {
            if !selected.get(i).copied().unwrap_or(false) || !book.selectable() {
                continue;
            }
            match &book.id {
                Some(id) => indexed.push(id.clone()),
                None => new_paths.push(book.path.clone()),
            }
        }
        let total = new_paths.len() + indexed.len();
        if total == 0 {
            return Ok(());
        }
        let destination = self.library_destination();
        let sources = self.chosen_sources();
        let purpose = ScanPurpose::Import {
            sources,
            new_paths,
            indexed,
            destination: destination.clone(),
        };
        if let Err(e) = self.spawn_scan(purpose) {
            return self.show_scan(
                Refresh::Full,
                ui_scan::ScanState::Imported {
                    destination: destination.display().to_string(),
                    migrated: 0,
                    failed: 1,
                    cancelled: false,
                    errors: vec![e],
                },
            );
        }
        self.show_scan(
            Refresh::Full,
            ui_scan::ScanState::Importing {
                destination: destination.display().to_string(),
                done: 0,
                total,
                status: tr(Msg::LibraryScanImportingStatus).to_string(),
            },
        )
    }

    /// Runs a scan or an import on a worker of its own, without touching the
    /// screen the reader is on. A failure to start the worker is returned,
    /// so the caller can show a retry instead of a task that never ends.
    fn spawn_scan(&mut self, purpose: ScanPurpose) -> std::result::Result<(), String> {
        if self.scan_task.is_some() {
            return Ok(());
        }
        if matches!(&purpose, ScanPurpose::Import { .. }) {
            // The worker opens the saved index, including just-imported books.
            self.saver
                .save_sync(&self.lib, IMPORT_SAVE_TIMEOUT)
                .map_err(|error| format!("can't save the current library: {error}"))?;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crate::wake::channel();
        let cancel_flag = cancel.clone();
        let root = scan_root(&self.root);
        let data_dir = self.lib.data_dir().to_path_buf();
        let (ack, ack_rx) = match &purpose {
            ScanPurpose::Import { .. } => {
                let (a, b) = std::sync::mpsc::channel();
                (Some(a), Some(b))
            }
            _ => (None, None),
        };
        let worker_purpose = purpose.clone();

        let handle = std::thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || {
                let tx_progress = tx.clone();
                let opts = ScanOptions {
                    cancel: Some(cancel_flag.clone()),
                    on_progress: Some(Arc::new(move |p| {
                        let _ = tx_progress.send(ScanMsg::Progress(p));
                    })),
                    ..ScanOptions::default()
                };
                let result = run_scan_worker(
                    &worker_purpose,
                    &root,
                    &data_dir,
                    &opts,
                    &cancel_flag,
                    &tx,
                    ack_rx,
                );
                let _ = tx.send(ScanMsg::Done(result));
            })
            .map_err(|e| format!("can't start the scan: {e}"))?;

        self.scan_task = Some(ScanTask {
            confirmed_removed: if matches!(&purpose, ScanPurpose::Import { .. }) {
                aristotle_library::index::Index {
                    root: self.lib.index().root.clone(),
                    removed: self.lib.removed().to_vec(),
                    ..Default::default()
                }
            } else {
                Default::default()
            },
            cancel,
            rx,
            purpose,
            ack,
            _handle: Some(handle),
        });
        Ok(())
    }

    fn cancel_scan(&mut self) -> Result<(), Error> {
        let import = matches!(
            self.scan_task.as_ref().map(|t| &t.purpose),
            Some(ScanPurpose::Import { .. })
        );
        if import {
            // Keep the task and its receiver: the worker reports the
            // cancellation through them, so the screen can reach a terminal
            // state. Answer the save handshake with no admitted IDs.
            if let Some(task) = self.scan_task.as_mut() {
                task.cancel.store(true, Ordering::SeqCst);
                if let Some(ack) = task.ack.take() {
                    let _ = ack.send(ImportDecision::Cancelled);
                }
            }
            return Ok(());
        }
        if let Some(task) = self.scan_task.take() {
            task.cancel.store(true, Ordering::SeqCst);
        }
        if self.lib.books().is_empty() {
            self.show_scan_empty(Refresh::Full)
        } else {
            self.show_library(Refresh::Full)
        }
    }

    fn handle_scan_progress(&mut self, p: aristotle_library::ScanProgress) -> Result<(), Error> {
        let Screen::Scan { state, .. } = &mut self.screen else {
            return Ok(());
        };
        let ui_scan::ScanState::Scanning {
            found_count,
            current_file,
            progress,
            scanned_books,
            ..
        } = state
        else {
            return Ok(());
        };
        *found_count = p.found;
        *current_file = p.path.rsplit('/').next().unwrap_or(&p.path).to_string();
        if p.total > 0 {
            *progress = (p.current as f32 / p.total as f32).clamp(0.0, 1.0);
        }
        if let Some((title, format)) = p.new_book {
            if scanned_books.len() < 5 && !scanned_books.iter().any(|b| b.title == title) {
                scanned_books.push(ui_scan::ScannedBook { title, format });
            }
        }
        let state_clone = state.clone();
        // The status bar says what the scan has found so far.
        self.set_loading(
            LoadingSource::Scan,
            Some(Loading::without_percent(trn(Plural::StatusScan, p.found))),
        );
        self.show_scan(Refresh::Auto, state_clone)
    }

    fn handle_import_progress(&mut self, done: usize, total: usize) -> Result<(), Error> {
        let Screen::Scan {
            state:
                ui_scan::ScanState::Importing {
                    done: current,
                    total: all,
                    ..
                },
            ..
        } = &mut self.screen
        else {
            return Ok(());
        };
        *current = done;
        *all = total.max(1);
        if let Some(state) = self.screen_state() {
            self.show_scan(Refresh::Auto, state)?;
        }
        Ok(())
    }

    /// The state of the scan screen, for a rebuild after an in-place change.
    fn screen_state(&self) -> Option<ui_scan::ScanState> {
        match &self.screen {
            Screen::Scan { state, .. } => Some(state.clone()),
            _ => None,
        }
    }

    /// The worker copied and verified the selected books. Merge them into
    /// the live index, save it durably, and only then let the worker remove
    /// the originals.
    fn on_import_prepared(
        &mut self,
        batch: aristotle_library::transfer::PreparedBatch,
    ) -> Result<(), Error> {
        let Some(task) = self.scan_task.as_mut() else {
            return Ok(());
        };
        let ack = task.ack.as_ref().cloned();
        let root = scan_root(&self.root);
        if task.cancel.load(Ordering::SeqCst) || batch.cancelled || batch.books.is_empty() {
            if let Some(ack) = ack {
                let _ = ack.send(ImportDecision::Cancelled);
            }
            return Ok(());
        }
        // Keep the current state (including reading changes this session has
        // not written yet) so a failed save cannot replace it with an older
        // disk snapshot.
        let snapshot = self.lib.index().clone();
        let records: Vec<_> = batch.books.iter().map(|b| b.record.clone()).collect();
        let admitted = self
            .lib
            .apply_imported(&root, records, &task.confirmed_removed);
        perf!(
            "import: {} records folded into the live index",
            admitted.len()
        );
        let save = self.saver.save_sync(&self.lib, IMPORT_SAVE_TIMEOUT);
        if save.is_ok() {
            self.covers_fill = None;
            self.covers_fill_started = false;
        } else {
            eprintln!("aristotle: import: can't save the index; keeping the originals");
            // Restore the live state, then queue it: a timed-out write that
            // lands late must be superseded, not resurrect stale paths.
            self.lib.restore_index(snapshot);
            self.saver.save(&self.lib);
        }
        if let Some(ack) = ack {
            let decision = match save {
                Ok(()) => ImportDecision::Saved(admitted),
                Err(error) => ImportDecision::Failed(error.to_string()),
            };
            let _ = ack.send(decision);
        }
        Ok(())
    }

    fn handle_scan_done(&mut self, result: Result<ScanDone, String>) -> Result<(), Error> {
        let purpose = self.scan_task.take().map(|t| t.purpose);
        // The scan is over: the status bar says nothing about it any more.
        self.set_loading(LoadingSource::Scan, None);
        let on_scan = matches!(
            &self.screen,
            Screen::Scan { state, .. } if !matches!(state, ui_scan::ScanState::Sources { .. })
        );
        match result {
            Ok(ScanDone::Report(report, scanned)) => {
                if report.cancelled {
                    return Ok(());
                }
                match purpose {
                    Some(ScanPurpose::Background { .. }) => {
                        // The scan ran on a copy of the index; fold it into
                        // the one the reader has been using (FIX-106).
                        self.lib.absorb_scan(scanned);
                        self.saver.save(&self.lib);
                        perf!(
                            "scan: folded in ({} updated, {} restored, {} removed, {} waiting)",
                            report.updated,
                            report.restored,
                            report.removed,
                            report.candidates.len()
                        );
                        self.covers_fill = None;
                        self.covers_fill_started = false;
                        if matches!(self.screen, Screen::Cloud(_)) {
                            let page = self.list_page();
                            self.show_cloud(Refresh::Auto, page)?;
                        }
                    }
                    _ => {
                        if on_scan {
                            let destination = self.library_destination();
                            let state = self.review_state(&report, &destination);
                            self.show_scan(Refresh::Full, state)?;
                        }
                    }
                }
            }
            Ok(ScanDone::Import(batch)) => {
                let destination = match &purpose {
                    Some(ScanPurpose::Import { destination, .. }) => {
                        destination.display().to_string()
                    }
                    _ => self.library_destination().display().to_string(),
                };
                if on_scan {
                    self.show_scan(
                        Refresh::Full,
                        ui_scan::ScanState::Imported {
                            destination,
                            migrated: batch.books.len(),
                            failed: batch.errors.len(),
                            cancelled: batch.cancelled,
                            errors: batch
                                .errors
                                .iter()
                                .map(|(path, reason)| format!("{path}: {reason}"))
                                .collect(),
                        },
                    )?;
                }
            }
            Err(e) => {
                eprintln!("aristotle: background scan error: {e}");
                if on_scan {
                    match purpose {
                        Some(ScanPurpose::Import { destination, .. }) => {
                            self.show_scan(
                                Refresh::Full,
                                ui_scan::ScanState::Imported {
                                    destination: destination.display().to_string(),
                                    migrated: 0,
                                    failed: 1,
                                    cancelled: false,
                                    errors: vec![e],
                                },
                            )?;
                        }
                        _ => self.show_scan_failed(Refresh::Full, e)?,
                    }
                }
            }
        }
        Ok(())
    }

    /// Progress of a scan while the reader is on another screen, and the
    /// messages of an import, including the save handshake.
    fn poll_scan(&mut self) -> Result<(), Error> {
        let mut progress_update = None;
        let mut import_progress = None;
        let mut prepared = None;
        let mut done = None;
        let mut stopped = false;
        if let Some(task) = &mut self.scan_task {
            loop {
                match task.rx.try_recv() {
                    Ok(ScanMsg::Progress(p)) => progress_update = Some(p),
                    Ok(ScanMsg::ImportProgress { done, total }) => {
                        import_progress = Some((done, total))
                    }
                    Ok(ScanMsg::Prepared(batch)) => prepared = Some(batch),
                    Ok(ScanMsg::Done(r)) => {
                        done = Some(r);
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    // The worker panicked or exited without a report: the
                    // scan is over, and it has to say so.
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        stopped = true;
                        break;
                    }
                }
            }
        }
        if done.is_none() && stopped {
            eprintln!("aristotle: scan worker stopped without a report");
            done = Some(Err(tr(Msg::LibraryScanStopped).to_string()));
        }
        if let Some(batch) = prepared {
            self.on_import_prepared(batch)?;
        }
        if let Some((done_count, total)) = import_progress {
            self.handle_import_progress(done_count, total)?;
        }
        if let Some(done) = done {
            self.handle_scan_done(done)?;
        } else if let Some(p) = progress_update {
            let percent = if p.total > 0 {
                ((p.current * 100) / p.total).min(100) as u8
            } else {
                0
            };
            self.handle_scan_progress(p)?;
            if !matches!(self.screen, Screen::Scan { .. }) {
                // The scan runs behind the screen the reader is on: the
                // status bar says what is going on.
                self.set_loading(
                    LoadingSource::Scan,
                    Some(Loading::with_percent(
                        aristotle_i18n::tr(aristotle_i18n::Msg::LibraryScanInProgress).to_string(),
                        percent,
                    )),
                );
                // The status bar says it now.
                self.render_ui()?;
            }
        }
        Ok(())
    }

    /// Applies the covers the background fill has made (covers.rs). The
    /// first call starts it; a scan starts it again for the books it added.
    fn poll_covers(&mut self) -> Result<(), Error> {
        if !self.covers_fill_started && self.scan_task.is_none() {
            self.covers_fill_started = true;
            self.covers_fill = covers::Fill::start(self.lib.data_dir(), covers::jobs(&self.lib));
        }
        let Some(fill) = &mut self.covers_fill else {
            return Ok(());
        };
        let (filled, finished) = fill.poll();
        let mut changed = false;
        for (id, key) in filled {
            changed |= self.lib.set_cover(&id, &key);
            self.covers_pending += 1;
        }
        // The index is written every so often, not once per book: the
        // thumbnails are already on disk, and a cover that arrives after a
        // restart is made again from the book.
        if changed && (finished || self.covers_pending >= COVERS_SAVE_EVERY) {
            self.covers_pending = 0;
            self.saver.save(&self.lib);
            self.debounce.saved();
        }
        if finished {
            self.covers_fill = None;
            if changed {
                self.repaint_covers()?;
            }
        }
        Ok(())
    }

    /// Draws the screen again when the covers that just arrived are on it.
    fn repaint_covers(&mut self) -> Result<(), Error> {
        match &self.screen {
            Screen::Home { .. } | Screen::Library { .. } => self.repaint_screen(Refresh::Auto),
            Screen::Cloud(_) => {
                let page = self.list_page();
                self.show_cloud(Refresh::Auto, page)
            }
            _ => Ok(()),
        }
    }

    // --- Book actions -----------------------------------------------------

    /// What the sheet says about the book: its facts, how far it has been
    /// read and how long it has been read.
    fn book_action_info(&mut self, book_id: &str) -> Result<BookActionInfo, Error> {
        let rec = self
            .lib
            .book(book_id)
            .cloned()
            .ok_or_else(|| format_err!("no book {book_id} in the library"))?;
        let book = crate::shelf::screen_book(&self.lib, &rec, &mut self.covers);
        // The statistics are read once, here: how long the book has been
        // read, and the day it was opened the first time.
        let store = crate::stats::Store::new(self.lib.data_dir());
        let records = store.records();
        let totals = crate::stats::book_totals_by(&records, &|ts| crate::stats::date_of(ts));
        let read_secs = totals.get(book_id).map_or(0, |t| t.duration);
        let start_date = records
            .iter()
            .filter(|r| r.book_hash == book_id)
            .map(|r| r.start_time)
            .min()
            .map(|ts| {
                let d = crate::stats::date_of(ts);
                aristotle_i18n::fmt_date(
                    d.year,
                    d.month as u32,
                    d.day as u32,
                    crate::stats::weekday_of(d),
                )
            });
        Ok(BookActionInfo {
            id: rec.id.clone(),
            title: book.title.clone(),
            author: book.author.clone(),
            format: rec.format.name().to_uppercase(),
            size: format_size(rec.file.size),
            path: rec.file.path.clone(),
            full_path: self
                .lib
                .absolute_path(&rec)
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            progress: book.progress,
            finished: matches!(book.status, ui_lib::Status::Finished),
            unread: matches!(book.status, ui_lib::Status::Unread),
            read_secs,
            start_date,
            cover: book.cover.clone(),
        })
    }

    /// Opens the actions of the book whose row was held down. The row
    /// stays inverted, the way the prototype shows it.
    fn open_book_actions(&mut self, book_id: &str, held: NodeId) -> Result<(), Error> {
        let info = self.book_action_info(book_id)?;
        let sheet = ui_actions::build_book_actions(
            &mut self.ui.tree,
            &self.styles,
            &info,
            ActionStep::Menu,
        );
        self.ui.tree.attach_overlay(sheet.root);
        self.ui.tree.set_inverted(held, true);
        self.ui.layout();
        self.actions = Some(BookActions {
            book_id: book_id.to_string(),
            step: ActionStep::Menu,
            sheet,
        });
        self.render_ui()
    }

    /// Shows another step of the sheet.
    fn set_actions_step(&mut self, step: ActionStep) -> Result<(), Error> {
        let Some(actions) = self.actions.clone() else {
            return Ok(());
        };
        if actions.step == step {
            return Ok(());
        }
        let info = self.book_action_info(&actions.book_id)?;
        ui_actions::set_book_actions(&mut self.ui.tree, &self.styles, &actions.sheet, &info, step);
        self.ui.layout();
        if let Some(actions) = &mut self.actions {
            actions.step = step;
        }
        self.render_ui()
    }

    /// Closes the sheet and draws the screen under it again. The screen
    /// is built anew, so the library's filters and the home's books are
    /// the ones the index holds now.
    fn close_book_actions(&mut self) -> Result<(), Error> {
        if self.actions.take().is_none() {
            return Ok(());
        }
        if matches!(self.screen, Screen::Library { .. }) {
            let page = self.list_page();
            self.show_library_at(Refresh::Full, page)?;
        } else if matches!(self.screen, Screen::Home { .. }) {
            let page = self.home_page_now();
            self.show_home_at(Refresh::Full, page)?;
        }
        Ok(())
    }

    /// Handles the actions of the book whose sheet is on the screen.
    /// Returns whether the event was one of them.
    fn on_book_actions_event(&mut self, ev: &UiEvent) -> Result<bool, Error> {
        let Some(actions) = self.actions.clone() else {
            return Ok(false);
        };
        let UiEvent::Action { id, .. } = ev else {
            return Ok(false);
        };
        match *id {
            ui_actions::action::CANCEL => self.close_book_actions()?,
            ui_actions::action::DETAILS => self.set_actions_step(ActionStep::Details)?,
            ui_actions::action::TOGGLE_FINISHED => {
                self.toggle_finished(&actions.book_id)?;
            }
            ui_actions::action::DELETE_PROMPT => {
                self.set_actions_step(ActionStep::ConfirmDelete)?;
            }
            ui_actions::action::CONFIRM_DELETE => self.delete_book_file(&actions.book_id)?,
            ui_actions::action::BACK_TO_MENU => self.set_actions_step(ActionStep::Menu)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Marks the book as finished, or as not finished, and closes the
    /// sheet: the library's filters follow the status.
    fn toggle_finished(&mut self, book_id: &str) -> Result<(), Error> {
        let Some(rec) = self.lib.book(book_id) else {
            return Ok(());
        };
        let status = match rec.status {
            Status::Finished => Status::Unread,
            _ => Status::Finished,
        };
        self.lib.set_status(book_id, status);
        self.saver.save(&self.lib);
        self.close_book_actions()
    }

    /// Deletes the book's local file and the caches that belong to it.
    /// The index keeps the book's progress, its highlights and its notes,
    /// so a book that comes back over USB resumes where it was.
    fn delete_book_file(&mut self, book_id: &str) -> Result<(), Error> {
        let Some(rec) = self.lib.book(book_id).cloned() else {
            return Ok(());
        };
        let deleted = bookdelete::delete(&self.lib, &rec, &self.mobi_cache)?;
        perf!(
            "delete: {} (file {}, cover {}, pages {}, snapshot {}, converted {}, crengine {})",
            rec.file.path,
            deleted.file,
            deleted.cover,
            deleted.pages,
            deleted.snapshot,
            deleted.converted,
            deleted.crengine
        );
        self.lib.remove_book(book_id);
        self.saver.save(&self.lib);
        self.close_book_actions()
    }

    /// The page of the books opened lately that is on the screen.
    fn home_page_now(&self) -> usize {
        match &self.screen {
            Screen::Home { view, .. } => self.ui.tree.page(view.recent_pages).0,
            _ => 0,
        }
    }

    // --- Settings ---------------------------------------------------------

    /// The typesetting panel of the settings: the reading preferences the
    /// next book opens with, with the same preview and controls as the
    /// reader's menu.
    fn typeset_view(&mut self) -> ui_typeset::TypesetView {
        use ui_typeset::FontOption;
        let data = self.lib.data_dir().to_path_buf();
        let user_families = crate::fonts::families(&data, &self.fonts_dir);
        let mut cjk_fonts = vec![FontOption {
            name: crate::fonts::DEFAULT_CJK.into(),
            sample_font: FamilyId(0),
        }];
        let mut latin_fonts = vec![FontOption {
            name: crate::fonts::DEFAULT_LATIN.into(),
            sample_font: FamilyId(0),
        }];
        for f in &user_families {
            if f.name == crate::fonts::DEFAULT_CJK || f.name == crate::fonts::DEFAULT_LATIN {
                continue;
            }
            let is_cjk = f.name.contains("SC")
                || f.name.contains("TC")
                || f.name.contains("CJK")
                || f.name.chars().any(crate::selection::is_cjk);
            let ui = self.ui_family(f);
            let option = FontOption {
                name: f.name.clone(),
                sample_font: ui,
            };
            if is_cjk {
                cjk_fonts.push(option);
            } else {
                latin_fonts.push(option);
            }
        }
        let (title, author) = crate::shelf::current_book(&self.lib).map_or_else(
            || ("红楼梦".to_string(), "曹雪芹".to_string()),
            |rec| {
                let author = rec
                    .meta
                    .authors
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                (rec.meta.title.clone(), author)
            },
        );
        let selected_cjk = cjk_fonts
            .iter()
            .position(|f| f.name == self.prefs.font_cjk_in_use())
            .unwrap_or(0);
        let selected_latin = latin_fonts
            .iter()
            .position(|f| f.name == self.prefs.font_latin_in_use())
            .unwrap_or(0);
        ui_typeset::TypesetView {
            title,
            author,
            cjk_fonts,
            selected_cjk_font: selected_cjk,
            cjk_page: selected_cjk / ui_typeset::FONTS_PER_PAGE,
            latin_fonts,
            selected_latin_font: selected_latin,
            latin_page: selected_latin / ui_typeset::FONTS_PER_PAGE,
            size_index: self.prefs.size.min(ui_typeset::SIZES_PX.len() - 1),
            spacing_index: self.prefs.spacing.min(ui_typeset::SPACING_RATIO.len() - 1),
            margin_index: (2usize.saturating_sub(self.prefs.margins))
                .min(ui_typeset::MARGINS_PX.len() - 1),
            indent_index: self.prefs.cjk_indent.min(1),
            // Script conversion, punctuation trim and page animation are
            // not offered; old reader.json fields are still read.
            script_index: 0,
            gap_index: self.prefs.cjk_gap.min(2),
            trim_index: 0,
            anim_enabled: false,
            anim_speed: 1,
        }
    }

    fn show_typeset(
        &mut self,
        refresh: Refresh,
        view: ui_typeset::TypesetView,
        settings_page: usize,
    ) -> Result<(), Error> {
        let mut tree = Tree::new();
        let (root, _ids) = ui_typeset::build_typeset_screen(&mut tree, &self.styles, &view);
        tree.set_root(root);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::Typeset {
            view: Box::new(view),
            settings_page,
        };
        self.render_ui()
    }

    fn on_typeset_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_typeset::action as a;
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        let Screen::Typeset {
            view,
            settings_page,
        } = &self.screen
        else {
            return Ok(Flow::Continue);
        };
        let mut view = (**view).clone();
        let page = *settings_page;
        if id == a::ADD_FONT {
            return self
                .show_wifi_transfer(Refresh::Full, SoonBack::Settings { page })
                .map(|_| Flow::Continue);
        }
        if id == a::COLLAPSE {
            return self
                .show_settings(Refresh::Full, page)
                .map(|_| Flow::Continue);
        }
        let mut prefs_changed = false;
        match id {
            a::PREV_CJK_PAGE => view.cjk_page = view.cjk_page.saturating_sub(1),
            a::NEXT_CJK_PAGE => {
                let last = view.cjk_fonts.len().saturating_sub(1) / ui_typeset::FONTS_PER_PAGE;
                view.cjk_page = (view.cjk_page + 1).min(last);
            }
            a::PREV_LATIN_PAGE => view.latin_page = view.latin_page.saturating_sub(1),
            a::NEXT_LATIN_PAGE => {
                let last = view.latin_fonts.len().saturating_sub(1) / ui_typeset::FONTS_PER_PAGE;
                view.latin_page = (view.latin_page + 1).min(last);
            }
            i if (a::CJK_FONT_BASE..a::LATIN_FONT_BASE).contains(&i) => {
                let idx = (i - a::CJK_FONT_BASE) as usize;
                if let Some(font) = view.cjk_fonts.get(idx) {
                    let name = font.name.clone();
                    view.selected_cjk_font = idx;
                    view.cjk_page = idx / ui_typeset::FONTS_PER_PAGE;
                    self.apply_reading_font(crate::fonts::FontTarget::Cjk, &name);
                }
            }
            i if (a::LATIN_FONT_BASE..a::SIZE_BASE).contains(&i) => {
                let idx = (i - a::LATIN_FONT_BASE) as usize;
                if let Some(font) = view.latin_fonts.get(idx) {
                    let name = font.name.clone();
                    view.selected_latin_font = idx;
                    view.latin_page = idx / ui_typeset::FONTS_PER_PAGE;
                    self.apply_reading_font(crate::fonts::FontTarget::Latin, &name);
                }
            }
            i if (a::SIZE_BASE..a::SPACING_BASE).contains(&i) => {
                let idx = (i - a::SIZE_BASE) as usize;
                if idx < ui_typeset::SIZES_PX.len() {
                    view.size_index = idx;
                    self.prefs.size = idx;
                    prefs_changed = true;
                }
            }
            i if (a::SPACING_BASE..a::MARGIN_BASE).contains(&i) => {
                let idx = (i - a::SPACING_BASE) as usize;
                if idx < ui_typeset::SPACING_RATIO.len() {
                    view.spacing_index = idx;
                    self.prefs.spacing = idx;
                    prefs_changed = true;
                }
            }
            i if (a::MARGIN_BASE..a::INDENT_BASE).contains(&i) => {
                let idx = (i - a::MARGIN_BASE) as usize;
                if idx < ui_typeset::MARGINS_PX.len() {
                    view.margin_index = idx;
                    self.prefs.margins = 2usize.saturating_sub(idx);
                    prefs_changed = true;
                }
            }
            i if (a::INDENT_BASE..a::GAP_BASE).contains(&i) => {
                let idx = (i - a::INDENT_BASE) as usize;
                if idx < 2 {
                    view.indent_index = idx;
                    self.prefs.cjk_indent = idx;
                    prefs_changed = true;
                }
            }
            i if (a::GAP_BASE..a::TRIM_BASE).contains(&i) => {
                let idx = (i - a::GAP_BASE) as usize;
                if idx < ui_typeset::GAP_RATIO.len() {
                    view.gap_index = idx;
                    self.prefs.cjk_gap = idx;
                    prefs_changed = true;
                }
            }
            _ => {}
        }
        if prefs_changed {
            self.save_prefs();
        }
        self.show_typeset(Refresh::Full, view, page)?;
        Ok(Flow::Continue)
    }

    fn show_settings(&mut self, refresh: Refresh, page: usize) -> Result<(), Error> {
        let facts = Facts::read(&self.battery, &self.root);
        let sync_account = self
            .net
            .accounts
            .sync
            .as_ref()
            .filter(|s| !s.server.is_empty())
            .map(|s| {
                aristotle_i18n::tf!(
                    aristotle_i18n::Msg::SettingsSyncOn,
                    user = s.username,
                    server = s.server
                )
            });
        let light = self
            .light
            .as_ref()
            .map(|l| (l.step().unwrap_or(0), LIGHT_STEPS));
        let font_label = ui_typeset::display_font_name(self.prefs.font_cjk_in_use());
        let wifi_transfer_label = if self.prefs.language == "zh" {
            "未开启"
        } else {
            "Off"
        };
        let view = SystemView {
            status: facts.status_line(),
            light,
            night: self.prefs.night,
            covers: self.prefs.covers.mode(),
            sleep_minutes: self.prefs.sleep(),
            sleep_screen: match self.prefs.sleep_screen {
                prefs::SleepScreen::LastScreen => 0,
                prefs::SleepScreen::BookCover => 1,
                prefs::SleepScreen::CoverQuote => 2,
                prefs::SleepScreen::CoverClip => 3,
                prefs::SleepScreen::CustomImage => 4,
            },
            sleep_screen_cover: self.prefs.sleep_screen == prefs::SleepScreen::BookCover,
            clippings: self.prefs.clippings,
            battery: facts.battery_info(),
            storage: facts.storage,
            usb: facts.usb,
            sync: sync_account.as_deref(),
            catalogs: self.net.accounts.catalogs.len(),
            exit_message: self.settings_message.as_deref(),
            clock_style: self.prefs.clock_style as usize,
            home_stats: self.prefs.home_stats,
            home_quote: self.prefs.home_quote,
            home_pet: self.prefs.home_pet,
            font: font_label,
            refresh_interval: self.prefs.refresh_interval as usize,
            wifi_transfer: wifi_transfer_label,
            account: self.net.accounts.readest.as_ref().map(|r| r.email.as_str()),
        };
        let (tree, ids) = set_ui::Settings::build(&mut self.ui.text, &self.styles, &view);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        if page > 0 {
            self.ui.tree.set_page(ids.list, page);
            self.ui.layout();
        }
        ids.sync(&mut self.ui.tree);
        self.ui.gesture_config_mut().long_press_us = 500_000;
        self.screen = Screen::Settings {
            ids: Some(ids),
            page,
        };
        self.render_ui()
    }

    /// The page that picks the screensaver's own picture.
    ///
    /// The folder is read here, not before: the page is opened rarely
    /// and the screensaver itself only needs the folder when it falls
    /// asleep.
    fn show_sleep_images(&mut self, refresh: Refresh, page: usize) -> Result<(), Error> {
        let facts = Facts::read(&self.battery, &self.root);
        let dir = screensaver::dir_for(&self.root);
        if !matches!(self.screen, Screen::SleepImages { .. }) {
            self.sleep_pictures.invalidate();
        }
        let pictures = self.sleep_pictures.list(&dir);
        let pages = aristotle_library::screensaver::page_count(pictures.len());
        let page = page.min(pages.saturating_sub(1));
        let shown = aristotle_library::screensaver::page_pictures(&pictures, page);
        // A picture that was picked and is gone is no longer picked: the
        // rotation takes over, the page says so, and the settings are
        // cleared, so a new file with the same name is not picked by
        // itself.
        let stored = self.prefs.sleep_image.clone();
        let picked = stored
            .as_deref()
            .filter(|name| pictures.iter().any(|p| p.name == *name));
        if stored.is_some() && picked.is_none() {
            self.prefs.sleep_image = None;
            self.save_prefs();
        }
        // The rotation is the first cell of the first page, whether or not
        // a picture is picked: only the tick follows the settings.
        let rotate_all = page == 0;
        let rotation_picked = picked.is_none();
        let mut thumbs = Vec::with_capacity(shown.len());
        for picture in shown {
            let thumb = self.sleep_pictures.thumb(picture);
            thumbs.push(ui_sleep_images::Picture {
                name: picture.name.clone(),
                thumb,
                picked: picked == Some(picture.name.as_str()),
            });
        }
        let view = ui_sleep_images::View {
            status: facts.status_line(),
            pictures: &thumbs,
            rotate_all,
            rotation_picked,
            current: picked,
            page,
            pages,
        };
        let (tree, _ids) = ui_sleep_images::build(&self.styles, &view, self.ui.bounds().w);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::SleepImages { pictures, page };
        self.render_ui()
    }

    /// The events of the page that picks the screensaver's own picture.
    fn on_sleep_images_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_sleep_images::action as a;
        if let UiEvent::Swipe(dir) = ev {
            let id = match dir {
                Direction::Up | Direction::Left => a::NEXT_PAGE,
                Direction::Down | Direction::Right => a::PREV_PAGE,
            };
            return self.sleep_images_action(id);
        }
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        self.sleep_images_action(id)
    }

    /// What the page does when something on it is tapped.
    fn sleep_images_action(&mut self, id: aristotle_ui::tree::ActionId) -> Result<Flow, Error> {
        use ui_sleep_images::action as a;
        let Screen::SleepImages { pictures, page, .. } = &self.screen else {
            return Ok(Flow::Continue);
        };
        let page = *page;
        let count = pictures.len();
        match id {
            a::BACK => {
                // Back to the settings, on the page where the sleep
                // screen sits (the second one).
                return self.show_settings(Refresh::Full, 1).map(|_| Flow::Continue);
            }
            a::WIFI_TRANSFER => {
                // The Wi-Fi transfer page; it comes back here, so the
                // picture that was uploaded can be picked right away.
                return self
                    .show_wifi_transfer(Refresh::Full, SoonBack::SleepImages { page })
                    .map(|_| Flow::Continue);
            }
            a::PREV_PAGE | a::NEXT_PAGE => {
                let pages = aristotle_library::screensaver::page_count(count);
                let next = match id {
                    a::PREV_PAGE => page.saturating_sub(1),
                    _ => (page + 1).min(pages.saturating_sub(1)),
                };
                if next == page {
                    return Ok(Flow::Continue);
                }
                return self
                    .show_sleep_images(Refresh::Auto, next)
                    .map(|_| Flow::Continue);
            }
            a::ROTATE_ALL => {
                // The rotation: the screensaver walks through the folder.
                self.prefs.sleep_image = None;
                self.prefs.sleep_image_turn = 0;
                self.save_prefs();
                return self
                    .show_sleep_images(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            _ => {
                // A picture of the page: the number counts from the
                // first picture of the page, not from the first of the
                // folder.
                let index = id.checked_sub(a::PICTURE).map(|i| i as usize);
                let Some(index) = index else {
                    return Ok(Flow::Continue);
                };
                let offset = aristotle_library::screensaver::page_offset(page);
                let Some(picture) = pictures.get(offset + index) else {
                    return Ok(Flow::Continue);
                };
                self.prefs.sleep_image = Some(picture.name.clone());
                self.save_prefs();
                // The tick moves: only the two cells change on the
                // screen, so the rest of the page is not refreshed.
                return self
                    .show_sleep_images(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
        }
    }

    fn on_settings_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use set_ui::action as a;
        if let UiEvent::Swipe(dir) = ev {
            let id = match dir {
                Direction::Up | Direction::Left => a::NEXT_PAGE,
                Direction::Down | Direction::Right => a::PREV_PAGE,
            };
            let Screen::Settings { ids: Some(ids), .. } = &mut self.screen else {
                return Ok(Flow::Continue);
            };
            let list = ids.list;
            let page = self.ui.tree.page(list).0;
            if ids.handle(&mut self.ui.tree, id) {
                self.ui.layout();
                ids.sync(&mut self.ui.tree);
                let now = self.ui.tree.page(list).0;
                if now != page {
                    self.ui.request_clean_refresh();
                }
                if let Screen::Settings { page: p, .. } = &mut self.screen {
                    *p = now;
                }
            }
            return Ok(Flow::Continue);
        }
        let UiEvent::Action { id, node, at } = ev else {
            return Ok(Flow::Continue);
        };
        let Screen::Settings { ids: Some(ids), .. } = &self.screen else {
            return Ok(Flow::Continue);
        };
        let list = ids.list;
        let page = self.ui.tree.page(list).0;
        let turned = ids.handle(&mut self.ui.tree, id);
        if turned {
            self.ui.layout();
            ids.sync(&mut self.ui.tree);
            let now = self.ui.tree.page(list).0;
            if now != page {
                // A new page of settings: clean, without a flash.
                self.ui.request_clean_refresh();
            }
            if let Screen::Settings { page: p, .. } = &mut self.screen {
                *p = now;
            }
            return Ok(Flow::Continue);
        }
        match id {
            a::BACK => return self.show_mine(Refresh::Full).map(|_| Flow::Continue),
            a::RETURN_KINDLE => {
                self.exit_target = launcher::Target::Kindle;
                return Ok(Flow::Quit);
            }
            a::OPEN_KUAL => {
                if self.kual_available {
                    self.exit_target = launcher::Target::Kual;
                    return Ok(Flow::Quit);
                }
                self.settings_message = Some(
                    aristotle_i18n::tr(aristotle_i18n::Msg::SettingsKualUnavailable).to_string(),
                );
            }
            a::LIGHT_DOWN | a::LIGHT_UP | a::LIGHT_SET => {
                let cur = self.light_step();
                let step = match id {
                    a::LIGHT_SET => Slider::step_at(self.ui.tree.rect(node), at.x, LIGHT_STEPS),
                    a::LIGHT_DOWN => cur.saturating_sub(1),
                    _ => (cur + 1).min(LIGHT_STEPS),
                };
                self.set_light_step(step);
            }
            a::NIGHT => self.set_night(!self.prefs.night),
            a::SYNC => {
                return self
                    .open_network(catalogs::NetView::Sync, page)
                    .map(|_| Flow::Continue)
            }
            a::CATALOGS => {
                return self
                    .open_network(catalogs::NetView::Catalogs, page)
                    .map(|_| Flow::Continue)
            }
            a::CLIPPINGS => {
                self.prefs.clippings = !self.prefs.clippings;
                self.save_prefs();
                perf!(
                    "clippings: {}",
                    if self.prefs.clippings { "on" } else { "off" }
                );
            }
            a::HOME_STATS => {
                self.prefs.home_stats = !self.prefs.home_stats;
                self.save_prefs();
                return self
                    .show_settings(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            a::HOME_QUOTE => {
                self.prefs.home_quote = !self.prefs.home_quote;
                self.save_prefs();
                return self
                    .show_settings(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            a::HOME_PET => {
                self.prefs.home_pet = !self.prefs.home_pet;
                self.save_prefs();
                return self
                    .show_settings(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            a::CLOCK_STYLE => {
                self.prefs.clock_style = (self.prefs.clock_style + 1) % 4;
                self.save_prefs();
                return self
                    .show_settings(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            a::FONT => {
                let view = self.typeset_view();
                return self
                    .show_typeset(Refresh::Full, view, page)
                    .map(|_| Flow::Continue);
            }
            a::REFRESH_INTERVAL => {
                self.prefs.refresh_interval = (self.prefs.refresh_interval + 1) % 3;
                self.save_prefs();
                return self
                    .show_settings(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            a::SLEEP => {
                let cur = self.prefs.sleep();
                let next_idx = match SLEEP_MINUTES.iter().position(|&m| m == cur) {
                    Some(i) => (i + 1) % SLEEP_MINUTES.len(),
                    None => 0,
                };
                self.prefs.set_sleep(SLEEP_MINUTES[next_idx]);
                self.save_prefs();
                return self
                    .show_settings(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            a::SLEEP_SCREEN => {
                self.prefs.sleep_screen = match self.prefs.sleep_screen {
                    prefs::SleepScreen::LastScreen => prefs::SleepScreen::BookCover,
                    prefs::SleepScreen::BookCover => prefs::SleepScreen::CoverQuote,
                    prefs::SleepScreen::CoverQuote => prefs::SleepScreen::CoverClip,
                    prefs::SleepScreen::CoverClip => prefs::SleepScreen::CustomImage,
                    prefs::SleepScreen::CustomImage => prefs::SleepScreen::LastScreen,
                };
                self.save_prefs();
                return self
                    .show_settings(Refresh::Auto, page)
                    .map(|_| Flow::Continue);
            }
            a::SLEEP_IMAGE => {
                // The page that picks the screensaver's own picture.
                return self
                    .show_sleep_images(Refresh::Full, 0)
                    .map(|_| Flow::Continue);
            }
            a::ACCOUNT => {
                if self.net.accounts.readest.is_some() {
                    return self
                        .show_readest_status(Refresh::Full)
                        .map(|_| Flow::Continue);
                } else {
                    return self
                        .show_readest_login(Refresh::Full)
                        .map(|_| Flow::Continue);
                }
            }
            a::WIFI_TRANSFER => {
                return self
                    .show_wifi_transfer(Refresh::Full, SoonBack::Settings { page })
                    .map(|_| Flow::Continue);
            }
            a::USB => {
                let storage = match system::usb_mode() {
                    Some(system::UsbMode::Storage { .. }) => false,
                    _ => true,
                };
                let res = system::set_usb_mode(storage.then_some(10));
                perf!(
                    "usb: {} requested: {res:?}",
                    if storage { "drive" } else { "network" }
                );
                if let Err(e) = res {
                    eprintln!("aristotle: usb mode: {e}");
                    self.settings_message = Some(
                        aristotle_i18n::tr(aristotle_i18n::Msg::SettingsUsbFailed).to_string(),
                    );
                } else {
                    self.settings_message = None;
                }
            }
            a::LANGUAGE => {
                let locales = aristotle_i18n::locales();
                let cur = locales
                    .iter()
                    .position(|l| l.code == self.prefs.language)
                    .unwrap_or(0);
                let next = (cur + 1) % locales.len();
                let code = locales[next].code;
                self.prefs.language = code.to_string();
                self.save_prefs();
                aristotle_i18n::set_language(code);
                perf!("language: {code}");
                return self
                    .show_settings(Refresh::Full, page)
                    .map(|_| Flow::Continue);
            }
            id => {
                if let Some(i) = in_range(id, a::LIGHT_PRESET, LIGHT_PRESETS.len()) {
                    self.set_light_step(LIGHT_PRESETS[i].1);
                } else if let Some(i) = in_range(id, a::COVERS, CoverMode::ALL.len()) {
                    self.prefs.covers = CoverPref::from_mode(CoverMode::ALL[i]);
                    self.save_prefs();
                } else {
                    return Ok(Flow::Continue);
                }
            }
        }
        // Rebuild with only the changed pixels sent; a night switch makes
        // that render a full flash.
        self.show_settings(Refresh::Auto, page)?;
        Ok(Flow::Continue)
    }

    // --- Screensaver ------------------------------------------------------

    fn sleep_deadline(&self) -> Option<Instant> {
        // Asleep, or powerd was asked and has a few seconds to answer.
        if self.asleep.is_some() || self.sleep_ctl.sleep_pending() {
            return None;
        }
        self.prefs
            .sleep()
            .map(|m| self.last_touch + Duration::from_secs(m as u64 * 60))
    }

    /// Save progress and switch the light off; powerd controls CPU suspend.
    fn fall_asleep(&mut self, by_powerd: bool) -> Result<(), Error> {
        if self.usb_mode == UsbMode::DriveMode {
            perf!("power: ignoring fall_asleep in USB DriveMode");
            return Ok(());
        }
        // The snapshot and the progress need the book back from a layout.
        self.settle_restyle()?;
        // Finish an opening without repainting; cancel work without a snapshot.
        self.finish_opening(true)?;
        self.cancel_pending();
        self.prewarm.cancel();
        self.ui.reset_gestures();
        if let Screen::Reader(r) = &mut self.screen {
            r.gestures.reset();
        }
        if let Screen::Reader(r) = &self.screen {
            let now = aristotle_library::library::now();
            self.lib
                .set_progress(&r.session.id, r.session.progress(now));
        }
        // Push while the radio is still up. Each push has a short timeout and
        // a failure is queued, so a dead network cannot hold the sleep.
        self.sync_push(false, true);
        self.readest_push(false, true);
        // The device is going to sleep: nothing listens on the network now.
        self.stop_wifi();
        self.save_snapshot();
        self.saver.save(&self.lib);
        self.speed.save();
        self.stats.finish_page(Instant::now());
        self.stats.flush();
        self.debounce.saved();
        self.asleep = Some(self.light_step());
        // powerd ramps the light down for its screensaver and back up on
        // wake; switching it off as well made it flicker on the way down.
        self.light_off_by_app = !(by_powerd && self.sleep_ctl.managed());
        if self.light_off_by_app {
            self.apply_light_step(0);
        }
        self.sleep_ctl.asleep(by_powerd);
        if self.prefs.sleep_screen == prefs::SleepScreen::LastScreen {
            perf!(
                "sleep: last screen ({})",
                if by_powerd { "powerd" } else { "own" }
            );
            return Ok(());
        }
        if self.prefs.sleep_screen == prefs::SleepScreen::CustomImage {
            // The picture of the reader's own. When there is none, or it
            // cannot be shown, the last screen stays: the device must
            // never fail to sleep. This branch comes before the book is
            // looked up: the picture needs no book, and the large cover
            // of the current one is expensive to make.
            let dir = screensaver::dir_for(&self.root);
            let pictures = self.sleep_pictures.list(&dir);
            // A picture that was picked and is gone is no longer picked:
            // the rotation takes over, and the settings are cleared so a
            // new file with the same name is not picked by itself.
            let stored = self.prefs.sleep_image.clone();
            let picked = stored
                .as_deref()
                .filter(|name| pictures.iter().any(|p| p.name == *name));
            if stored.is_some() && picked.is_none() {
                self.prefs.sleep_image = None;
                self.save_prefs();
            }
            let choice = aristotle_library::screensaver::choose(
                &pictures,
                picked,
                self.prefs.sleep_image_turn,
            );
            let picture = match choice {
                aristotle_library::screensaver::Choice::One(p)
                | aristotle_library::screensaver::Choice::Next(p) => p,
                aristotle_library::screensaver::Choice::None => {
                    perf!("sleep: no picture in {}", dir.display());
                    return Ok(());
                }
            };
            let Some(image) = self.sleep_pictures.screen(&picture) else {
                perf!("sleep: the picture {} cannot be shown", picture.name);
                // The rotation skips a picture it cannot show, so the next
                // sleep moves on instead of trying the same file forever.
                if picked.is_none() {
                    self.prefs.sleep_image_turn =
                        (self.prefs.sleep_image_turn % pictures.len() + 1) % pictures.len();
                    self.save_prefs();
                }
                return Ok(());
            };
            // The rotation moves on: the next sleep shows the next
            // picture of the folder.
            if picked.is_none() {
                self.prefs.sleep_image_turn =
                    (self.prefs.sleep_image_turn % pictures.len() + 1) % pictures.len();
                self.save_prefs();
            }
            let tree =
                sleep::build_image(&mut self.ui.text, &self.styles, &image, self.prefs.night);
            self.ui.set_tree(tree);
            perf!(
                "sleep: own picture ({})",
                if by_powerd { "powerd" } else { "own" }
            );
            return self.submit_ui(Content::Chrome);
        }
        let book_rec = shelf::current_book(&self.lib).cloned();
        let book = book_rec.as_ref().map(|rec| {
            let mut book = shelf::screen_book(&self.lib, rec, &mut self.covers);
            // The screensaver shows one book large: its own cover at the
            // size of the sleep screen, made from the book the first time.
            book.cover = self.covers.get_large(&self.lib, rec);
            book
        });
        let tree = match self.prefs.sleep_screen {
            prefs::SleepScreen::LastScreen => unreachable!(),
            // The picture of the reader's own has its own road above.
            prefs::SleepScreen::CustomImage => unreachable!(),
            prefs::SleepScreen::BookCover => sleep::build(&self.styles, book.as_ref()),
            prefs::SleepScreen::CoverQuote => {
                let quote_mode = self.prefs.quote_mode.into();
                let filter = sleep::QuoteFilter {
                    mode: quote_mode,
                    is_favorite: |id: u32| self.prefs.favorite_quotes.contains(&id),
                    is_hidden: |id: u32| self.prefs.hidden_quotes.contains(&id),
                    fixed_id: self.prefs.fixed_quote_id,
                };
                let day = crate::stats::day_number(aristotle_library::library::now());
                let book_title = book.as_ref().map(|b| b.title.as_str()).unwrap_or("");
                let content = sleep::resolve_sleep_content(
                    sleep::SleepQuoteKind::DailyQuote,
                    &[],
                    book_title,
                    filter,
                    self.quotes.all(),
                    day,
                );
                sleep::build_quote(&self.styles, book.as_ref(), &content)
            }
            prefs::SleepScreen::CoverClip => {
                let quote_mode = self.prefs.quote_mode.into();
                let filter = sleep::QuoteFilter {
                    mode: quote_mode,
                    is_favorite: |id: u32| self.prefs.favorite_quotes.contains(&id),
                    is_hidden: |id: u32| self.prefs.hidden_quotes.contains(&id),
                    fixed_id: self.prefs.fixed_quote_id,
                };
                let day = crate::stats::day_number(aristotle_library::library::now());
                let book_title = book.as_ref().map(|b| b.title.as_str()).unwrap_or("");
                let clip_texts: Vec<&str> = book_rec
                    .as_ref()
                    .map(|rec| {
                        rec.annotations
                            .iter()
                            .filter(|a| {
                                a.kind == aristotle_library::index::AnnotationKind::Highlight
                            })
                            .filter_map(|a| a.text.as_deref())
                            .collect()
                    })
                    .unwrap_or_default();
                let content = sleep::resolve_sleep_content(
                    sleep::SleepQuoteKind::Highlights,
                    &clip_texts,
                    book_title,
                    filter,
                    self.quotes.all(),
                    day,
                );
                sleep::build_quote(&self.styles, book.as_ref(), &content)
            }
        };
        self.ui.set_tree(tree);
        perf!(
            "sleep: screensaver ({})",
            if by_powerd { "powerd" } else { "own" }
        );
        self.submit_ui(Content::Chrome)
    }

    /// powerd left its screensaver, or a touch woke ours: the light and
    /// the screen come back.
    fn wake(&mut self, by_powerd: bool) -> Result<(), Error> {
        self.ui.reset_gestures();
        if let Some(step) = self.asleep.take() {
            if std::mem::take(&mut self.light_off_by_app) {
                self.apply_light_step(step);
            }
        }
        self.last_touch = Instant::now();
        self.sleep_ctl.awake(by_powerd, self.last_touch);
        self.prewarm.input();
        perf!(
            "sleep: woken ({})",
            if by_powerd { "powerd" } else { "touch" }
        );
        let refresh = if self.prefs.sleep_screen == prefs::SleepScreen::LastScreen {
            Refresh::Auto
        } else {
            Refresh::Full
        };
        self.repaint_screen(refresh)
    }

    /// Redraw the current screen with the requested refresh mode.
    fn repaint_screen(&mut self, refresh: Refresh) -> Result<(), Error> {
        match &self.screen {
            Screen::Home { .. } => self.show_home(refresh),
            Screen::Library { .. } => self.show_library(refresh),
            Screen::Search { .. } => self.show_search(refresh),
            Screen::Mine => self.show_mine(refresh),
            Screen::About => self.show_about(refresh),
            Screen::ComingSoon { title, back } => {
                let title = title.clone();
                self.show_coming_soon(refresh, &title, *back)
            }
            Screen::Settings { page, .. } => {
                let page = *page;
                self.show_settings(refresh, page)
            }
            Screen::SleepImages { page, .. } => {
                let page = *page;
                self.show_sleep_images(refresh, page)
            }
            Screen::Typeset {
                view,
                settings_page,
            } => {
                let view = (**view).clone();
                self.show_typeset(refresh, view, *settings_page)
            }
            Screen::Network(_) => self.show_network(refresh),
            Screen::Scan { state, .. } => {
                let state = state.clone();
                self.show_scan(refresh, state)
            }
            Screen::QuoteLibrary { tab, page } => {
                let (tab, page) = (*tab, *page);
                self.show_quote_library_page(refresh, tab, page)
            }
            Screen::Marks { page, .. } => {
                let page = *page;
                self.show_marks(refresh, page)
            }
            Screen::QuoteAdd {
                state,
                return_tab,
                return_page,
            } => {
                let state = state.clone();
                let return_tab = *return_tab;
                let return_page = *return_page;
                self.show_quote_add_with_state(refresh, state, return_tab, return_page)
            }
            Screen::WifiTransfer { back, .. } => {
                // The service was closed when the device went to sleep; it
                // stays closed until the reader starts it again.
                let back = *back;
                self.build_wifi_transfer(refresh, back)
            }
            // (A book still opening was finished before the screensaver.)
            Screen::Reader(_) | Screen::Opening => self.reader_after_sleep(refresh),
            Screen::Calendar { month, day, .. } => {
                let (month, day) = (*month, *day);
                let records = self.reading_records();
                self.show_calendar_at(refresh, month, day, records)
            }
            Screen::BookStats {
                book_hash,
                return_month,
                return_day,
                ..
            } => {
                let book_hash = book_hash.clone();
                let (month, day) = (*return_month, *return_day);
                let records = self.reading_records();
                self.show_book_stats(refresh, &book_hash, month, day, records)
            }
            Screen::ReadestLogin(_) => self.show_readest_login(refresh),
            Screen::ReadestStatus => self.show_readest_status(refresh),
            Screen::ReadestEdit(_) => self.show_readest_status(refresh),
            Screen::Cloud(_) => {
                let page = self.list_page();
                self.show_cloud(refresh, page)
            }
        }
    }

    // --- Event loop -------------------------------------------------------

    /// Repaint the first frame if the stock GUI painted over it while it was
    /// shutting down; see [`StartupWatch`].
    fn watch_startup(&mut self) -> Result<(), Error> {
        let Some(watch) = self.startup_watch else {
            return Ok(());
        };
        if self.asleep.is_some() || Instant::now() >= watch.until {
            self.startup_watch = None;
            return Ok(());
        }
        if self.panel.still_shows_ui(&self.ui) {
            return Ok(());
        }
        perf!("startup: the stock GUI painted over the first frame; repainting");
        self.startup_watch = (watch.repairs > 1).then(|| StartupWatch {
            until: watch.until,
            repairs: watch.repairs - 1,
        });
        self.repaint_screen(Refresh::Full)
    }

    fn poll_timeout(&self, now_us: u64, now: Instant) -> Duration {
        if self.usb_mode == UsbMode::DriveMode {
            // Wakes up periodically (1s) as a fallback in case the
            // usb-watch thread's notification is lost; the lipc queries
            // themselves run in that thread (USB-001 review 3).
            return Duration::from_secs(1);
        }
        let mut timeout = Duration::MAX;
        if self.startup_watch.is_some() {
            // The stock GUI can draw over the first frame at any moment in
            // its shutdown; look between its updates (STARTUP-001).
            timeout = timeout.min(Duration::from_millis(200));
        }
        if self.asleep.is_none() {
            if let Some(t) = self.ui.next_deadline(now_us) {
                timeout = timeout.min(Duration::from_micros(t.max(1000)));
            }
            if let Some(d) = self.debounce.deadline() {
                timeout = timeout.min(d.saturating_duration_since(now));
            }
            // The layout of a changed typography starts when the changes
            // have settled: the loop has to wake up then.
            if let Some(d) = self.restyler.deadline() {
                timeout = timeout.min(d.saturating_duration_since(now));
            }
        }
        if let Some(d) = self.sleep_deadline() {
            timeout = timeout.min(d.saturating_duration_since(now));
        }
        if self.asleep.is_none() {
            if let Screen::Reader(r) = &self.screen {
                if let Some(t) = r.gestures.next_deadline(now_us) {
                    timeout = timeout.min(Duration::from_micros(t.max(1000)));
                }
            }
        }
        if self.prewarm_allowed() {
            if let Some(d) = self.prewarm.deadline() {
                timeout = timeout.min(d.saturating_duration_since(now));
            }
        }
        if let Some(d) = self.sleep_ctl.deadline(self.asleep.is_some()) {
            timeout = timeout.min(d.saturating_duration_since(now));
        }
        // The clock of the home screen: the loop wakes up when the minute
        // turns, so the time is never a minute old.
        if let Some(d) = self.home_clock_deadline() {
            timeout = timeout.min(d);
        }
        // A worker wakes the loop only when it ends: while a book is laid
        // out, the loop wakes up by itself to read how far it has come.
        if self.asleep.is_none() && self.layout_running() {
            timeout = timeout.min(LAYOUT_POLL);
        }
        // A file on its way: the loop wakes up now and then to see how far
        // it has come.
        if self.asleep.is_none() && self.wifi.is_some() {
            timeout = timeout.min(WIFI_POLL);
        }
        timeout
    }

    /// Whether a book is being laid out on a worker, with progress to show.
    fn layout_running(&self) -> bool {
        self.pending.is_some() || self.opening.is_some() || self.restyler.worker().is_some()
    }

    /// How long until the clock of the home screen shows a new minute.
    /// Nothing when the home screen is not up, or the app is asleep.
    fn home_clock_deadline(&self) -> Option<Duration> {
        if self.asleep.is_some() || !matches!(self.screen, Screen::Home { .. }) {
            return None;
        }
        let t = system::local_time()?;
        Some(Duration::from_secs((60 - t.second as u64).clamp(1, 60)))
    }

    /// A new minute on the home screen: the time changes, and only the
    /// word of the clock is drawn again. At the top of the hour the whole
    /// screen is drawn, so the date and the lunar calendar change with it.
    fn tick_home_clock(&mut self) -> Result<(), Error> {
        let Screen::Home { .. } = &self.screen else {
            return Ok(());
        };
        let Some(t) = system::local_time() else {
            return Ok(());
        };
        if (t.hour, t.minute) == self.home_clock {
            return Ok(());
        }
        let time = fmt_time(t.hour, t.minute);
        let Screen::Home { view, .. } = &self.screen else {
            return Ok(());
        };
        view.set_time(&mut self.ui.tree, &time);
        self.ui.layout();
        self.home_clock = (t.hour, t.minute);
        if t.minute == 0 {
            // The top of the hour: the date may have turned too, so the
            // whole screen is drawn again.
            return self.show_home(Refresh::Full);
        }
        Ok(())
    }

    fn run(&mut self, input: &mut Input) -> Result<(), Error> {
        let worker = wake::fd()?;
        loop {
            if STOP.load(Ordering::Relaxed) {
                return Ok(());
            }
            let timeout = self.poll_timeout(monotonic_us(), Instant::now());
            let events = match input.poll_with_worker(timeout, self.sleep_ctl.fd(), Some(worker)) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            wake::drain();
            // Input means the reader is looking at the screen: stop
            // repairing the first frame under their hands.
            if !events.touch.is_empty() || !events.keys.is_empty() {
                self.startup_watch = None;
            }
            for ev in &events.touch {
                if self.on_touch(ev)? == Flow::Quit {
                    return Ok(());
                }
            }
            for key in &events.keys {
                if self.on_key(*key)? == Flow::Quit {
                    return Ok(());
                }
            }
            if let Err(e) = self.on_power(Some(input)) {
                eprintln!("aristotle: error handling power/usb event: {e}");
            }
            self.watch_startup()?;

            // Item 5 / Review 3: Fallback bidirectional reconciliation between UsbMode and
            // volumd state. The lipc queries run in `usb_watch`'s thread, which wakes this
            // poll only when a powerd event was missed; nothing here waits for volumd.
            while let Some((mode, status)) = self.usb_watch.try_recv() {
                if mode != self.usb_mode {
                    continue;
                }
                let Some(event) = reconcile(self.usb_mode, status) else {
                    continue;
                };
                perf!("usb reconciliation: volumd disagrees with the app; applying {event:?}");
                if let Err(e) = self.on_usb_event(event, Some(input)) {
                    eprintln!("aristotle: error applying a reconciled USB event: {e}");
                }
            }

            // Item 1: While in DriveMode, completely suppress UI animations, reading tick,
            // UI rendering, background sync, and automatic sleep.
            if self.asleep.is_none() && self.usb_mode != UsbMode::DriveMode {
                let ticked = self.ui.tick(monotonic_us());
                for ev in ticked {
                    if self.on_ui_event(ev)? == Flow::Quit {
                        return Ok(());
                    }
                }
                self.tick_reading(monotonic_us())?;
                if (!events.touch.is_empty() || !events.keys.is_empty()) && self.ui_driven() {
                    self.render_ui()?;
                }
            }
            self.panel.poll();
            if self.usb_mode != UsbMode::DriveMode {
                self.background()?;
                let now = Instant::now();
                if self.sleep_deadline().is_some_and(|d| now >= d)
                    && self.sleep_ctl.request_sleep(now)
                {
                    self.fall_asleep(false)?;
                }
                match self.sleep_ctl.tick(Instant::now(), self.asleep.is_some()) {
                    powerd::Action::Sleep if self.asleep.is_none() => self.fall_asleep(false)?,
                    _ => {}
                }
            }
        }
    }

    /// powerd's events: its screensaver, waking, suspending.
    fn on_power(&mut self, mut input: Option<&mut Input>) -> Result<(), Error> {
        use aristotle_hal::powerd::PowerEvent as P;
        for ev in self.sleep_ctl.events() {
            match ev {
                P::GoingToScreenSaver(src) => {
                    perf!("power: goingToScreenSaver {src}");
                    if self.usb_mode == UsbMode::DriveMode {
                        self.sleep_ctl.powerd_took_over();
                    } else if self.asleep.is_none() {
                        self.fall_asleep(true)?;
                    } else {
                        self.sleep_ctl.powerd_took_over();
                    }
                }
                P::OutOfScreenSaver(src) => {
                    perf!("power: outOfScreenSaver {src}");
                    if self.usb_mode == UsbMode::DriveMode {
                        perf!("power: in DriveMode; deferring wake until USB drive mode ends");
                    } else if self.asleep.is_some() {
                        self.wake(true)?;
                        self.ui.reset_gestures();
                        if let Screen::Reader(r) = &mut self.screen {
                            r.gestures.reset();
                        }
                    }
                }
                P::ReadyToSuspend(level) => {
                    perf!("power: readyToSuspend level {level}");
                    if self.asleep.is_none() {
                        self.sleep_ctl.resync();
                    }
                }
                P::DriveMode(active) => {
                    perf!("power: driveModeStateChanged {active}");
                    if active {
                        self.on_usb_event(UsbEvent::DriveModeStarted, input.as_deref_mut())?;
                    } else {
                        self.on_usb_event(UsbEvent::DriveModeEnded, input.as_deref_mut())?;
                    }
                }
                P::UserstoreLikelyToUnmount => {
                    perf!("power: userstoreIsLikelyToUnMount");
                    self.on_usb_event(UsbEvent::DriveModeRequested, input.as_deref_mut())?;
                }
                P::UserstoreUnavailable => {
                    perf!("power: userstoreUnAvailable");
                    self.on_usb_event(UsbEvent::DriveModeStarted, input.as_deref_mut())?;
                }
                P::UserstoreAvailable => {
                    perf!("power: userstoreAvailable");
                    self.on_usb_event(UsbEvent::DriveModeEnded, input.as_deref_mut())?;
                }
                other => perf!("power: {other:?}"),
            }
        }
        Ok(())
    }

    fn on_usb_event(&mut self, event: UsbEvent, input: Option<&mut Input>) -> Result<(), Error> {
        let (next_mode, action) = handle_usb_transition(self.usb_mode, event);
        self.usb_mode = next_mode;
        self.usb_watch.set_mode(next_mode);
        match action {
            UsbAction::None => {}
            UsbAction::SaveAndFreeze => {
                perf!("usb: entering drive mode; settling in-flight work and saving progress");
                // 1. Settle in-flight restyle, cancel openings and worker tasks (Item 6):
                self.settle_restyle()?;
                self.finish_opening(true)?;
                self.cancel_pending();
                self.prewarm.cancel();
                self.ui.reset_gestures();
                if let Screen::Reader(r) = &mut self.screen {
                    r.gestures.reset();
                }

                // 2. Set current reading progress in library (in memory):
                if let Screen::Reader(r) = &self.screen {
                    let now = aristotle_library::library::now();
                    self.lib
                        .set_progress(&r.session.id, r.session.progress(now));
                }
                // Push before the radio stops; failures are queued.
                self.sync_push(false, true);
                self.readest_push(false, true);
                self.stop_wifi();

                // Only perform disk writes if userstore (/mnt/us) is still available (Review 1 & 2):
                let userstore_available =
                    aristotle_hal::powerd::is_userstore_available().unwrap_or(false);
                if userstore_available {
                    self.save_snapshot();
                    let _ = self.snapshots.sync(Duration::from_millis(500));

                    // 3. Confirm index flush before userstore unmounts with 1.5s timeout:
                    match self.saver.save_sync(&self.lib, Duration::from_millis(1500)) {
                        Ok(()) => {
                            perf!("usb: library index synced to disk successfully");
                            self.debounce.saved();
                        }
                        Err(e) => {
                            eprintln!(
                                "aristotle: failed to sync library index to disk before USB freeze: {e}"
                            );
                        }
                    }
                    self.speed.save();
                    self.stats.finish_page(Instant::now());
                    self.stats.flush();
                } else {
                    perf!(
                        "usb: userstore already unavailable; skipping disk writes, keeping progress in memory"
                    );
                }
            }
            UsbAction::RestoreAndResume => {
                perf!("usb: drive mode ended; restoring blanket state and screen");
                self.sleep_pictures.invalidate();
                // 1. Unload blanket usb module; unload blanket screensaver only if not asleep (Item 8, Review 9):
                if let Err(e) = aristotle_hal::powerd::blanket(false, "usb") {
                    eprintln!("aristotle: unloading blanket usb module: {e}");
                }
                if self.asleep.is_none() {
                    if let Err(e) = aristotle_hal::powerd::blanket(false, "screensaver") {
                        eprintln!("aristotle: unloading blanket screensaver module: {e}");
                    }
                }

                // 2. Reopen input devices if available (Item 4):
                if let Some(input) = input {
                    if let Err(e) = input.reopen() {
                        eprintln!(
                            "aristotle: failed to reopen input devices after USB drive mode: {e}"
                        );
                    }
                }

                // 3. Reset gestures and swallow remaining touch:
                self.ui.reset_gestures();
                if let Screen::Reader(r) = &mut self.screen {
                    r.gestures.reset();
                }
                self.swallow = true;

                // 4. Resume reading if not asleep (Review 9):
                if self.asleep.is_none() {
                    self.last_touch = Instant::now();
                    self.repaint_screen(Refresh::Full)?;
                }
            }
        }
        Ok(())
    }

    /// Whether the UI tree (not the page-turn path) owns input right now.
    fn ui_driven(&self) -> bool {
        match &self.screen {
            Screen::Home { .. }
            | Screen::Library { .. }
            | Screen::Search { .. }
            | Screen::Mine
            | Screen::About
            | Screen::ComingSoon { .. }
            | Screen::Settings { .. }
            | Screen::SleepImages { .. }
            | Screen::Typeset { .. }
            | Screen::Network(_)
            | Screen::Scan { .. }
            | Screen::QuoteLibrary { .. }
            | Screen::Marks { .. }
            | Screen::QuoteAdd { .. }
            | Screen::Calendar { .. }
            | Screen::BookStats { .. }
            | Screen::WifiTransfer { .. }
            | Screen::ReadestLogin(_)
            | Screen::ReadestStatus
            | Screen::ReadestEdit(_)
            | Screen::Cloud(_) => true,
            Screen::Reader(r) => r.menu_open || !r.overlay.is_none(),
            Screen::Opening => self.opening.as_ref().is_some_and(|o| o.menu_open),
        }
    }

    fn prewarm_allowed(&self) -> bool {
        matches!(self.screen, Screen::Library { .. })
            && self.pending.is_none()
            && self.asleep.is_none()
    }

    /// Work done between events: adopting page numbers, saving progress,
    /// the clock in the status line.
    fn background(&mut self) -> Result<(), Error> {
        if self.usb_mode == UsbMode::DriveMode {
            return Ok(());
        }
        if self.asleep.is_some() {
            self.poll_net()?;
            return Ok(());
        }
        if let Some(result) = self.opening.as_ref().and_then(|o| o.worker.poll()) {
            self.adopt(result, false)?;
        }
        if let Some(result) = self.pending.as_ref().and_then(|p| p.worker.poll()) {
            self.finish_pending(result)?;
        }
        // The book being laid out again after a change of typography: start
        // the layout when the changes have settled, take it over when it
        // comes back.
        self.poll_restyle()?;
        self.poll_lookup()?;
        self.poll_net()?;
        self.poll_scan()?;
        self.poll_covers()?;
        self.poll_wifi()?;
        self.poll_formatting_progress()?;
        self.poll_pending_loading()?;
        self.tick_home_clock()?;
        // Snapshots ahead of time: only on the library screen (not the
        // settings), with no book opening and no screensaver.
        if self.prewarm_allowed() {
            let style = self.page_style();
            self.prewarm.tick(
                &self.lib,
                &style,
                &self.mobi_cache,
                self.speed.secs_per_page(),
            );
        }
        if let Screen::Reader(r) = &mut self.screen {
            if r.session.poll_pagination() {
                let (pn, pp) = r.session.prerender();
                perf!(
                    "paginate: page numbers adopted; prerender again {:.0} + {:.0} ms",
                    ms(pn),
                    ms(pp)
                );
            }
        }
        if self.debounce.due(Instant::now()) {
            self.saver.save(&self.lib);
            self.debounce.saved();
        }
        Ok(())
    }

    fn poll_formatting_progress(&mut self) -> Result<(), Error> {
        let (progress, nodes, label) = if let Some(o) = &self.opening {
            (
                &o.worker.progress,
                Some(o.progress),
                formatting_label(&o.title),
            )
        } else if let Some(w) = self.restyler.worker() {
            let pb = match &self.screen {
                Screen::Reader(r) => Some(r.progress),
                _ => None,
            };
            (&w.progress, pb, restyle_label())
        } else {
            if self.formatting_progress.is_active() {
                let action = self.formatting_progress.finish();
                if action == ProgressAction::Hide {
                    self.hide_formatting_progress()?;
                }
            }
            return Ok(());
        };

        let raw = progress.load(Ordering::Relaxed);
        let now = Instant::now();
        let action = self.formatting_progress.update(raw, now);
        match action {
            ProgressAction::Show(p) => {
                if let Some(nodes) = nodes {
                    self.show_formatting_progress(nodes, &label, p)?;
                }
            }
            ProgressAction::Update(p) => {
                if let Some(nodes) = nodes {
                    self.update_formatting_progress(nodes, &label, p)?;
                }
            }
            ProgressAction::Hide => {
                if let Some(nodes) = nodes {
                    self.hide_formatting_progress_node(nodes)?;
                }
            }
            ProgressAction::None => {}
        }
        Ok(())
    }

    /// The book opening in the background: the status bar of the screen it
    /// was opened from says what is going on. Nothing is said until the
    /// layout has taken long enough to be worth a word, and then the bar
    /// moves as the engine reports it.
    fn poll_pending_loading(&mut self) -> Result<(), Error> {
        let Some(p) = &self.pending else {
            return Ok(());
        };
        let raw = p.worker.progress.load(Ordering::Relaxed);
        let action = self.loading_progress.update(raw, Instant::now());
        let loading = match action {
            ProgressAction::Show(percent) | ProgressAction::Update(percent) => {
                if p.reports_progress {
                    Some(Loading::with_percent(formatting_label(&p.title), percent))
                } else {
                    // The engine says nothing about how far it has come:
                    // the bar would stand at zero, so only the words.
                    Some(Loading::without_percent(opening_label(&p.title)))
                }
            }
            ProgressAction::Hide => None,
            ProgressAction::None => return Ok(()),
        };
        self.set_loading(LoadingSource::Opening, loading);
        // The bar says it now: the user is waiting for the book.
        self.render_ui()?;
        Ok(())
    }

    /// The progress bar of the reader, with the line of text beside it.
    fn show_formatting_progress(
        &mut self,
        nodes: ProgressNodes,
        label: &str,
        percent: u8,
    ) -> Result<(), Error> {
        self.set_progress_text(nodes, label, percent);
        self.ui.tree.set_hidden(nodes.bar, false);
        if let Some(w) = self
            .ui
            .tree
            .widget_mut::<aristotle_ui::widgets::ProgressBar>(nodes.bar)
        {
            w.percent = percent;
        }
        self.ui.tree.invalidate(nodes.row, Refresh::Transient);
        self.render_ui()?;
        Ok(())
    }

    fn update_formatting_progress(
        &mut self,
        nodes: ProgressNodes,
        label: &str,
        percent: u8,
    ) -> Result<(), Error> {
        self.set_progress_text(nodes, label, percent);
        if let Some(w) = self
            .ui
            .tree
            .widget_mut::<aristotle_ui::widgets::ProgressBar>(nodes.bar)
        {
            w.percent = percent;
        }
        self.ui.tree.invalidate(nodes.row, Refresh::Transient);
        self.render_ui()?;
        Ok(())
    }

    /// The line of text beside the progress bar: what is being laid out,
    /// and how far it has come.
    fn set_progress_text(&mut self, nodes: ProgressNodes, label: &str, percent: u8) {
        let text = Loading::with_percent(label, percent).text();
        self.ui.tree.set_text(nodes.text, &text);
    }

    fn hide_formatting_progress_node(&mut self, nodes: ProgressNodes) -> Result<(), Error> {
        // The line of text goes with the bar: it says what the bar shows.
        self.ui.tree.set_text(nodes.text, "");
        if !self.ui.tree.node(nodes.bar).hidden {
            self.ui.tree.set_hidden(nodes.bar, true);
            self.ui.tree.invalidate(nodes.row, Refresh::Transient);
            self.render_ui()?;
        }
        Ok(())
    }

    fn hide_formatting_progress(&mut self) -> Result<(), Error> {
        let nodes = if let Some(o) = &self.opening {
            Some(o.progress)
        } else if let Screen::Reader(r) = &self.screen {
            Some(r.progress)
        } else {
            None
        };
        if let Some(nodes) = nodes {
            self.hide_formatting_progress_node(nodes)?;
        }
        Ok(())
    }

    fn on_touch(&mut self, ev: &aristotle_hal::input::TouchEvent) -> Result<Flow, Error> {
        if self.usb_mode == UsbMode::DriveMode {
            return Ok(Flow::Continue);
        }
        if self.asleep.is_some() {
            // Only the non-suspending fallback wakes on touch.
            if ev.phase == HalPhase::Down {
                if self.sleep_ctl.touch_asleep(Instant::now()) {
                    self.wake(false)?;
                }
                self.ui.reset_gestures();
                if let Screen::Reader(r) = &mut self.screen {
                    r.gestures.reset();
                }
                self.swallow = true;
            }
            return Ok(Flow::Continue);
        }
        if self.swallow {
            if ev.phase == HalPhase::Up {
                self.swallow = false;
            }
            return Ok(Flow::Continue);
        }
        self.last_touch = Instant::now();
        self.sleep_ctl.touched();
        let ev = ui_event(ev);
        self.prewarm.input();
        if self.opening.is_some() {
            return self.on_opening_touch(ev);
        }
        if let Screen::Reader(r) = &mut self.screen {
            // The page's gestures own the touch unless the menu or an
            // overlay is up; a hold keeps it until the finger lifts.
            if r.gestures.holding() || (!r.menu_open && r.overlay.is_none()) {
                let intents = r.gestures.feed(ev);
                for i in intents {
                    self.on_intent(i)?;
                }
                return Ok(Flow::Continue);
            }
        }
        for e in self.ui.handle_touch(ev) {
            if self.on_ui_event(e)? == Flow::Quit {
                return Ok(Flow::Quit);
            }
        }
        Ok(Flow::Continue)
    }

    /// A press of one of the page-turn buttons. They turn pages in the
    /// reader; the menu and an overlay keep them.
    fn on_key(&mut self, key: Key) -> Result<Flow, Error> {
        if self.usb_mode == UsbMode::DriveMode || self.asleep.is_some() {
            return Ok(Flow::Continue);
        }
        self.last_touch = Instant::now();
        self.sleep_ctl.touched();
        let dir = match key {
            Key::PageForward => Turn::Forward,
            Key::PageBack => Turn::Back,
        };
        if let Screen::Reader(r) = &mut self.screen {
            if r.menu_open || !r.overlay.is_none() || r.gestures.holding() {
                return Ok(Flow::Continue);
            }
        } else if let Screen::Settings { ids: Some(ids), .. } = &mut self.screen {
            let action_id = match dir {
                Turn::Forward => set_ui::action::NEXT_PAGE,
                Turn::Back => set_ui::action::PREV_PAGE,
            };
            let list = ids.list;
            let page = self.ui.tree.page(list).0;
            if ids.handle(&mut self.ui.tree, action_id) {
                self.ui.layout();
                ids.sync(&mut self.ui.tree);
                let now = self.ui.tree.page(list).0;
                if now != page {
                    self.ui.request_clean_refresh();
                }
                if let Screen::Settings { page: p, .. } = &mut self.screen {
                    *p = now;
                }
            }
            return Ok(Flow::Continue);
        } else if let Screen::Marks {
            page, total_pages, ..
        } = self.screen
        {
            let new_page = match dir {
                Turn::Forward => (page + 1).min(total_pages),
                Turn::Back => page.saturating_sub(1).max(1),
            };
            if new_page != page {
                self.show_marks(Refresh::Full, new_page)?;
            }
            return Ok(Flow::Continue);
        } else if let Screen::QuoteLibrary { tab, page } = self.screen {
            let total = match tab {
                ui_home::quotes::QuoteCategory::Reading => self.quotes.reading_count(),
                ui_home::quotes::QuoteCategory::Mood => self.quotes.mood_count(),
            };
            let total_pages = (total + 3) / 4;
            let new_page = match dir {
                Turn::Forward => (page + 1).min(total_pages),
                Turn::Back => page.saturating_sub(1).max(1),
            };
            if new_page != page {
                self.show_quote_library_page(Refresh::Full, tab, new_page)?;
            }
            return Ok(Flow::Continue);
        } else if matches!(self.screen, Screen::SleepImages { .. }) {
            // The picker promises page keys in its hint, and the hardware
            // buttons take the same route as the swipe: the same actions,
            // so the page edges are the same too.
            let id = match dir {
                Turn::Forward => ui_sleep_images::action::NEXT_PAGE,
                Turn::Back => ui_sleep_images::action::PREV_PAGE,
            };
            self.sleep_images_action(id)?;
            return Ok(Flow::Continue);
        } else {
            // Outside the reader the buttons do nothing.
            return Ok(Flow::Continue);
        }
        self.on_intent(Intent::Turn {
            dir,
            at_us: monotonic_us(),
        })?;
        Ok(Flow::Continue)
    }

    fn on_ui_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        // The bottom bar is shared chrome: its actions belong to no
        // screen, so they are answered before the screen sees them.
        if let UiEvent::Action { id, .. } = ev {
            if let Some(tab) = ui_nav::tab_of(id) {
                if tab != self.section {
                    self.switch_section(tab)?;
                }
                return Ok(Flow::Continue);
            }
        }
        match &self.screen {
            Screen::Home { .. } => self.on_home_event(ev),
            Screen::Mine => self.on_mine_event(ev),
            Screen::Calendar { .. } => self.on_calendar_event(ev),
            Screen::BookStats { .. } => self.on_book_stats_event(ev),
            Screen::About => self.on_about_event(ev),
            Screen::ComingSoon { .. } => self.on_coming_soon_event(ev),
            Screen::Library { .. } => self.on_library_event(ev),
            Screen::Search { .. } => self.on_search_event(ev),
            Screen::Settings { .. } => self.on_settings_event(ev),
            Screen::SleepImages { .. } => self.on_sleep_images_event(ev),
            Screen::Typeset { .. } => self.on_typeset_event(ev),
            Screen::Network(_) => self.on_network_event(ev),
            Screen::Scan { .. } => self.on_scan_event(ev),
            Screen::QuoteLibrary { .. } => self.on_quote_library_event(ev),
            Screen::Marks { .. } => self.on_marks_event(ev),
            Screen::QuoteAdd { .. } => self.on_quote_add_event(ev),
            Screen::WifiTransfer { .. } => self.on_wifi_event(ev),
            Screen::ReadestLogin(_) => self.on_readest_login_event(ev),
            Screen::ReadestStatus => self.on_readest_status_event(ev),
            Screen::ReadestEdit(_) => self.on_readest_edit_event(ev),
            Screen::Cloud(_) => self.on_cloud_event(ev),
            Screen::Reader(_) => {
                if self.reader_event_needs_book(&ev) {
                    self.settle_restyle()?;
                }
                let Screen::Reader(r) = &self.screen else {
                    return Ok(Flow::Continue);
                };
                let reading_action =
                    matches!(ev, UiEvent::Action { id, .. } if id >= overlay::FIRST_ACTION);
                if !r.overlay.is_none() || reading_action {
                    self.on_overlay_event(ev).map(|_| Flow::Continue)
                } else {
                    self.on_menu_event(ev).map(|_| Flow::Continue)
                }
            }
            Screen::Opening => self.on_opening_ui_event(ev),
        }
    }

    /// The home screen: the card continues the book being read, the row
    /// opens the books opened lately, and the heading of that row opens
    /// the library.
    fn on_home_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_home::action as a;
        // The book's own actions come first: they are on the screen, and
        // their ids are above the home screen's.
        if self.on_book_actions_event(&ev)? {
            return Ok(Flow::Continue);
        }
        match ev {
            // A swipe sideways turns to the next page of the books opened
            // lately.
            UiEvent::Swipe(Direction::Left) => self.home_page(1)?,
            UiEvent::Swipe(Direction::Right) => self.home_page(-1)?,
            UiEvent::LongAction { id, node, .. } => {
                // A book of the row held down opens its actions.
                let book = match id {
                    id if id >= a::RECENT => {
                        let Screen::Home { model, .. } = &self.screen else {
                            return Ok(Flow::Continue);
                        };
                        model
                            .recent
                            .get((id - a::RECENT) as usize)
                            .map(|(id, _)| id.clone())
                    }
                    _ => None,
                };
                if let Some(book) = book {
                    self.open_book_actions(&book, node)?;
                }
            }
            UiEvent::Action { id, node, .. } => match id {
                a::RESUME => {
                    let Screen::Home { model, .. } = &self.screen else {
                        return Ok(Flow::Continue);
                    };
                    if let Some((id, _)) = model.reading.as_ref() {
                        let id = id.clone();
                        self.open_from_library(&id, node)?;
                    }
                }
                a::ALL_BOOKS => self.show_library(Refresh::Full)?,
                a::NEXT_QUOTE => {
                    // Another quotation from the active pool: only the card of it is drawn
                    // again, the rest of the screen stays as it is.
                    let mode: ui_home::quotes::QuoteMode = self.prefs.quote_mode.into();
                    let pool = ui_home::quotes::active_pool(
                        mode,
                        |id| self.prefs.favorite_quotes.contains(&id),
                        |id| self.prefs.hidden_quotes.contains(&id),
                        self.prefs.fixed_quote_id,
                        self.quotes.all(),
                    );
                    let next_quote = ui_home::quotes::next_in_pool(self.home_quote_id, &pool);
                    self.home_quote_id = next_quote.id;
                    let Screen::Home { view, .. } = &self.screen else {
                        return Ok(Flow::Continue);
                    };
                    let brand = tr(Msg::HomeQuoteBrand);
                    view.set_quote(&mut self.ui.tree, next_quote, brand);
                    self.ui.layout();
                }
                id if id >= a::RECENT => {
                    let Screen::Home { model, .. } = &self.screen else {
                        return Ok(Flow::Continue);
                    };
                    let i = (id - a::RECENT) as usize;
                    if let Some((id, _)) = model.recent.get(i) {
                        let id = id.clone();
                        self.open_from_library(&id, node)?;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// Turns to the next page of the books opened lately. Only that block
    /// is drawn again; the rest of the screen stays as it is.
    fn home_page(&mut self, delta: i32) -> Result<(), Error> {
        let Screen::Home { view, .. } = &self.screen else {
            return Ok(());
        };
        let (page, count) = self.ui.tree.page(view.recent_pages);
        if count < 2 {
            return Ok(());
        }
        let next = (page as i32 + delta).clamp(0, count as i32 - 1) as usize;
        if next == page {
            return Ok(());
        }
        self.ui.tree.set_page(view.recent_pages, next);
        self.ui.layout();
        view.sync(&mut self.ui.tree);
        // Only the block of books changes, so only it is drawn again.
        // The screen is not flashed: the rest of it has not changed.
        Ok(())
    }

    /// The "mine" screen: the settings, the way out, and the about page.
    fn on_mine_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_mine::action as a;
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        match id {
            a::SETTINGS => self.show_settings(Refresh::Full, 0)?,
            a::RETURN_KINDLE => {
                self.exit_target = launcher::Target::Kindle;
                return Ok(Flow::Quit);
            }
            a::OPEN_KUAL => {
                if self.kual_available {
                    self.exit_target = launcher::Target::Kual;
                    return Ok(Flow::Quit);
                }
                self.settings_message = Some(
                    aristotle_i18n::tr(aristotle_i18n::Msg::SettingsKualUnavailable).to_string(),
                );
            }
            a::ABOUT => self.show_about(Refresh::Full)?,
            // The features the prototype links to are not built yet: they
            // open the page that says so.
            a::CALENDAR | a::STATS_CALENDAR | a::ENTRY_CALENDAR => {
                self.show_calendar(Refresh::Full)?
            }
            a::ENTRY_MARKS => {
                self.show_marks(Refresh::Full, 1)?;
            }
            a::ENTRY_QUOTES => {
                self.show_quote_library(Refresh::Full)?;
            }
            a::ACCOUNT => {
                if self.net.accounts.readest.is_some() {
                    self.show_readest_status(Refresh::Full)?;
                } else {
                    self.show_readest_login(Refresh::Full)?;
                }
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// The reading calendar: the month, the day picked, and the way back.
    fn on_calendar_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_cal::action as a;
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        let Screen::Calendar {
            month,
            day,
            records,
        } = &mut self.screen
        else {
            return Ok(Flow::Continue);
        };
        let month = *month;
        let day = *day;
        // The stays stay in memory while the calendar is open, so picking
        // a day costs no file reading.
        let records = std::mem::take(records);
        if id == a::BACK {
            self.show_mine(Refresh::Full)?;
            return Ok(Flow::Continue);
        }
        let of = |ts: i64| crate::stats::date_of(ts);
        let date = crate::stats::LocalDate {
            year: month.0,
            month: month.1,
            day,
        };
        let books = self.day_books(&records, date, &of);
        if id >= a::BOOK && (id - a::BOOK) < books.len() as u32 {
            let idx = (id - a::BOOK) as usize;
            let book = &books[idx];
            self.show_book_stats(Refresh::Full, &book.hash, month, day, records)?;
            return Ok(Flow::Continue);
        }
        if id == a::PREV_MONTH {
            let (year, number) = crate::stats::month_step(month.0, month.1, -1);
            self.show_calendar_at(Refresh::Full, (year, number), 1, records)?;
            return Ok(Flow::Continue);
        }
        if id == a::NEXT_MONTH {
            let (year, number) = crate::stats::month_step(month.0, month.1, 1);
            self.show_calendar_at(Refresh::Full, (year, number), 1, records)?;
            return Ok(Flow::Continue);
        }
        if id >= a::FIRST_DAY {
            let picked = (id - a::FIRST_DAY + 1) as u8;
            // A day that is not on the screen is ignored.
            if picked <= crate::stats::days_in_month(month.0, month.1) {
                // Only the day and the card below it change: the rest of
                // the screen stays, so the update is a partial one.
                self.show_calendar_at(Refresh::Auto, month, picked, records)?;
                return Ok(Flow::Continue);
            }
        }
        // An action the screen does not know: the calendar stays as it is.
        self.screen = Screen::Calendar {
            month,
            day,
            records,
        };
        Ok(Flow::Continue)
    }

    /// Single book reading statistics: back to calendar, or open the book.
    fn on_book_stats_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        let Screen::BookStats {
            book_hash,
            return_month,
            return_day,
            records,
        } = &mut self.screen
        else {
            return Ok(Flow::Continue);
        };
        let book_hash = book_hash.clone();
        let return_month = *return_month;
        let return_day = *return_day;
        let records = std::mem::take(records);

        if id == ui_book_stats::action::BACK {
            self.show_calendar_at(Refresh::Full, return_month, return_day, records)?;
            return Ok(Flow::Continue);
        }
        if id == ui_book_stats::action::OPEN {
            self.open_book(&book_hash, None)?;
            return Ok(Flow::Continue);
        }

        self.screen = Screen::BookStats {
            book_hash,
            return_month,
            return_day,
            records,
        };
        Ok(Flow::Continue)
    }

    /// The about page: back to the "mine" screen.
    fn on_about_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        if id == ui_about::action::BACK {
            self.show_mine(Refresh::Full)?;
        }
        Ok(Flow::Continue)
    }

    /// Events on "My highlights and notes": paging, and opening a book at
    /// the tapped highlight.
    fn on_marks_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_marks::action as a;
        let Screen::Marks {
            page, total_pages, ..
        } = self.screen
        else {
            return Ok(Flow::Continue);
        };
        let page = page;
        let action_id = match ev {
            UiEvent::Action { id, .. } => id,
            UiEvent::Swipe(dir) => match dir {
                Direction::Up | Direction::Left => a::NEXT_PAGE,
                Direction::Down | Direction::Right => a::PREV_PAGE,
            },
            _ => return Ok(Flow::Continue),
        };

        let back = action_id == a::BACK;
        let mut go_page = None;
        let mut open = None;
        match action_id {
            a::PREV_PAGE if page > 1 => go_page = Some(page - 1),
            a::NEXT_PAGE if page < total_pages => go_page = Some(page + 1),
            id if id >= a::CARD_BASE => {
                // The action carries the row's place in the whole list, so
                // it stays valid whatever the page holds.
                let index = (id - a::CARD_BASE) as usize;
                let all = aristotle_library::marks::entries(self.lib.index());
                if let Some(entry) = all.get(index) {
                    if !entry.missing {
                        open = Some((entry.book_id.clone(), entry.annotation.clone()));
                    }
                }
            }
            _ => {}
        }

        if back {
            self.show_mine(Refresh::Full)?;
        } else if let Some(page) = go_page {
            self.show_marks(Refresh::Full, page)?;
        } else if let Some((id, annotation)) = open {
            self.open_book_at(&id, None, Some(annotation))?;
        }
        Ok(Flow::Continue)
    }

    /// Events on the quotation library screen.
    fn on_quote_library_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_quotes::action as a;
        let Screen::QuoteLibrary { tab, page } = self.screen else {
            return Ok(Flow::Continue);
        };
        let (tab, page) = (tab, page);

        let all_quotes = self.quotes.all();
        let filtered: Vec<&ui_home::quotes::QuoteItem> =
            all_quotes.iter().filter(|q| q.category == tab).collect();
        let total_pages = (filtered.len() + 3) / 4;

        let action_id = match ev {
            UiEvent::Action { id, .. } => id,
            UiEvent::Swipe(dir) => match dir {
                Direction::Up | Direction::Left => a::NEXT_PAGE,
                Direction::Down | Direction::Right => a::PREV_PAGE,
            },
            _ => return Ok(Flow::Continue),
        };

        match action_id {
            a::BACK => {
                self.show_mine(Refresh::Full)?;
            }
            a::TAB_READING => {
                if tab != ui_home::quotes::QuoteCategory::Reading {
                    self.show_quote_library_page(
                        Refresh::Full,
                        ui_home::quotes::QuoteCategory::Reading,
                        1,
                    )?;
                }
            }
            a::TAB_MOOD => {
                if tab != ui_home::quotes::QuoteCategory::Mood {
                    self.show_quote_library_page(
                        Refresh::Full,
                        ui_home::quotes::QuoteCategory::Mood,
                        1,
                    )?;
                }
            }
            a::MODE_ALL_RANDOM => {
                if self.prefs.quote_mode != prefs::QuoteMode::AllRandom {
                    self.prefs.quote_mode = prefs::QuoteMode::AllRandom;
                    self.save_prefs();
                    self.show_quote_library_page(Refresh::Full, tab, page)?;
                }
            }
            a::MODE_FAVORITES_RANDOM => {
                if self.prefs.quote_mode != prefs::QuoteMode::FavoritesRandom {
                    self.prefs.quote_mode = prefs::QuoteMode::FavoritesRandom;
                    self.save_prefs();
                    self.show_quote_library_page(Refresh::Full, tab, page)?;
                }
            }
            a::MODE_FIXED => {
                if self.prefs.quote_mode != prefs::QuoteMode::Fixed {
                    self.prefs.quote_mode = prefs::QuoteMode::Fixed;
                    if self.prefs.fixed_quote_id.is_none() {
                        self.prefs.fixed_quote_id = Some(self.home_quote_id);
                    }
                    self.save_prefs();
                    self.show_quote_library_page(Refresh::Full, tab, page)?;
                }
            }
            a::ADD_QUOTE => {
                self.show_quote_add(Refresh::Full, tab, page)?;
            }
            a::PREV_PAGE => {
                if page > 1 {
                    self.show_quote_library_page(Refresh::Full, tab, page - 1)?;
                }
            }
            a::NEXT_PAGE => {
                if page < total_pages {
                    self.show_quote_library_page(Refresh::Full, tab, page + 1)?;
                }
            }
            id if (a::CARD_BASE..a::CARD_BASE + 4).contains(&id) => {
                let idx = (id - a::CARD_BASE) as usize;
                let start = (page - 1) * 4;
                if let Some(item) = filtered.get(start + idx) {
                    if self.prefs.quote_mode == prefs::QuoteMode::Fixed {
                        self.prefs.fixed_quote_id = Some(item.id);
                        self.save_prefs();
                        self.show_quote_library_page(Refresh::Full, tab, page)?;
                    }
                }
            }
            id if (a::FAVORITE_BASE..a::FAVORITE_BASE + 4).contains(&id) => {
                let idx = (id - a::FAVORITE_BASE) as usize;
                let start = (page - 1) * 4;
                if let Some(item) = filtered.get(start + idx) {
                    if self.prefs.favorite_quotes.contains(&item.id) {
                        self.prefs.favorite_quotes.remove(&item.id);
                    } else {
                        self.prefs.favorite_quotes.insert(item.id);
                    }
                    self.save_prefs();
                    self.show_quote_library_page(Refresh::Full, tab, page)?;
                }
            }
            id if (a::HIDE_BASE..a::HIDE_BASE + 4).contains(&id) => {
                let idx = (id - a::HIDE_BASE) as usize;
                let start = (page - 1) * 4;
                if let Some(item) = filtered.get(start + idx) {
                    self.prefs.hidden_quotes.insert(item.id);
                    self.save_prefs();
                    self.show_quote_library_page(Refresh::Full, tab, page)?;
                }
            }
            id if (a::RESTORE_BASE..a::RESTORE_BASE + 4).contains(&id) => {
                let idx = (id - a::RESTORE_BASE) as usize;
                let start = (page - 1) * 4;
                if let Some(item) = filtered.get(start + idx) {
                    self.prefs.hidden_quotes.remove(&item.id);
                    self.save_prefs();
                    self.show_quote_library_page(Refresh::Full, tab, page)?;
                }
            }
            id if (a::DELETE_BASE..a::DELETE_BASE + 4).contains(&id) => {
                let idx = (id - a::DELETE_BASE) as usize;
                let start = (page - 1) * 4;
                if let Some(item) = filtered.get(start + idx) {
                    if self.quotes.is_custom(item.id) {
                        self.pending_delete_quote = Some(item.id);
                        self.show_quote_library_page(Refresh::Auto, tab, page)?;
                    }
                }
            }
            a::DIALOG_CANCEL => {
                self.pending_delete_quote = None;
                self.show_quote_library_page(Refresh::Auto, tab, page)?;
            }
            a::DIALOG_CONFIRM => {
                if let Some(id) = self.pending_delete_quote.take() {
                    let _ = self.delete_custom_quote(id);
                    self.show_quote_library_page(Refresh::Full, tab, page)?;
                }
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// Events on the quotation entry screen.
    fn on_quote_add_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let Screen::QuoteAdd {
            ref mut state,
            return_tab,
            return_page,
        } = self.screen
        else {
            return Ok(Flow::Continue);
        };
        let return_tab = return_tab;
        let return_page = return_page;

        let action_id = match ev {
            UiEvent::Action { id, .. } => id,
            _ => return Ok(Flow::Continue),
        };

        match state.handle_action(action_id) {
            ui_quote_add::QuoteAddOutcome::Cancel => {
                self.show_quote_library_page(Refresh::Full, return_tab, return_page)?;
            }
            ui_quote_add::QuoteAddOutcome::Save {
                text,
                author,
                source,
                category,
            } => {
                match self
                    .quotes
                    .add_custom(self.lib.data_dir(), text, author, source, category)
                {
                    Ok(item) => {
                        let total_in_cat = self
                            .quotes
                            .all()
                            .iter()
                            .filter(|q| q.category == item.category)
                            .count();
                        let target_page = (total_in_cat + 3) / 4;
                        self.show_quote_library_page(
                            Refresh::Full,
                            item.category,
                            target_page.max(1),
                        )?;
                    }
                    Err(e) => {
                        eprintln!("aristotle: failed to save custom quote: {e}");
                        self.show_quote_library_page(Refresh::Full, return_tab, return_page)?;
                    }
                }
            }
            ui_quote_add::QuoteAddOutcome::Changed => {
                let state_clone = state.clone();
                self.show_quote_add_with_state(
                    Refresh::Auto,
                    state_clone,
                    return_tab,
                    return_page,
                )?;
            }
            ui_quote_add::QuoteAddOutcome::None => {}
        }

        Ok(Flow::Continue)
    }

    /// The page of a feature that is not built yet: the way back.
    fn on_coming_soon_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        if id == ui_soon::action::BACK {
            let back = match &self.screen {
                Screen::ComingSoon { back, .. } => *back,
                _ => SoonBack::Mine,
            };
            match back {
                SoonBack::Mine => self.show_mine(Refresh::Full)?,
                SoonBack::Library { page } => self.show_library_at(Refresh::Full, page)?,
                SoonBack::Settings { page } => self.show_settings(Refresh::Full, page)?,
                SoonBack::SleepImages { page } => self.show_sleep_images(Refresh::Full, page)?,
                SoonBack::Scan => self.show_scan_empty(Refresh::Full)?,
                SoonBack::Quotes => self.show_quote_library(Refresh::Full)?,
                SoonBack::Wifi => self.show_wifi_transfer(Refresh::Full, SoonBack::Mine)?,
                SoonBack::Reader => {
                    if let Some(r) = self.saved_reading.take() {
                        self.screen = Screen::Reader(r);
                        self.rebuild_reader(Refresh::Full)?;
                        self.render_ui()?;
                        self.stats_page_shown();
                    } else {
                        self.show_library(Refresh::Full)?;
                    }
                }
            }
        }
        Ok(Flow::Continue)
    }

    fn on_wifi_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_wifi::action as a;
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        match id {
            a::BACK => {
                // Leaving the page stops the service: nothing is left
                // listening on the network.
                self.stop_wifi();
                let back = match &self.screen {
                    Screen::WifiTransfer { back, .. } => *back,
                    _ => SoonBack::Mine,
                };
                match back {
                    SoonBack::Mine => self.show_mine(Refresh::Full)?,
                    SoonBack::Library { page } => self.show_library_at(Refresh::Full, page)?,
                    SoonBack::Settings { page } => self.show_settings(Refresh::Full, page)?,
                    SoonBack::SleepImages { page } => {
                        self.show_sleep_images(Refresh::Full, page)?
                    }
                    SoonBack::Scan => self.show_scan_empty(Refresh::Full)?,
                    SoonBack::Quotes => self.show_quote_library(Refresh::Full)?,
                    SoonBack::Wifi => self.show_mine(Refresh::Full)?,
                    SoonBack::Reader => {
                        if let Some(r) = self.saved_reading.take() {
                            let refresh_fonts =
                                matches!(r.overlay, reading::Overlay::Typeset { .. });
                            self.screen = Screen::Reader(r);
                            self.rebuild_reader(Refresh::Full)?;
                            if refresh_fonts {
                                self.open_typeset()?;
                            } else {
                                self.render_ui()?;
                            }
                            self.stats_page_shown();
                        } else {
                            self.show_library(Refresh::Full)?;
                        }
                    }
                }
            }
            a::TOGGLE => {
                if self.wifi.is_some() {
                    // Stop: nothing listens on the network any more.
                    self.stop_wifi();
                } else {
                    // Start over.
                    self.start_wifi_service();
                }
                let back = match &self.screen {
                    Screen::WifiTransfer { back, .. } => *back,
                    _ => SoonBack::Mine,
                };
                self.build_wifi_transfer(Refresh::Full, back)?;
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// The folder the picker shows, as an absolute path. The scan root itself
    /// is the whole store.
    fn scan_picker_path(&self) -> Option<PathBuf> {
        let root = scan_root(&self.root);
        match &self.screen {
            Screen::Scan {
                state: ui_scan::ScanState::Sources { path, .. },
                ..
            } => {
                if path.is_empty() {
                    Some(root)
                } else {
                    Some(root.join(path))
                }
            }
            _ => Some(if self.scan_picker_path.is_empty() {
                root
            } else {
                root.join(&self.scan_picker_path)
            }),
        }
    }

    fn scan_picker_mode(&self) -> ui_scan::PickerMode {
        match &self.screen {
            Screen::Scan {
                state: ui_scan::ScanState::Sources { mode, .. },
                ..
            } => *mode,
            _ => ui_scan::PickerMode::Sources,
        }
    }

    fn show_scan_picker(&mut self, path: Option<&Path>) -> Result<(), Error> {
        let mode = self.scan_picker_mode();
        self.show_scan_sources(Refresh::Full, mode, path)
    }

    /// Checks or unchecks one folder of the scan sources. Checking a folder
    /// turns the whole-store choice off; checking the root is the whole store.
    fn toggle_scan_source(&mut self, relative: String) -> Result<(), Error> {
        if relative.is_empty() {
            self.scan_whole = !self.scan_whole;
            self.scan_sources.clear();
        } else if let Some(at) = self.scan_sources.iter().position(|p| p == &relative) {
            self.scan_sources.remove(at);
            self.scan_whole = false;
        } else {
            self.scan_sources.push(relative);
            self.scan_whole = false;
        }
        let path = self.scan_picker_path();
        self.show_scan_picker(path.as_deref())
    }

    /// Turns the source list one page forward or back.
    fn page_scan_folders(&mut self, forward: bool) -> Result<(), Error> {
        let Screen::Scan { view, .. } = &mut self.screen else {
            return Ok(());
        };
        let Some(list) = view.folder_list else {
            return Ok(());
        };
        let (page, count) = self.ui.tree.page(list);
        let next = if forward {
            (page + 1).min(count.saturating_sub(1))
        } else {
            page.saturating_sub(1)
        };
        if next == page {
            return Ok(());
        }
        self.ui.tree.set_page(list, next);
        self.ui.layout();
        if let Some(pager) = view.folder_pager {
            let text = tf!(Msg::LibraryPagerPageOf, page = next + 1, count = count);
            self.ui.tree.set_text(pager, &text);
        }
        self.ui.request_clean_refresh();
        self.render_ui()
    }

    /// Turns the review list one page forward or back, keeping the checks.
    fn page_review(&mut self, forward: bool) -> Result<(), Error> {
        let list = match &self.screen {
            Screen::Scan { view, .. } => view.review_list,
            _ => None,
        };
        let Some(list) = list else {
            return Ok(());
        };
        let (page, count) = self.ui.tree.page(list);
        let next = if forward {
            (page + 1).min(count.saturating_sub(1))
        } else {
            page.saturating_sub(1)
        };
        if next == page {
            return Ok(());
        }
        let pager = match &self.screen {
            Screen::Scan { view, .. } => view.review_pager,
            _ => None,
        };
        self.ui.tree.set_page(list, next);
        self.ui.layout();
        if let Some(pager) = pager {
            let text = tf!(Msg::LibraryPagerPageOf, page = next + 1, count = count);
            self.ui.tree.set_text(pager, &text);
        }
        self.ui.request_clean_refresh();
        self.render_ui()
    }

    /// The page of the review list the reader is on.
    fn review_page(&self) -> usize {
        match &self.screen {
            Screen::Scan { view, .. } => view
                .review_list
                .map(|list| self.ui.tree.page(list).0)
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// Checks or unchecks one row of the review. A book already inside the
    /// library folder has nothing to do and keeps its disabled box.
    fn toggle_review(&mut self, index: usize) -> Result<(), Error> {
        let Screen::Scan {
            state: ui_scan::ScanState::Review {
                books, selected, ..
            },
            ..
        } = &mut self.screen
        else {
            return Ok(());
        };
        if let Some(flag) = selected.get_mut(index) {
            if books.get(index).is_some_and(|book| book.selectable()) {
                *flag = !*flag;
            }
        }
        let Some(state) = self.screen_state() else {
            return Ok(());
        };
        let page = self.review_page();
        self.show_review(state, page)
    }

    fn set_all_review(&mut self, checked: bool) -> Result<(), Error> {
        let Screen::Scan {
            state: ui_scan::ScanState::Review {
                books, selected, ..
            },
            ..
        } = &mut self.screen
        else {
            return Ok(());
        };
        for (i, flag) in selected.iter_mut().enumerate() {
            *flag = checked && books.get(i).is_some_and(|book| book.selectable());
        }
        let Some(state) = self.screen_state() else {
            return Ok(());
        };
        let page = self.review_page();
        self.show_review(state, page)
    }

    fn on_scan_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_scan::action as a;
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        // Row ranges first: their bases are action ids too.
        // Tagged row namespaces, decoded against the rows actually shown.
        if id >= a::REVIEW_ITEM {
            self.toggle_review((id - a::REVIEW_ITEM) as usize)?;
            return Ok(Flow::Continue);
        }
        if id >= a::SOURCE_ITEM {
            if self.scan_picker_mode() == ui_scan::PickerMode::Sources {
                let relative = match &self.screen {
                    Screen::Scan {
                        state: ui_scan::ScanState::Sources { folders, .. },
                        ..
                    } => folders
                        .get((id - a::SOURCE_ITEM) as usize)
                        .map(|folder| folder.relative.clone()),
                    _ => None,
                };
                if let Some(relative) = relative {
                    self.toggle_scan_source(relative)?;
                }
            }
            return Ok(Flow::Continue);
        }
        if id >= a::SCOPE_ITEM {
            let index = (id - a::SCOPE_ITEM) as usize;
            let relative = match &self.screen {
                Screen::Scan {
                    state: ui_scan::ScanState::Sources { folders, .. },
                    ..
                } => folders.get(index).map(|folder| folder.relative.clone()),
                _ => None,
            };
            if let Some(relative) = relative {
                // The row is relative to the scan root, not to the shown
                // folder: joining it to the shown path would repeat a level.
                let path = scan_root(&self.root).join(relative);
                self.show_scan_picker(Some(&path))?;
            }
            return Ok(Flow::Continue);
        }
        match id {
            a::BACK => match &self.screen {
                Screen::Scan {
                    state:
                        ui_scan::ScanState::Sources {
                            mode: ui_scan::PickerMode::Destination,
                            ..
                        },
                    ..
                } => {
                    // Leaving the destination picker changes nothing, and
                    // returns to source selection explicitly.
                    let path = self.scan_picker_path();
                    self.show_scan_sources(
                        Refresh::Full,
                        ui_scan::PickerMode::Sources,
                        path.as_deref(),
                    )?;
                }
                Screen::Scan {
                    state: ui_scan::ScanState::Review { .. },
                    ..
                } => {
                    // Back from the review imports nothing.
                    self.show_scan_picker(None)?;
                }
                Screen::Scan {
                    state: ui_scan::ScanState::Importing { .. },
                    ..
                } => {
                    self.cancel_scan()?;
                }
                Screen::Scan { .. } => {
                    if self.lib.books().is_empty() {
                        self.show_home(Refresh::Full)?;
                    } else {
                        self.show_library(Refresh::Full)?;
                    }
                }
                _ => {}
            },
            a::START_SCAN | a::CHOOSE_SCOPE => {
                let path = self.scan_picker_path();
                self.show_scan_sources(
                    Refresh::Full,
                    ui_scan::PickerMode::Sources,
                    path.as_deref(),
                )?;
            }
            a::SCAN_SCOPE => {
                if let Some(task) = self.scan_task.take() {
                    task.cancel.store(true, Ordering::SeqCst);
                }
                self.start_scan()?;
            }
            a::SCOPE_PARENT => {
                let path = self.scan_picker_path();
                let parent = path.as_deref().and_then(|p| {
                    scan_scope::Scope::new(&scan_root(&self.root), p).and_then(|s| s.parent())
                });
                if let Some(parent) = parent {
                    self.show_scan_picker(Some(&parent))?;
                }
            }
            a::SOURCE_WHOLE => {
                self.scan_whole = !self.scan_whole;
                self.scan_sources.clear();
                let path = self.scan_picker_path();
                self.show_scan_picker(path.as_deref())?;
            }
            a::SOURCE_CURRENT => {
                let path = match &self.screen {
                    Screen::Scan {
                        state: ui_scan::ScanState::Sources { path, .. },
                        ..
                    } => path.clone(),
                    _ => self.scan_picker_path.clone(),
                };
                self.toggle_scan_source(path)?;
            }
            a::SOURCE_CLEAR => {
                self.scan_sources.clear();
                self.scan_whole = false;
                let path = self.scan_picker_path();
                self.show_scan_picker(path.as_deref())?;
            }
            a::DEST_CHOOSE => {
                let path = self.scan_picker_path();
                self.show_scan_sources(
                    Refresh::Full,
                    ui_scan::PickerMode::Destination,
                    path.as_deref(),
                )?;
            }
            a::DEST_USE => {
                let Some(path) = self.scan_picker_path() else {
                    return Ok(Flow::Continue);
                };
                let root = scan_root(&self.root);
                match resolve_library_destination(&root, &path) {
                    Ok(destination) => {
                        let previous = self.prefs.library_destination.replace(destination);
                        if let Err(error) = self.prefs.save(self.lib.data_dir()) {
                            self.prefs.library_destination = previous;
                            self.show_scan_failed(Refresh::Full, error.to_string())?;
                            return Ok(Flow::Continue);
                        }
                    }
                    Err(e) => {
                        self.show_scan_failed(Refresh::Full, e.to_string())?;
                        return Ok(Flow::Continue);
                    }
                }
                self.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Sources, Some(&path))?;
            }
            a::DEST_CANCEL => {
                let path = self.scan_picker_path();
                self.show_scan_sources(
                    Refresh::Full,
                    ui_scan::PickerMode::Sources,
                    path.as_deref(),
                )?;
            }
            a::PREV_PAGE | a::NEXT_PAGE => {
                self.page_scan_folders(id == a::NEXT_PAGE)?;
            }
            a::REVIEW_ALL => self.set_all_review(true)?,
            a::REVIEW_CLEAR => self.set_all_review(false)?,
            a::REVIEW_PREV => self.page_review(false)?,
            a::REVIEW_NEXT => self.page_review(true)?,
            a::REVIEW_CANCEL => self.show_scan_picker(None)?,
            a::REVIEW_CONFIRM => {
                self.confirm_import()?;
            }
            a::WIFI_TRANSFER => {
                self.show_wifi_transfer(Refresh::Full, SoonBack::Scan)?;
            }
            a::CLOUD_LIBRARY => {
                self.open_cloud()?;
            }
            a::CANCEL_SCAN => {
                self.cancel_scan()?;
            }
            a::VIEW_LIBRARY => {
                self.show_library(Refresh::Full)?;
            }
            a::RESCAN => {
                self.start_scan()?;
            }
            // The preview/device shortcut never imports: it only ever
            // belongs to a scan that stages its result (UX-112).
            a::FINISH_SCAN => {}
            _ => {}
        }
        Ok(Flow::Continue)
    }

    fn on_library_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_lib::action as a;
        // The book's own actions come first: they are on the screen, and
        // their ids are above the library's.
        if self.on_book_actions_event(&ev)? {
            return Ok(Flow::Continue);
        }
        // Anything but opening a book gives up a book still opening.
        let opens_book = match (&self.screen, &ev) {
            (Screen::Library { model, .. }, UiEvent::Action { id, .. }) => {
                *id == a::RESUME
                    || (*id >= a::ITEM
                        && *id < a::ITEM_MORE
                        && matches!(
                            model.target((*id - a::ITEM) as usize),
                            Some(Target::Book(_))
                        ))
            }
            _ => false,
        };
        if !opens_book {
            self.cancel_pending();
        }
        let Screen::Library { model, view } = &mut self.screen else {
            return Ok(Flow::Continue);
        };
        match ev {
            UiEvent::LongPress(p)
                if self.nav.at_home() && quit_zone(&self.ui, view.title).contains(p) =>
            {
                perf!("quit: title held");
                return Ok(Flow::Quit);
            }
            UiEvent::LongAction { id, node, .. } => {
                // A book held down opens its actions.
                let book = match id {
                    id if id >= a::ITEM_MORE => model.target((id - a::ITEM_MORE) as usize).cloned(),
                    id if id >= a::ITEM => model.target((id - a::ITEM) as usize).cloned(),
                    _ => None,
                };
                if let Some(Target::Book(book)) = book {
                    self.open_book_actions(&book, node)?;
                }
            }
            UiEvent::Swipe(dir) => {
                let old_page = self.ui.tree.page(view.list).0;
                if view.handle_swipe(&mut self.ui.tree, dir) {
                    self.ui.layout();
                    view.sync(&mut self.ui.tree);
                    model.load_covers(&self.lib, &mut self.covers, view, &mut self.ui.tree);
                    let new_page = self.ui.tree.page(view.list).0;
                    if new_page != old_page {
                        if self.prefs.covers.mode() != CoverMode::None {
                            self.ui.request_full_refresh();
                        } else {
                            self.ui.request_clean_refresh();
                        }
                    }
                }
            }
            UiEvent::Action { id, node, .. } => match id {
                a::RESUME => {
                    if let Some(book) = model.hero_id().map(String::from) {
                        self.open_from_library(&book, node)?;
                    }
                }
                a::SEARCH => {
                    self.show_search(Refresh::Full)?;
                }
                a::WIFI_TRANSFER => {
                    let page = self.ui.tree.page(view.list).0;
                    self.show_wifi_transfer(Refresh::Full, SoonBack::Library { page })?;
                }
                a::CLOUD_LIBRARY => {
                    self.open_cloud()?;
                }
                a::SCAN => {
                    // The sources are chosen first, and kept for the next
                    // scan; the destination stays as it was.
                    let path = self.scan_picker_path();
                    self.show_scan_sources(
                        Refresh::Full,
                        ui_scan::PickerMode::Sources,
                        path.as_deref(),
                    )?;
                }
                a::SORT => {
                    self.nav.sort = (self.nav.sort + 1) % SORTS.len();
                    self.prefs.sort = self.nav.sort;
                    self.save_prefs();
                    self.show_library(Refresh::Auto)?;
                }
                a::TOGGLE_LAYOUT => {
                    self.prefs.library_layout = match self.prefs.library_layout {
                        Layout::List => Layout::Grid,
                        Layout::Grid => Layout::List,
                    };
                    self.save_prefs();
                    self.show_library(Refresh::Full)?;
                }
                a::BACK => {
                    self.nav.up();
                    self.show_library(Refresh::Full)?;
                }
                id if id >= a::ITEM_MORE => {
                    let i = (id - a::ITEM_MORE) as usize;
                    if let Some(Target::Book(book)) = model.target(i).cloned() {
                        self.open_book_actions(&book, node)?;
                    }
                }
                id if id >= a::ITEM => match model.target((id - a::ITEM) as usize).cloned() {
                    Some(Target::Book(book)) => self.open_from_library(&book, node)?,
                    Some(Target::Folder(name)) => {
                        self.nav.folder.push(name);
                        self.show_library(Refresh::Full)?;
                    }
                    Some(Target::Series(name)) => {
                        self.nav.series = Some(name);
                        self.show_library(Refresh::Full)?;
                    }
                    None => {}
                },
                id => {
                    if let Some(i) = in_range(id, a::VIEW, View::ALL.len()) {
                        self.nav.view = View::ALL[i];
                        self.prefs.library_view = i;
                        self.save_prefs();
                        self.show_library(Refresh::Auto)?;
                    } else if let Some(i) = in_range(id, a::FILTER, Filter::ALL.len()) {
                        self.nav.filter = Filter::ALL[i];
                        self.show_library(Refresh::Auto)?;
                    } else if view.handle(&mut self.ui.tree, id) {
                        self.ui.layout();
                        view.sync(&mut self.ui.tree);
                        model.load_covers(&self.lib, &mut self.covers, view, &mut self.ui.tree);
                        if view.jump_open {
                            self.ui.request_clean_refresh();
                        } else if self.prefs.covers.mode() != CoverMode::None {
                            self.ui.request_full_refresh();
                        } else {
                            self.ui.request_clean_refresh();
                        }
                    }
                }
            },
            _ => {}
        }
        Ok(Flow::Continue)
    }

    fn show_search(&mut self, refresh: Refresh) -> Result<(), Error> {
        let books_records = self.lib.books();
        // The index needs titles and authors only: the thumbnails are read
        // for the few results the screen draws, not for the whole library.
        let mut ui_books = Vec::with_capacity(books_records.len());
        let mut book_ids = Vec::with_capacity(books_records.len());
        for rec in books_records {
            ui_books.push(crate::shelf::screen_book_plain(rec));
            book_ids.push(rec.id.clone());
        }
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let keyboard = KeyboardState::new();
        let index = ui_search::SearchIndex::build(&ui_books);
        let mut tree = Tree::new();
        let (_root, search_ui) = ui_search::build_search_screen(
            &mut tree,
            &self.styles,
            &ui_books,
            &mut |i| {
                self.lib
                    .books()
                    .get(i)
                    .and_then(|rec| self.covers.get(&self.lib, rec))
            },
            &index,
            &keyboard,
            &status,
        );
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::Search {
            view: search_ui,
            books: ui_books,
            book_ids,
        };
        self.render_ui()
    }

    fn on_search_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let Screen::Search {
            view,
            books,
            book_ids,
        } = &mut self.screen
        else {
            return Ok(Flow::Continue);
        };
        let UiEvent::Action { id, node, .. } = ev else {
            return Ok(Flow::Continue);
        };
        match view.handle_action(id) {
            ui_search::SearchOutcome::Back => {
                // 收起键盘后全屏刷新一次 (GC16)
                self.show_library(Refresh::Full)?;
            }
            ui_search::SearchOutcome::OpenBook(orig_idx) => {
                if let Some(id) = book_ids.get(orig_idx) {
                    let id = id.clone();
                    self.open_from_library(&id, node)?;
                }
            }
            ui_search::SearchOutcome::Changed => {
                let facts = Facts::status(&self.battery);
                let status = facts.status_line();
                let mut tree = Tree::new();
                let (_root, new_view) = ui_search::build_search_screen(
                    &mut tree,
                    &self.styles,
                    books,
                    &mut |i| {
                        self.lib
                            .books()
                            .get(i)
                            .and_then(|rec| self.covers.get(&self.lib, rec))
                    },
                    &view.index,
                    &view.keyboard,
                    &status,
                );
                self.ui.set_tree_with(tree, Refresh::Auto);
                self.ui.layout();
                *view = new_view;
                self.render_ui()?;
            }
            ui_search::SearchOutcome::None => {}
        }
        Ok(Flow::Continue)
    }

    // --- Reader -----------------------------------------------------------

    /// Open a book from the library. The pressed card already shows the
    /// tap landed. With a snapshot the open is a single flash. Without one
    /// the book opens on a worker while the card stays inverted; the status
    /// bar of the screen says what is going on while it takes.
    fn open_from_library(&mut self, id: &str, card: NodeId) -> Result<(), Error> {
        self.open_book(id, Some(card))
    }

    /// Open the first book whose title contains `title` (case and
    /// diacritics ignored), as `--open` asks.
    fn open_by_title(&mut self, title: &str) -> Result<(), Error> {
        let want = aristotle_library::text::fold(title);
        let id = self
            .lib
            .books()
            .iter()
            .find(|b| aristotle_library::text::fold(&b.meta.title).contains(&want))
            .map(|b| b.id.clone());
        match id {
            Some(id) => self.open_book(&id, None),
            None => {
                eprintln!("aristotle: no book titled {title:?}");
                Ok(())
            }
        }
    }

    fn open_book(&mut self, id: &str, card: Option<NodeId>) -> Result<(), Error> {
        self.open_book_at(id, card, None)
    }

    /// [`App::open_book`], optionally at a saved annotation: a card of "My
    /// highlights and notes" opens its book on the highlight's page. A
    /// jump skips the snapshot, which shows the saved reading position
    /// instead, and never adopts a prewarm made for that position.
    fn open_book_at(
        &mut self,
        id: &str,
        card: Option<NodeId>,
        jump: Option<aristotle_library::index::Annotation>,
    ) -> Result<(), Error> {
        if self.pending.as_ref().is_some_and(|p| p.id == id) {
            return Ok(());
        }
        self.cancel_pending();
        let Some(rec) = self.lib.book(id).cloned() else {
            return Ok(());
        };
        self.sync_before_open(id);
        // The fonts are registered before the layout: with many font files
        // on the device that takes a while, and the status bar says so.
        self.register_fonts_for_crengine()?;
        let t0 = Instant::now();
        let style = self.page_style();
        let mut src = match Source::new(&self.lib, &rec, &self.mobi_cache) {
            Ok(mut s) => {
                s.secs_per_page = self.speed.secs_per_page();
                s.night = self.prefs.night;
                s
            }
            Err(e) => {
                eprintln!("aristotle: {}: {e:#}", rec.meta.title);
                return Ok(());
            }
        };
        src.jump = jump;
        let jumping = src.jump.is_some();
        if !jumping {
            if let Some(snap) = open::find_snapshot(&src, &style) {
                if let aristotle_render::handoff::Action::CancelAndBoost(job) =
                    self.prewarm.on_open(&rec.id, &style)
                {
                    job.worker.cancel();
                    crate::pagecache::restore_priority(job.worker.tid());
                }
                return self.open_from_snapshot(src, style, snap, t0);
            }
        }

        let (worker, adopted_prewarm) = match self.prewarm.on_open(&rec.id, &style) {
            aristotle_render::handoff::Action::Adopt(job) if !jumping => {
                crate::pagecache::restore_priority(job.worker.tid());
                perf!(
                    "open: \"{}\": no snapshot, adopting in-progress prewarm",
                    rec.meta.title
                );
                (job.worker, true)
            }
            aristotle_render::handoff::Action::Adopt(job)
            | aristotle_render::handoff::Action::CancelAndBoost(job) => {
                job.worker.cancel();
                crate::pagecache::restore_priority(job.worker.tid());
                perf!(
                    "open: \"{}\": no snapshot, opening in the background (prewarm cancelled)",
                    rec.meta.title
                );
                (open::spawn_with(src, style, false), false)
            }
            aristotle_render::handoff::Action::Normal => {
                perf!(
                    "open: \"{}\": no snapshot, opening in the background",
                    rec.meta.title
                );
                (open::spawn_with(src, style, false), false)
            }
        };

        // Nothing to show before the layout: the tapped card stays inverted
        // as the acknowledgement, the book opens on a worker, and the
        // library keeps taking input (another card, a filter) meanwhile.
        if let Some(card) = card {
            self.ui.tree.set_inverted(card, true);
        }
        self.pending = Some(Box::new(Pending {
            id: id.to_string(),
            title: rec.meta.title.clone(),
            reports_progress: reports_progress(rec.format),
            worker,
            card,
            tapped: t0,
            adopted_prewarm,
        }));
        // The status bar of the screen the book was opened from says what
        // is going on, once the layout takes long enough to be worth it.
        self.loading_progress.start(Instant::now());
        Ok(())
    }

    /// Forget a book opening without a snapshot (the reader tapped
    /// something else): its worker drops to idle priority, its card is
    /// released.
    fn cancel_pending(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        self.release_card(p.card);
        perf!(
            "open: \"{}\" given up {:.0} ms after the tap",
            p.title,
            ms(p.tapped.elapsed())
        );
    }

    /// The library card of a book no longer opening: not inverted, and the
    /// status bar says nothing about it any more.
    fn release_card(&mut self, card: Option<NodeId>) {
        if let Some(card) = card {
            self.ui.tree.set_inverted(card, false);
        }
        self.loading_progress.reset();
        self.set_loading(LoadingSource::Opening, None);
    }

    /// Show a book opened without a snapshot: one mode-8 flash to its page.
    fn finish_pending(&mut self, result: Result<Prepared, Error>) -> Result<(), Error> {
        let Some(p) = self.pending.take() else {
            return Ok(());
        };
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(e) => {
                eprintln!("aristotle: {}: {e:#}", p.title);
                self.release_card(p.card);
                return self.render_ui();
            }
        };
        let times = prepared.times;
        // The book is open: the status bar says nothing about it any more.
        self.loading_progress.reset();
        self.set_loading(LoadingSource::Opening, None);
        let mut session = Session::from_prepared(prepared)?;
        session.set_speed(self.speed.secs_per_page());
        let page = session.page(session.current)?;
        let light = self.light_level();
        let settings = self.prefs.menu_settings(light);
        let (tree, menu, page_node, nav, progress) =
            reader_tree(&self.styles, &session.title, &settings, page);
        // A screen switch: the stock UI's flashing full refresh (mode 8).
        self.ui.set_tree_with(tree, Refresh::Full);
        self.ui.gesture_config_mut().long_press_us = 500_000;
        self.render_ui()?;
        let prewarm_note = if p.adopted_prewarm {
            "adopted prewarm; "
        } else {
            ""
        };
        perf!(
            "open: \"{}\": {}book {:.0} ms, position {:.0} ms, first page {:.0} ms, submitted {:.0} ms after the tap; page numbers {}; no snapshot; waited for lock {:.0} ms",
            session.title,
            prewarm_note,
            ms(times.open),
            ms(times.place),
            ms(times.render),
            ms(p.tapped.elapsed()),
            if times.pages_cached { "from cache" } else { "being computed in the background" },
            ms(times.lock_wait)
        );
        self.lib
            .mark_opened(&p.id, aristotle_library::library::now());
        self.debounce.changed(Instant::now());
        let gestures = TurnGestures::with_density(self.ui.bounds(), self.ui.profile().density);
        let old = std::mem::replace(&mut self.screen, Screen::Opening);
        self.enter_reader(session, menu, page_node, nav, progress, settings, gestures);
        drop(old);
        if let Screen::Reader(r) = &mut self.screen {
            let (pn, pp) = r.session.prerender();
            perf!(
                "open: prerender next {:.0} ms, previous {:.0} ms",
                ms(pn),
                ms(pp)
            );
        }
        Ok(())
    }

    /// Show the page the book was left at from its snapshot, and open the
    /// book itself on a worker thread (see open.rs).
    fn open_from_snapshot(
        &mut self,
        mut src: Source,
        style: Style,
        mut snap: snapshot::Loaded,
        t0: Instant,
    ) -> Result<(), Error> {
        let light = self.light_level();
        let settings = self.prefs.menu_settings(light);
        let page = snap.page.clone();
        let (tree, menu, page_node, nav, progress) =
            reader_tree(&self.styles, &src.title, &settings, page.clone());
        // A screen switch: the stock UI's flashing full refresh (mode 8).
        self.ui.set_tree_with(tree, Refresh::Full);
        self.ui.gesture_config_mut().long_press_us = 500_000;
        self.render_ui()?;
        perf!(
            "open: \"{}\": snapshot submitted after {:.0} ms (read {:.0} ms, {} KB; decode {:.0} ms); opening the book in the background",
            src.title,
            ms(t0.elapsed()),
            ms(snap.read),
            snap.bytes / 1024,
            ms(snap.decode)
        );
        let (title, id, stamp) = (src.title.clone(), src.id.clone(), src.stamp);
        // Open exactly where the snapshot is, not where its locator maps.
        src.exact = Some(snap.info.location);
        let worker = open::spawn(src, style.clone());
        let t = Instant::now();
        let next = snap.take_next().map(|(i, p)| (i, Rc::new(p)));
        if next.is_some() {
            perf!(
                "open: snapshot's next page decoded in {:.0} ms",
                ms(t.elapsed())
            );
        }
        self.lib.mark_opened(&id, aristotle_library::library::now());
        self.debounce.changed(Instant::now());
        self.screen = Screen::Opening;
        self.formatting_progress.start(Instant::now());
        self.opening = Some(Box::new(Opening {
            title,
            id,
            stamp,
            style,
            worker,
            tapped: t0,
            current: (snap.info, page),
            next,
            turned: false,
            menu_open: false,
            menu,
            page_node,
            nav,
            progress,
            settings,
            gestures: TurnGestures::with_density(self.ui.bounds(), self.ui.profile().density),
        }));
        Ok(())
    }

    /// Input while the book opens. Turns between the snapshot's two pages
    /// and the menu work at once; anything else waits for the book, then
    /// goes the normal way.
    fn on_opening_touch(&mut self, ev: TouchEvent) -> Result<Flow, Error> {
        let Some(o) = self.opening.as_mut() else {
            return Ok(Flow::Continue);
        };
        if o.menu_open {
            for e in self.ui.handle_touch(ev) {
                if self.on_ui_event(e)? == Flow::Quit {
                    return Ok(Flow::Quit);
                }
            }
            return Ok(Flow::Continue);
        }
        for intent in o.gestures.feed(ev) {
            if self.opening_intent(intent)? {
                continue;
            }
            let before = self.panel.full_updates();
            let stale = self.finish_opening(true)?;
            self.on_intent(intent)?;
            self.redraw_if_stale(stale, before)?;
        }
        Ok(Flow::Continue)
    }

    /// Handle `intent` from the snapshot alone; false if it needs the book.
    fn opening_intent(&mut self, intent: Intent) -> Result<bool, Error> {
        let Some(o) = self.opening.as_mut() else {
            return Ok(false);
        };
        match intent {
            Intent::Turn { dir, at_us } => {
                let target = match (dir, o.turned) {
                    (Turn::Forward, false) => o.next.as_ref(),
                    (Turn::Back, true) => Some(&o.current),
                    _ => None,
                };
                let Some((_, page)) = target.cloned() else {
                    return Ok(false);
                };
                let waited = self.panel.show_page(&page, false, "turn")?;
                let tap_to_submit = monotonic_us().saturating_sub(at_us) as f64 / 1000.0;
                o.turned = dir == Turn::Forward;
                if let Some(w) = self.ui.tree.widget_mut::<PageView>(o.page_node) {
                    w.page = page;
                }
                self.ui.tree.invalidate(o.page_node, Refresh::Auto);
                // The panel already shows the page; this only updates the UI's copy.
                self.ui.render();
                perf!(
                    "turn {:?}: tap->submit {:.0} ms (a snapshot page, the book still opening; waited for the panel {:.0} ms)",
                    dir,
                    tap_to_submit,
                    ms(waited)
                );
                Ok(true)
            }
            // Selecting, footnotes and the bookmark need the book: while it
            // opens they do nothing (no wait, nothing on the panel).
            Intent::Hold { .. } | Intent::Drag(_) | Intent::Release(_) | Intent::Tap(_) => Ok(true),
            Intent::OpenMenu => {
                let light = self.light_level();
                let night = self.prefs.night;
                let Some(o) = self.opening.as_mut() else {
                    return Ok(false);
                };
                o.menu_open = true;
                o.settings.light = light;
                o.settings.night = night;
                let (info, page) = o.shown().clone();
                fit_menu(
                    &mut self.ui,
                    &self.styles,
                    &o.title,
                    &o.settings,
                    &page,
                    &mut o.menu,
                    &mut o.page_node,
                    &mut o.nav,
                    &mut o.progress,
                );
                self.ui.tree.set_text(o.menu.chapter_label, &info.chapter);
                self.ui.tree.set_text(o.menu.page_label, &info.label);
                self.ui.tree.set_progress(o.menu.progress, info.fraction);
                if let Some(w) = self.ui.tree.widget_mut::<Slider>(o.menu.light) {
                    w.value = light;
                }
                self.ui.tree.invalidate(o.menu.light, Refresh::Auto);
                self.ui.reset_gestures();
                self.ui.layout();
                let parts = menu_parts(&self.ui, &o.menu);
                self.ui.render();
                self.panel.present_overlay(&self.ui, &parts);
                Ok(true)
            }
        }
    }

    /// The menu while the book opens: closing it and going back to the
    /// library need only the snapshot; everything else waits for the book.
    fn on_opening_ui_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let Some(o) = self.opening.as_mut() else {
            return Ok(Flow::Continue);
        };
        if !o.menu_open {
            return Ok(Flow::Continue);
        }
        match ev {
            UiEvent::Tap(_) => {
                o.menu_open = false;
                o.gestures.reset();
                self.ui.tree.set_hidden(o.menu.root, true);
                self.ui.tree.set_hidden(o.nav, true);
                self.ui.render();
                let page = o.shown().1.clone();
                self.panel.show_page(&page, false, "menu closed")?;
            }
            UiEvent::Action {
                id: menu_action::BACK,
                ..
            } => self.abandon_opening()?,
            UiEvent::Action { id, .. } => {
                let before = self.panel.full_updates();
                let stale = self.finish_opening(true)?;
                if id >= overlay::FIRST_ACTION {
                    self.on_overlay_event(ev)?;
                } else {
                    self.on_menu_event(ev)?;
                }
                self.redraw_if_stale(stale, before)?;
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// After an action that needed the book: if the adopted page differs
    /// from what the panel shows and the action did not redraw the whole
    /// screen itself (a turn at the first or last page, a light change),
    /// one full mode-9 update now.
    fn redraw_if_stale(&mut self, stale: bool, full_updates_before: u64) -> Result<(), Error> {
        if !stale || self.panel.full_updates() != full_updates_before {
            return Ok(());
        }
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        self.ui.tree.invalidate(r.page_node, Refresh::Auto);
        if r.menu_open {
            self.render_ui_with(Content::Page)
        } else {
            let page = r.session.page(r.session.current)?;
            self.panel.show_page(&page, false, "corrected page")?;
            self.ui.render();
            Ok(())
        }
    }

    /// Keep the page shown from the snapshot as the progress when the book
    /// will not be adopted (back to the library, or the open failed).
    fn keep_shown_progress(&mut self, o: &Opening) {
        if !o.turned {
            return;
        }
        let (info, page) = o.shown().clone();
        let now = aristotle_library::library::now();
        let progress = progress::progress_for(info.locator.clone(), info.fraction, now);
        self.lib.set_progress(&o.id, progress);
        if info.last {
            self.lib.set_status(&o.id, Status::Finished);
        }
        let key = snapshot::Key::new(
            &o.id,
            o.stamp,
            &o.style,
            info.locator.clone(),
            self.prefs.night,
        );
        self.snapshots.save(key, (info, page), None);
    }

    /// Back to the library before the book finished opening: drop the
    /// worker and keep the page shown as the progress (and its snapshot).
    fn abandon_opening(&mut self) -> Result<(), Error> {
        let Some(o) = self.opening.take() else {
            return Ok(());
        };
        self.formatting_progress.reset();
        self.keep_shown_progress(&o);
        perf!(
            "open: \"{}\": back to the library {:.0} ms after the tap, before the book was ready",
            o.title,
            ms(o.tapped.elapsed())
        );
        // Dropping the worker cancels it between its steps, at idle priority.
        drop(o);
        self.show_library(Refresh::Full)?;
        self.snapshots.forget();
        self.saver.save(&self.lib);
        self.debounce.saved();
        Ok(())
    }

    /// Wait for the book being opened and adopt it. With `deferred`, a
    /// page that differs from the snapshot is not sent: the caller redraws
    /// next. Returns whether the panel then still shows the snapshot's page
    /// where the book's differs.
    fn finish_opening(&mut self, deferred: bool) -> Result<bool, Error> {
        let Some(o) = self.opening.as_ref() else {
            return Ok(false);
        };
        let t = Instant::now();
        let result = o.worker.wait();
        perf!("open: waited {:.0} ms for the book", ms(t.elapsed()));
        self.adopt(result, deferred)
    }

    /// Take over the session the worker made, and make sure the panel shows
    /// its page: no update if the snapshot showed exactly that page, else
    /// one full mode-9 update (none with `deferred`; see
    /// [`App::finish_opening`]).
    fn adopt(&mut self, result: Result<Prepared, Error>, deferred: bool) -> Result<bool, Error> {
        let Some(o) = self.opening.take() else {
            return Ok(false);
        };
        let prepared = match result {
            Ok(p) => p,
            Err(e) => {
                eprintln!("aristotle: {}: {e:#}", o.title);
                self.keep_shown_progress(&o);
                self.rebuild_library_at(Refresh::Full, 0, deferred)?;
                self.save_after_failed_open();
                return Ok(false);
            }
        };
        let t = Instant::now();
        let times = prepared.times;
        let (at, right) = open::reconcile(
            o.current.0.location,
            o.next.as_ref().map(|(i, _)| i.location),
            o.turned,
            prepared.current,
            prepared.next(),
        );
        let mut session = Session::from_prepared(prepared)?;
        session.set_speed(self.speed.secs_per_page());
        session.current = at;
        let mut page = session.page(at)?;
        let verdict = if right {
            open::compare(&o.shown().1, &page, &session.style)
        } else {
            open::Verdict::Page
        };
        if verdict == open::Verdict::Chrome {
            // Same text, but the header or footer moved on since the
            // snapshot was saved (page numbers that were not known then, a
            // new reading speed). The page on the panel stays until the
            // next turn: the open remains one update.
            page = o.shown().1.clone();
            session.keep_page(at, page.clone());
        }
        perf!(
            "open: \"{}\": book ready {:.0} ms after the tap (worker: book {:.0} ms, position {:.0} ms, first page {:.0} ms, neighbours {:.0} ms; adopted in {:.0} ms; waited for lock {:.0} ms); snapshot {:?}; page numbers {}",
            o.title,
            ms(o.tapped.elapsed()),
            ms(times.open),
            ms(times.place),
            ms(times.render),
            ms(times.neighbours),
            ms(t.elapsed()),
            ms(times.lock_wait),
            verdict,
            if times.pages_cached { "from cache" } else { "being computed in the background" }
        );
        let same = verdict != open::Verdict::Page;
        if same && !o.turned {
            // The snapshot on disk is exactly this page: don't write it back.
            let key = snapshot::Key::new(
                &o.id,
                o.stamp,
                &o.style,
                o.current.0.locator.clone(),
                self.prefs.night,
            );
            let next = o.next.as_ref().map(|(i, _)| i.location);
            self.snapshots.already_saved(key, page.clone(), next);
        }
        if o.turned || !right {
            let now = aristotle_library::library::now();
            self.lib.set_progress(&session.id, session.progress(now));
            self.debounce.changed(Instant::now());
        }
        let o = *o;
        let menu_open = o.menu_open;
        let page_node = o.page_node;
        if let Some(w) = self.ui.tree.widget_mut::<PageView>(page_node) {
            w.page = page.clone();
        }
        self.formatting_progress.reset();
        self.enter_reader(
            session, o.menu, page_node, o.nav, o.progress, o.settings, o.gestures,
        );
        if let Screen::Reader(r) = &mut self.screen {
            if menu_open {
                // The labels go out in the same frame as the page, if any.
                r.menu_open = true;
                update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
            }
        }
        match (same, deferred, menu_open) {
            (_, true, _) => {}
            (true, false, true) => self.render_ui()?,
            // The UI's copy of the screen already has these pixels: no
            // update at all.
            (true, false, false) => {}
            // Never a partial fix-up: one full mode-9 update, like a page
            // turn (behind an open menu, like a chapter jump).
            (false, false, true) => {
                self.ui.tree.invalidate(page_node, Refresh::Auto);
                self.render_ui_with(Content::Page)?;
            }
            (false, false, false) => {
                self.panel.show_page(&page, false, "corrected page")?;
                self.ui.tree.invalidate(page_node, Refresh::Auto);
                // The panel already shows the page; this only updates the UI's copy.
                self.ui.render();
            }
        }
        if let Screen::Reader(r) = &mut self.screen {
            r.session.prerender();
        }
        Ok(!same && deferred)
    }

    /// After a book failed to open: the index (with the progress the reader
    /// may have turned to) goes to disk, unless drive mode means the
    /// userstore is on its way out (USB-001 review 3).
    fn save_after_failed_open(&self) {
        if self.usb_mode == UsbMode::DriveMode {
            eprintln!(
                "aristotle: USB drive mode active; not saving the library index after a failed open"
            );
        } else {
            self.saver.save(&self.lib);
        }
    }

    fn enter_reader(
        &mut self,
        session: Session,
        menu: Menu,
        page_node: NodeId,
        nav: NodeId,
        progress: ProgressNodes,
        settings: Settings,
        gestures: TurnGestures,
    ) {
        self.screen = Screen::Reader(Box::new(Reading {
            session,
            menu,
            page_node,
            nav,
            progress,
            menu_open: false,
            gestures,
            settings,
            overlay: Overlay::None,
            nodes: OverlayNodes::default(),
            turns_since_flash: 0,
        }));
        // A new book: no layout of the old one is wanted any more.
        self.restyler.reset(settings);
        // The book is laid out with the typography it was opened with: a
        // change back to it needs no layout.
        if let Screen::Reader(r) = &self.screen {
            let style = r.session.style.clone();
            self.restyler.set_applied(&style);
        }
        self.speed.page_shown(Instant::now());
        self.stats.finish_page(Instant::now());
        self.stats_page_shown();
        self.page_changed();
        self.sync_book_opened();
        self.readest_book_opened();
    }

    /// The open book's hash and the page on the panel, for the statistics.
    fn stats_page(&self) -> Option<(String, u32, u32)> {
        let Screen::Reader(r) = &self.screen else {
            return None;
        };
        let (page, total) = r.session.page_number()?;
        Some((r.session.id.clone(), page, total))
    }

    /// Start timing the page on the panel, if its number is known.
    fn stats_page_shown(&mut self) {
        let Some((hash, page, total)) = self.stats_page() else {
            return;
        };
        self.stats.page_shown(
            &hash,
            page,
            total,
            Instant::now(),
            aristotle_library::library::now(),
        );
    }

    /// Store the current page (and the next, if rendered) for the next
    /// open, with the progress it belongs to; see snapshot.rs. Call before
    /// saving the index.
    fn save_snapshot(&mut self) {
        if let Some((key, current, next)) = self.snapshot_parts() {
            self.snapshots.save(key, current, next);
        }
    }

    /// What [`App::save_snapshot`] stores, without copying any pixels yet;
    /// sets the progress it belongs to.
    #[allow(clippy::type_complexity)]
    fn snapshot_parts(
        &mut self,
    ) -> Option<(
        snapshot::Key,
        (snapshot::PageInfo, Rc<Page>),
        Option<(snapshot::PageInfo, Rc<Page>)>,
    )> {
        let Screen::Reader(r) = &mut self.screen else {
            return None;
        };
        let s = &mut r.session;
        let progress = s.progress(aristotle_library::library::now());
        // The snapshot is only shown if the index stores this locator.
        let stored = self.lib.book(&s.id).and_then(|b| b.progress.as_ref());
        if stored.map(|p| &p.locator) != Some(&progress.locator) {
            self.lib.set_progress(&s.id, progress.clone());
        }
        let key = snapshot::Key::new(
            &s.id,
            s.stamp(),
            &s.style,
            progress.locator,
            self.prefs.night,
        );
        let here = s.current;
        let current = s.page(here).ok()?;
        let next = s
            .known_next(here)
            .and_then(|l| s.rendered(l).map(|p| (open::page_info(s, l), p)));
        let info = open::page_info(s, here);
        Some((key, (info, current), next))
    }

    /// The reader screen built again around the current page, menu and
    /// overlays closed after the screensaver.
    fn reader_after_sleep(&mut self, refresh: Refresh) -> Result<(), Error> {
        let settings = self.prefs.menu_settings(self.light_level());
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.settings = settings;
        r.menu_open = false;
        r.overlay = Overlay::None;
        r.gestures.reset();
        self.rebuild_reader(refresh)?;
        self.render_ui()?;
        self.stats_page_shown();
        Ok(())
    }

    fn on_intent(&mut self, intent: Intent) -> Result<(), Error> {
        // A turn queues behind a layout; everything else needs the book,
        // which a partial layout or a catch-up may have.
        if !matches!(intent, Intent::Turn { .. }) {
            self.settle_restyle()?;
        }
        match intent {
            Intent::Turn { dir, at_us } => self.turn(dir, at_us),
            Intent::OpenMenu => self.open_menu(),
            Intent::Hold { at } => self.hold(at),
            Intent::Drag(p) => self.drag(p),
            Intent::Release(p) => self.release(p),
            Intent::Tap(p) => self.quiet_tap(p),
        }
    }

    /// Fire a long press on the page while the finger rests.
    fn tick_reading(&mut self, now_us: u64) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        for i in r.gestures.tick(now_us) {
            self.on_intent(i)?;
        }
        Ok(())
    }

    /// A page turn: show the (normally prerendered) page with one full
    /// update, then prepare the next ones while the panel works.
    fn turn(&mut self, dir: Turn, at_us: u64) -> Result<(), Error> {
        // While the book is laid out again, the engine is busy with the
        // layout: the turn waits for it instead of blocking the reader on
        // the engine lock.
        if self.restyler.running() {
            self.restyler.queue_turn(dir, at_us);
            return Ok(());
        }
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        if r.session.degraded {
            self.restyler.user_action(Instant::now());
        }
        let Some(target) = r.session.neighbor(r.session.current, dir) else {
            if dir == Turn::Forward && !r.session.degraded {
                self.lib.set_status(&r.session.id, Status::Finished);
                self.debounce.changed(Instant::now());
            }
            return Ok(());
        };
        let interval = self.prefs.refresh_pages();
        r.turns_since_flash += 1;
        let flash = r.turns_since_flash >= interval;
        if flash {
            r.turns_since_flash = 0;
        }
        let s = &mut r.session;
        let t = Instant::now();
        let ready = s.is_rendered(target);
        let page = s.page(target)?;
        let t_render = t.elapsed();
        let waited = self.panel.show_page(&page, flash, "turn")?;
        let tap_to_submit = monotonic_us().saturating_sub(at_us) as f64 / 1000.0;
        s.current = target;
        if dir == Turn::Forward {
            self.speed.turned_forward(Instant::now());
        } else {
            self.speed.page_shown(Instant::now());
        }
        s.set_speed(self.speed.secs_per_page());
        if let Some((hash, page, total)) = s
            .page_number()
            .map(|(page, total)| (s.id.clone(), page, total))
        {
            self.stats.record_page(
                &hash,
                page,
                total,
                Instant::now(),
                aristotle_library::library::now(),
            );
        }
        let (pn, pp) = s.prerender();
        let t = Instant::now();
        if let Some(w) = self.ui.tree.widget_mut::<PageView>(r.page_node) {
            w.page = page;
        }
        self.ui.tree.invalidate(r.page_node, Refresh::Auto);
        // The panel already shows the page; this only updates the UI's copy.
        self.ui.render();
        let t_sync = t.elapsed();
        perf!(
            "turn {:?}: tap->submit {:.0} ms (page {}, {:.0} ms; waited for the panel {:.0} ms); prerender next {:.0} ms, previous {:.0} ms; ui copy {:.0} ms",
            dir,
            tap_to_submit,
            if ready { "prerendered" } else { "rendered now" },
            ms(t_render),
            ms(waited),
            ms(pn),
            ms(pp),
            ms(t_sync)
        );
        if !s.degraded {
            let now = aristotle_library::library::now();
            self.lib.set_progress(&s.id, s.progress(now));
            if s.at_end() {
                self.lib.set_status(&s.id, Status::Finished);
            }
            self.debounce.changed(Instant::now());
        }
        self.page_changed();
        Ok(())
    }

    /// Show the last manual sync result in the reader menu, if it is open.
    ///
    /// The line is one of the menu's own texts (localized in `readest.rs`); it
    /// repaints only the menu bars, like every other change to them.
    pub(crate) fn refresh_reader_sync_status(&mut self) -> Result<(), Error> {
        if self.asleep.is_some() {
            return Ok(());
        }
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let Some(status) = r.menu.sync_status else {
            return Ok(());
        };
        let text = self.readest.manual_message.clone().unwrap_or_default();
        self.ui.tree.set_text(status, &text);
        self.ui.tree.set_hidden(status, text.is_empty());
        if !r.menu_open {
            return Ok(());
        }
        self.ui.layout();
        let parts = menu_parts(&self.ui, &r.menu);
        self.ui.render();
        self.panel.present_overlay(&self.ui, &parts);
        Ok(())
    }

    fn open_menu(&mut self) -> Result<(), Error> {
        let light = self.light_level();
        let night = self.prefs.night;
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.menu_open = true;
        r.settings.light = light;
        r.settings.night = night;
        let page = r.session.page(r.session.current)?;
        fit_menu(
            &mut self.ui,
            &self.styles,
            &r.session.title,
            &r.settings,
            &page,
            &mut r.menu,
            &mut r.page_node,
            &mut r.nav,
            &mut r.progress,
        );
        update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
        if let Some(w) = self.ui.tree.widget_mut::<Slider>(r.menu.light) {
            w.value = light;
        }
        if self.restyler.busy() && self.restyler.slow() {
            // The book is laid out again: the menu says so.
            self.ui.tree.set_hidden(r.menu.busy, false);
            self.restyler.set_mark(true);
        }
        self.ui.tree.invalidate(r.menu.light, Refresh::Auto);
        self.ui.reset_gestures();
        self.ui.layout();
        let parts = menu_parts(&self.ui, &r.menu);
        self.ui.render();
        self.panel.present_overlay(&self.ui, &parts);
        Ok(())
    }

    /// Close the menu: the page comes back with a full mode-9 update of the
    /// whole screen, which also clears what the menu's updates left behind.
    /// Whether a reader UI event needs the session's book, which a partial
    /// layout or a catch-up may have (see [`App::settle_restyle`]). Closing
    /// the menu and the light and typography controls do not: they wait for
    /// nothing, where waiting for a catch-up kept the menu open for about
    /// five seconds after a tap beside it (H-013).
    fn reader_event_needs_book(&self, ev: &UiEvent) -> bool {
        let Screen::Reader(r) = &self.screen else {
            return false;
        };
        if !r.menu_open || !r.overlay.is_none() {
            return true;
        }
        match ev {
            UiEvent::Tap(_) => false,
            UiEvent::Action { id, .. } => {
                !matches!(
                    *id,
                    menu_action::LIGHT
                        | menu_action::LIGHT_SET
                        | menu_action::LIGHT_DOWN
                        | menu_action::LIGHT_UP
                        | menu_action::SIZE_DOWN
                        | menu_action::SIZE_UP
                ) && !(menu_action::SPACING..menu_action::SPACING + 3).contains(id)
                    && !(menu_action::MARGINS..menu_action::MARGINS + 3).contains(id)
            }
            _ => true,
        }
    }

    fn close_menu(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.menu_open = false;
        r.gestures.reset();
        self.ui.tree.set_hidden(r.menu.root, true);
        self.ui.tree.set_hidden(r.nav, true);
        self.ui.render();
        // The page behind the menu is what the panel shows again; while a
        // layout has the book it is the one the page view holds.
        let page = match r.session.page(r.session.current) {
            Ok(page) => page,
            Err(_) if !r.session.has_book() => match self.ui.tree.widget::<PageView>(r.page_node) {
                Some(view) => view.page.clone(),
                None => return Ok(()),
            },
            Err(e) => return Err(e),
        };
        self.panel.show_page(&page, false, "menu closed")?;
        self.page_changed();
        Ok(())
    }

    fn on_menu_event(&mut self, ev: UiEvent) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        if !r.menu_open {
            return Ok(());
        }
        match ev {
            // A tap on the page between the bars closes the menu.
            UiEvent::Tap(_) => self.close_menu(),
            UiEvent::Action { id, node, at } => {
                let mut s = r.settings;
                match id {
                    menu_action::BACK => self.back_to_library(),
                    // The two cloud-sync actions of the open book (SYNC-111).
                    // The menu stays open so its one-line result is visible; a
                    // pulled position is offered once the menu closes.
                    menu_action::PULL_SYNC => self.readest_pull_book_now(),
                    menu_action::PUSH_SYNC => self.readest_push_book_now(),
                    menu_action::SEARCH => {
                        self.close_menu()?;
                        self.show_coming_soon(
                            Refresh::Full,
                            aristotle_i18n::tr(aristotle_i18n::Msg::CommonSearch),
                            SoonBack::Reader,
                        )
                    }
                    menu_action::BOOKMARK => {
                        self.toggle_bookmark()?;
                        if let Screen::Reader(r) = &mut self.screen {
                            let bookmarked = r.session.bookmarked(r.session.current).is_some();
                            if let Some(w) =
                                self.ui.tree.widget_mut::<BookmarkRibbon>(r.menu.bookmark)
                            {
                                w.bookmarked = bookmarked;
                            }
                            self.ui.tree.invalidate(r.menu.bookmark, Refresh::Auto);
                            self.ui.render();
                            let parts = menu_parts(&self.ui, &r.menu);
                            self.panel.present_overlay(&self.ui, &parts);
                        }
                        Ok(())
                    }
                    menu_action::CONTENTS => self.open_toc(),
                    menu_action::MARKS => self.open_marks(),
                    menu_action::GOTO => self.open_goto(),
                    menu_action::NIGHT => {
                        let on = !self.prefs.night;
                        s.night = on;
                        r.settings = s;
                        if let Some(t) = r.menu.night_toggle {
                            if let Some(w) = self.ui.tree.widget_mut::<Toggle>(t) {
                                w.on = on;
                            }
                            self.ui.tree.invalidate(t, Refresh::Auto);
                        }
                        self.set_night(on);
                        self.render_ui()
                    }
                    menu_action::LIGHT
                    | menu_action::LIGHT_SET
                    | menu_action::LIGHT_DOWN
                    | menu_action::LIGHT_UP => {
                        let cur = self.light_wanted.get();
                        let step = match id {
                            menu_action::LIGHT => {
                                if cur > 0 {
                                    0
                                } else {
                                    LIGHT_STEPS / 2
                                }
                            }
                            menu_action::LIGHT_SET => {
                                Slider::step_at(self.ui.tree.rect(node), at.x, LIGHT_STEPS)
                            }
                            menu_action::LIGHT_DOWN => cur.saturating_sub(1),
                            _ => (cur + 1).min(LIGHT_STEPS),
                        };
                        s.light = step as f32 / LIGHT_STEPS as f32;
                        r.settings = s;
                        r.menu.update_light(&mut self.ui.tree, s.light);
                        self.set_light_step(step);
                        self.ui.render();
                        let Screen::Reader(r) = &self.screen else {
                            return Ok(());
                        };
                        let parts = menu_parts(&self.ui, &r.menu);
                        self.panel.present_overlay(&self.ui, &parts);
                        Ok(())
                    }
                    menu_action::PREV_CHAPTER | menu_action::NEXT_CHAPTER => {
                        let forward = id == menu_action::NEXT_CHAPTER;
                        if let Some(loc) = r.session.chapter(forward) {
                            self.jump(loc)?;
                        }
                        Ok(())
                    }
                    menu_action::TYPESET => self.open_typeset(),
                    menu_action::FONT_LIST => self.open_fonts(fonts::FontTarget::Latin),
                    menu_action::CJK_FONT_LIST => self.open_fonts(fonts::FontTarget::Cjk),
                    menu_action::SIZE_DOWN => {
                        s.size = s.size.saturating_sub(1);
                        self.request_restyle(s)
                    }
                    menu_action::SIZE_UP => {
                        s.size = (s.size + 1).min(prefs::FONT_PT.len() - 1);
                        self.request_restyle(s)
                    }
                    i if (menu_action::SPACING..menu_action::SPACING + 3).contains(&i) => {
                        s.spacing = (i - menu_action::SPACING) as usize;
                        self.request_restyle(s)
                    }
                    i if (menu_action::MARGINS..menu_action::MARGINS + 3).contains(&i) => {
                        s.margins = (i - menu_action::MARGINS) as usize;
                        self.request_restyle(s)
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    /// Show the page at `location` behind the open menu.
    fn jump(&mut self, location: usize) -> Result<(), Error> {
        if self.restyler.running() {
            return Ok(());
        }
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        if r.session.degraded {
            self.restyler.user_action(Instant::now());
        }
        r.session.current = location;
        let page = r.session.page(location)?;
        if let Some((hash, page, total)) = r
            .session
            .page_number()
            .map(|(page, total)| (r.session.id.clone(), page, total))
        {
            self.stats.record_page(
                &hash,
                page,
                total,
                Instant::now(),
                aristotle_library::library::now(),
            );
        }
        if let Some(w) = self.ui.tree.widget_mut::<PageView>(r.page_node) {
            w.page = page.clone();
        }
        self.ui.tree.invalidate(r.page_node, Refresh::Auto);
        fit_menu(
            &mut self.ui,
            &self.styles,
            &r.session.title,
            &r.settings,
            &page,
            &mut r.menu,
            &mut r.page_node,
            &mut r.nav,
            &mut r.progress,
        );
        update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
        let now = aristotle_library::library::now();
        self.lib
            .set_progress(&r.session.id, r.session.progress(now));
        self.debounce.changed(Instant::now());
        self.render_ui_with(Content::Page)?;
        if let Screen::Reader(r) = &mut self.screen {
            r.session.prerender();
        }
        Ok(())
    }

    /// Leaves the reader and brings back the screen the book was opened
    /// from: the home page, the library, or a shelf of it.
    fn back_to_library(&mut self) -> Result<(), Error> {
        self.stats.finish_page(Instant::now());
        if let Screen::Reader(r) = &self.screen {
            let now = aristotle_library::library::now();
            self.lib
                .set_progress(&r.session.id, r.session.progress(now));
            // Leaving the book makes its manual sync results stale: they must
            // not appear over the next book (SYNC-111).
            self.readest.jobs.forget_book(&r.session.id);
        }
        self.readest.manual_message = None;
        // The book is closed: a layout of it is not wanted any more.
        self.restyler.reset(self.restyler.panel_typography());
        self.sync_push(true, false);
        self.readest_push(true, false);
        let snapshot = self.snapshot_parts();
        // The screen the book was opened from goes out first; copying the
        // pages and serialising the index wait until it is submitted.
        match self.section {
            ui_nav::Tab::Home => self.show_home(Refresh::Full)?,
            ui_nav::Tab::Library => self.show_library(Refresh::Full)?,
            ui_nav::Tab::Mine => self.show_mine(Refresh::Full)?,
        }
        if let Some((key, current, next)) = snapshot {
            self.snapshots.save(key, current, next);
        }
        // The closed book's page needn't stay in memory for the skip test.
        self.snapshots.forget();
        self.saver.save(&self.lib);
        self.speed.save();
        self.stats.flush();
        self.debounce.saved();
        Ok(())
    }

    /// Save progress, give back the light and the stock screensaver, and
    /// leave a white day-mode screen for the stock UI.
    fn leave(self) -> Result<(), Error> {
        let mut app = self;
        // A forward turn made while the book was opening counts.
        app.finish_opening(true)?;
        if let Screen::Reader(r) = &app.screen {
            let now = aristotle_library::library::now();
            app.lib.set_progress(&r.session.id, r.session.progress(now));
        }
        app.sync_push(true, true);
        app.readest_push(true, true);
        if let Some(step) = app.asleep.take() {
            if std::mem::take(&mut app.light_off_by_app) {
                app.apply_light_step(step);
            }
        }
        app.sleep_ctl.restore();
        // Only a known drive mode skips the flush: the userstore is on its
        // way out then. Every other exit saves, whatever a query of volumd
        // says (USB-001 review 3).
        if app.usb_mode == UsbMode::DriveMode {
            eprintln!("aristotle: quit: USB drive mode active; skipping the disk flush on leave");
        } else {
            app.save_snapshot();
            app.saver.save(&app.lib);
            app.speed.save();
            app.stats.finish_page(Instant::now());
            app.stats.flush();
            app.saver.flush();
            app.snapshots.flush();
        }
        if app.usb_mode != UsbMode::DriveMode {
            app.panel.clear()?;
        }
        perf!("quit: progress saved, screen cleared");
        Ok(())
    }
}

/// The reader screen: the page with the (hidden) menu and its row of
/// reading actions over it. Returns the tree, the menu, the page node,
/// the row, and the progress bar node.
fn reader_tree(
    styles: &Styles,
    title: &str,
    settings: &Settings,
    page: Rc<Page>,
) -> (Tree, Menu, NodeId, NodeId, ProgressNodes) {
    reader_tree_with(styles, title, settings, page, (0, 0), &mut |_| Vec::new())
}

/// [`reader_tree`] with the menu's edges moved by `extents` (see
/// [`fit_menu`]) and the layers `extra` adds between the page and the menu
/// (the selection, a popup; see reading.rs).
fn reader_tree_with(
    styles: &Styles,
    title: &str,
    settings: &Settings,
    page: Rc<Page>,
    extents: (i32, i32),
    extra: &mut dyn FnMut(&mut Tree) -> Vec<NodeId>,
) -> (Tree, Menu, NodeId, NodeId, ProgressNodes) {
    let mut t = Tree::new();
    let page_node = t.custom(PageView { page });
    let mut layers = vec![page_node];
    layers.extend(extra(&mut t));
    // The progress bar and the line of text beside it.
    let (progress_row, progress) = ui_reader::progress_row(&mut t, styles);
    let progress_padded = t.pad(Insets::new(0, 0, styles.density.px(24), 0), progress_row);
    let progress_aligned = t.align(0.5, 1.0, progress_padded);
    layers.push(progress_aligned);
    let spec = MenuSpec {
        title,
        // The reading font's row opens the list of fonts (fonts.rs).
        families: None,
        font_list: true,
        night: true,
        sync: true,
        top_extra: extents.0,
        bottom_extra: extents.1,
    };
    let menu = build_menu(&mut t, styles, &spec, settings);
    // The prototype has no second row of reading actions: 目录, 标注 and
    // 跳转 live in the bottom sheet. Keep an empty node for the reading
    // state that still hides it with the menu.
    let nav = t.add(Node::column(0).hidden(true), []);
    layers.push(menu.root);
    let root = t.add(Node::new(Kind::Stack), layers);
    t.set_root(root);
    (t, menu, page_node, nav, progress)
}

/// Show the menu over `page` with its edges in the white between two lines
/// of text, never through them: the bars hide whole lines. The top edge is
/// the bottom of the row of reading actions under the top bar. Rebuilds the
/// reader tree when the edges have to move (the menu's labels are then
/// fresh: set them after this).
#[allow(clippy::too_many_arguments)]
fn fit_menu(
    ui: &mut Ui,
    _styles: &Styles,
    _title: &str,
    _settings: &Settings,
    _page: &Rc<Page>,
    menu: &mut Menu,
    _page_node: &mut NodeId,
    _nav: &mut NodeId,
    _progress: &mut ProgressNodes,
) {
    // The prototype's menu has fixed edges and no dimming mask: the top bar
    // covers the running head, the sheet the last lines (ReaderMenu.dc.html).
    ui.tree.set_hidden(menu.root, false);
    ui.layout();
}

/// The opaque parts of the menu, for [`Panel::present_overlay`]: the top bar
/// and the bottom sheet.
fn menu_parts(ui: &Ui, menu: &Menu) -> [Rect; 2] {
    [ui.tree.rect(menu.top), ui.tree.rect(menu.sheet)]
}

fn update_menu_labels(ui: &mut Ui, r: &mut Reading, sync_status: Option<&str>) {
    if let Some(status) = r.menu.sync_status {
        let text = sync_status.unwrap_or("");
        ui.tree.set_text(status, text);
        ui.tree.set_hidden(status, text.is_empty());
    }
    let (chapter, page, fraction) = r.session.labels();
    ui.tree.set_text(r.menu.chapter_label, &chapter);
    ui.tree.set_text(r.menu.page_label, &page);
    ui.tree.set_progress(r.menu.progress, fraction);
    let bookmarked = r.session.bookmarked(r.session.current).is_some();
    if let Some(w) = ui.tree.widget_mut::<BookmarkRibbon>(r.menu.bookmark) {
        w.bookmarked = bookmarked;
    }
    ui.tree.invalidate(r.menu.bookmark, Refresh::Auto);
    if let Some(l) = r.menu.font_label {
        ui.tree.set_text(l, &r.session.style.font_family);
    }
    if let Some(l) = r.menu.cjk_font_label {
        ui.tree.set_text(l, &r.session.style.cjk_font_family);
    }
    if let Some(w) = ui.tree.widget_mut::<Steps>(r.menu.size_steps) {
        w.value = r.settings.size;
    }
}

/// A fresh, empty directory for one test, under `$TMPDIR` (the build
/// script points it at `$ARISTOTLE_CACHE/tmp`, not the RAM-backed /tmp).
#[cfg(test)]
pub(crate) fn testdir(name: &str) -> PathBuf {
    use std::sync::atomic::AtomicUsize;
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir()
        .join("aristotle-tests")
        .join(format!("{name}-{}-{n}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disconnected_scan_worker_leaves_a_retryable_report() {
        let dir = testdir("scan-disconnected");
        let root = dir.join("books");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        write_book(&root.join("existing.epub"));
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        lib.save().unwrap();
        let mut app = memory_app(&library, &root);
        let original = serde_json::to_value(app.lib.index()).unwrap();
        app.show_scan(
            Refresh::Full,
            ui_scan::ScanState::Scanning {
                path: root.display().to_string(),
                subdirs: true,
                found_count: 0,
                current_file: String::new(),
                progress: 0.0,
                status: String::new(),
                scanned_books: Vec::new(),
            },
        )
        .unwrap();
        let (tx, rx) = crate::wake::channel();
        app.scan_task = Some(ScanTask {
            confirmed_removed: Default::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
            purpose: ScanPurpose::Review {
                sources: Vec::new(),
            },
            ack: None,
            _handle: None,
        });
        drop(tx);
        app.poll_scan().unwrap();
        assert!(app.scan_task.is_none());
        assert!(matches!(
            app.screen,
            Screen::Scan {
                state: ui_scan::ScanState::Done { error: Some(_), .. },
                ..
            }
        ));
        assert_eq!(serde_json::to_value(app.lib.index()).unwrap(), original);
        assert!((0..app.ui.tree.len()).any(|n| {
            app.ui.tree.node(NodeId(n as u32)).action == Some(ui_scan::action::RESCAN)
        }));
    }

    fn wait_for_scan(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while app.scan_task.is_some() && Instant::now() < deadline {
            app.poll_scan().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.scan_task.is_none(), "the scan worker did not finish");
    }

    /// A start-up scan may refresh what the index knows, but a new local
    /// file, even a supported TXT, waits for the reader's review (UX-112).
    #[test]
    fn a_startup_scan_never_admits_a_new_local_file() {
        let dir = testdir("startup-no-admit");
        let root = dir.join("documents");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        write_book(&root.join("known.epub"));
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        lib.save().unwrap();
        drop(lib);
        std::fs::write(root.join("device-notes.txt"), b"# configuration\n").unwrap();
        let mut app = memory_app(&library, &root);
        app.scan(&root).unwrap();
        assert_eq!(app.lib.books().len(), 1, "the TXT is not imported");
        assert_eq!(app.lib.books()[0].file.path, "known.epub");
        // A restart cannot slip it in either.
        app.scan(&root).unwrap();
        assert_eq!(app.lib.books().len(), 1);
        app.leave().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The manual flow: scan stages, every box starts unchecked, and only the
    /// checked books are copied into the library folder and indexed. The
    /// unchecked originals do not move.
    #[test]
    fn a_manual_scan_reviews_and_imports_only_the_checked_books() {
        let dir = testdir("review-import");
        let root = dir.join("documents");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        for (name, text) in [
            ("Lalka.txt", "Lalka.\n"),
            ("Dziady.txt", "Dziady.\n"),
            ("Treny.txt", "Treny.\n"),
            ("Uczta.txt", "Uczta.\n"),
            ("Fedon.txt", "Fedon.\n"),
            ("Krol.txt", "Krol.\n"),
        ] {
            std::fs::write(root.join(name), text).unwrap();
        }
        let mut app = memory_app(&library, &root);
        app.scan_whole = true;
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Sources, None)
            .unwrap();
        app.start_scan().unwrap();
        wait_for_scan(&mut app);
        let Screen::Scan {
            state: ui_scan::ScanState::Review {
                books, selected, ..
            },
            ..
        } = &app.screen
        else {
            panic!("a manual scan must end on the review");
        };
        assert_eq!(books.len(), 6);
        assert!(
            selected.iter().all(|checked| !*checked),
            "unchecked by default"
        );
        assert_eq!(app.lib.books().len(), 0, "nothing is imported yet");
        let first = books.iter().position(|b| b.path == "Lalka.txt").unwrap();
        let last = books.iter().position(|b| b.path == "Krol.txt").unwrap();
        app.toggle_review(first).unwrap();
        app.toggle_review(last).unwrap();
        app.confirm_import().unwrap();
        wait_for_scan(&mut app);
        let Screen::Scan {
            state: ui_scan::ScanState::Imported {
                migrated, failed, ..
            },
            ..
        } = &app.screen
        else {
            panic!("the import must report");
        };
        assert_eq!((*migrated, *failed), (2, 0));
        let destination = app.library_destination();
        assert!(destination.join("Lalka.txt").exists());
        assert!(destination.join("Krol.txt").exists());
        assert!(!root.join("Lalka.txt").exists(), "checked originals moved");
        assert!(root.join("Dziady.txt").exists(), "unchecked books stay");
        assert_eq!(app.lib.books().len(), 2);
        assert!(app
            .lib
            .books()
            .iter()
            .all(|b| b.file.path.starts_with("墨阅书库/")));
        app.leave().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Navigating the picker joins the stored relative path with the scan
    /// root, never with the folder already shown, and a checkmark survives
    /// the next folder and the page.
    #[test]
    fn picker_navigation_joins_the_root_and_keeps_checkmarks() {
        let dir = testdir("nested-picker");
        let root = dir.join("store");
        std::fs::create_dir_all(root.join("documents/novels")).unwrap();
        let mut app = memory_app(&dir.join("library"), &root);
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Sources, None)
            .unwrap();
        let row = |app: &App, base, index| {
            (0..app.ui.tree.len())
                .map(|i| NodeId(i as u32))
                .find(|&node| app.ui.tree.node(node).action == Some(base + index))
        };
        let node = row(&app, ui_scan::action::SCOPE_ITEM, 0).expect("a folder row");
        app.on_scan_event(UiEvent::Action {
            id: ui_scan::action::SCOPE_ITEM,
            node,
            at: aristotle_ui::geom::Point::default(),
        })
        .unwrap();
        let Screen::Scan {
            state: ui_scan::ScanState::Sources { path, folders, .. },
            ..
        } = &app.screen
        else {
            panic!("the picker must show the folder");
        };
        assert_eq!(path, "documents", "the root is joined once");
        let novels = folders
            .iter()
            .position(|folder| folder.relative == "documents/novels")
            .expect("the nested folder is listed");
        app.on_scan_event(UiEvent::Action {
            id: ui_scan::action::SOURCE_ITEM + novels as u32,
            node: NodeId(0),
            at: aristotle_ui::geom::Point::default(),
        })
        .unwrap();
        assert_eq!(app.scan_sources, ["documents/novels"]);
        let Screen::Scan {
            state: ui_scan::ScanState::Sources { path, .. },
            ..
        } = &app.screen
        else {
            panic!("checking a box keeps the picker");
        };
        assert_eq!(path, "documents");
        app.leave().unwrap();
    }

    /// The destination picker can pick the store root explicitly, and using
    /// it returns to source selection instead of leaving the reader in
    /// destination navigation.
    #[test]
    fn the_destination_picker_accepts_the_root_and_returns_to_sources() {
        let dir = testdir("destination-root");
        let root = dir.join("store");
        std::fs::create_dir_all(root.join("documents")).unwrap();
        let mut app = memory_app(&dir.join("library"), &root);
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Destination, None)
            .unwrap();
        app.on_scan_event(UiEvent::Action {
            id: ui_scan::action::DEST_USE,
            node: NodeId(0),
            at: aristotle_ui::geom::Point::default(),
        })
        .unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        assert_eq!(
            app.prefs.library_destination.as_deref(),
            Some(canonical_root.as_path())
        );
        assert!(matches!(
            &app.screen,
            Screen::Scan {
                state: ui_scan::ScanState::Sources {
                    mode: ui_scan::PickerMode::Sources,
                    ..
                },
                ..
            }
        ));
        app.leave().unwrap();
    }

    /// A file the reader dropped straight into the library folder is still a
    /// new book: it is listed, starts unchecked, and gets its index entry
    /// only when confirmed. The copy is already in place (transfer's
    /// source==destination no-op).
    #[test]
    fn an_unindexed_file_in_the_destination_needs_its_own_confirmation() {
        let dir = testdir("dropped-in-destination");
        let root = dir.join("documents");
        let library = dir.join("library");
        let destination = root.join("墨阅书库");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("dropped.txt"), b"Dropped in.\n").unwrap();
        let mut app = memory_app(&library, &root);
        app.scan_whole = true;
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Sources, None)
            .unwrap();
        app.start_scan().unwrap();
        wait_for_scan(&mut app);
        let Screen::Scan {
            state: ui_scan::ScanState::Review {
                books, selected, ..
            },
            ..
        } = &app.screen
        else {
            panic!("the dropped file must be reviewed");
        };
        let index = books
            .iter()
            .position(|book| book.path == "墨阅书库/dropped.txt")
            .expect("the file is listed");
        assert!(books[index].id.is_none(), "it is new to the index");
        assert!(books[index].in_destination);
        assert!(books[index].selectable());
        assert!(selected.iter().all(|checked| !*checked));
        app.toggle_review(index).unwrap();
        app.confirm_import().unwrap();
        wait_for_scan(&mut app);
        let Screen::Scan {
            state: ui_scan::ScanState::Imported { migrated, .. },
            ..
        } = &app.screen
        else {
            panic!("the import must report");
        };
        assert_eq!(*migrated, 1);
        assert!(destination.join("dropped.txt").exists());
        assert_eq!(app.lib.books().len(), 1);
        assert_eq!(app.lib.books()[0].file.path, "墨阅书库/dropped.txt");
        app.leave().unwrap();
    }

    /// Cancelling a migration reaches a terminal screen, keeps every
    /// original and imports nothing.
    #[test]
    fn cancelling_an_import_keeps_the_originals_and_reports_it() {
        let dir = testdir("cancel-import");
        let root = dir.join("documents");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Lalka.txt"), b"Lalka.\n").unwrap();
        std::fs::write(root.join("Dziady.txt"), b"Dziady.\n").unwrap();
        let mut app = memory_app(&library, &root);
        app.scan_whole = true;
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Sources, None)
            .unwrap();
        app.start_scan().unwrap();
        wait_for_scan(&mut app);
        let count = match &app.screen {
            Screen::Scan {
                state: ui_scan::ScanState::Review { books, .. },
                ..
            } => books.len(),
            _ => 0,
        };
        assert!(count > 0, "the scan must reach the review");
        for i in 0..count {
            app.toggle_review(i).unwrap();
        }
        app.confirm_import().unwrap();
        app.cancel_scan().unwrap();
        wait_for_scan(&mut app);
        let Screen::Scan {
            state:
                ui_scan::ScanState::Imported {
                    cancelled,
                    migrated,
                    ..
                },
            ..
        } = &app.screen
        else {
            panic!("a cancelled import must reach its report");
        };
        assert!(*cancelled);
        assert_eq!(*migrated, 0);
        assert!(root.join("Lalka.txt").exists());
        assert!(root.join("Dziady.txt").exists());
        assert_eq!(app.lib.books().len(), 0);
        app.leave().unwrap();
    }

    #[test]
    fn idle_screens_have_no_clock_or_inapplicable_prewarm_timer() {
        let dir = testdir("idle-deadlines");
        let (root, library) = (dir.join("books"), dir.join("library"));
        // One book: the idle library with a shelf is the screen where prewarm
        // has work, so it wants the loop back at once. An empty library shows
        // the scan guide instead (LIB-004) and prewarm does not apply there.
        std::fs::create_dir_all(&root).unwrap();
        write_book(&root.join("idle.epub"));
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        lib.save().unwrap();
        drop(lib);
        let mut app = memory_app(&library, &root);
        app.prefs.set_sleep(None);
        let now = Instant::now() + prewarm::QUIET + Duration::from_secs(1);
        let now_us = monotonic_us();
        app.show_library(Refresh::Full).unwrap();
        assert_eq!(app.poll_timeout(now_us, now), Duration::ZERO);
        app.show_settings(Refresh::Full, 0).unwrap();
        assert_eq!(app.poll_timeout(now_us, now), Duration::MAX);
        let before = updates(&app).len();
        app.background().unwrap();
        assert_eq!(updates(&app).len(), before);
        app.open_network(catalogs::NetView::Sync, 0).unwrap();
        assert_eq!(app.poll_timeout(now_us, now), Duration::MAX);
        app.fall_asleep(true).unwrap();
        assert_eq!(app.poll_timeout(now_us, now), Duration::MAX);
        touch(&mut app, HalPhase::Down, 100, 100, now_us);
        assert!(app.asleep.is_some(), "touch must not cancel managed sleep");
    }

    #[test]
    fn a_finished_transfer_shows_a_full_bar() {
        let facts = Facts::status(&None);
        let mut state = transfer::state::State::new();
        state.add_received("qa103_upload.epub".into(), 4096);
        // The file that arrived leaves `current`: the page has to read the
        // stage, not a missing file, to show the bar whole.
        assert!(state.current.is_none());
        let model = wifi_model(&facts, None, &state, true, true);
        assert_eq!(model.phase, ui_wifi::Phase::Done);
        assert_eq!(model.progress, 1.0);
        assert_eq!(ui_wifi::percent_text(model.progress), "100%");
        // While a file is on its way the bar follows its bytes.
        state.set_current(transfer::state::InFlight {
            name: "b.epub".into(),
            received: 512,
            total: 1024,
            elapsed: Duration::from_secs(1),
        });
        let model = wifi_model(&facts, None, &state, true, true);
        assert_eq!(model.phase, ui_wifi::Phase::Receiving);
        assert!((model.progress - 0.5).abs() < 0.01);
    }

    #[test]
    fn the_bottom_bar_switches_the_section() {
        let dir = testdir("nav-sections");
        let mut app = memory_app(&dir.join("library"), &dir.join("books"));
        // Nothing to read yet: the app starts in the library (empty library scan guide).
        assert!(matches!(
            app.screen,
            Screen::Library { .. }
                | Screen::Scan {
                    state: ui_scan::ScanState::Empty,
                    ..
                }
        ));
        let event = |app: &App, action| {
            let node = (0..app.ui.tree.len())
                .map(|i| NodeId(i as u32))
                .find(|id| app.ui.tree.node(*id).action == Some(action))
                .expect("the bar is on the screen");
            UiEvent::Action {
                id: action,
                node,
                at: aristotle_ui::geom::Point { x: 200, y: 240 },
            }
        };
        let home = event(&app, ui_nav::action::TAB);
        assert!(app.on_ui_event(home).unwrap() == Flow::Continue);
        assert!(matches!(app.screen, Screen::Home { .. }));
        assert_eq!(app.section, ui_nav::Tab::Home);
        let mine = event(&app, ui_nav::action::TAB + 2);
        assert!(app.on_ui_event(mine).unwrap() == Flow::Continue);
        assert!(matches!(app.screen, Screen::Mine));
        assert_eq!(app.section, ui_nav::Tab::Mine);
        let library = event(&app, ui_nav::action::TAB + 1);
        assert!(app.on_ui_event(library).unwrap() == Flow::Continue);
        assert!(matches!(
            app.screen,
            Screen::Library { .. }
                | Screen::Scan {
                    state: ui_scan::ScanState::Empty,
                    ..
                }
        ));
        assert_eq!(app.section, ui_nav::Tab::Library);
        app.leave().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_home_page_is_where_the_app_starts_when_a_book_is_read() {
        let dir = testdir("nav-start");
        let (root, library) = (dir.join("books"), dir.join("library"));
        std::fs::create_dir_all(&root).unwrap();
        write_book(&root.join("read.epub"));
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        let id = lib.books()[0].id.clone();
        let at = progress::progress_at(&aristotle_render::book::Position::Fraction(0.4), 0.4, 1);
        lib.set_progress(&id, at);
        lib.save().unwrap();
        let app = memory_app(&library, &root);
        assert!(
            matches!(app.screen, Screen::Home { .. }),
            "a book is being read: the app starts on the home page"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn held_controls_use_a_relative_gesture_deadline() {
        let dir = testdir("hold-deadline");
        let mut app = memory_app(&dir.join("library"), &dir.join("books"));
        app.show_settings(Refresh::Full, 0).unwrap();
        let night = (0..app.ui.tree.len())
            .map(|n| NodeId(n as u32))
            .find(|&n| app.ui.tree.node(n).action == Some(set_ui::action::NIGHT))
            .unwrap();
        let at = app.ui.tree.rect(night).center();
        let now_us = monotonic_us();
        touch(&mut app, HalPhase::Down, at.x as u32, at.y as u32, now_us);
        let remaining = app.ui.next_deadline(now_us).unwrap();
        assert!(remaining > 1000);
        assert_eq!(
            app.poll_timeout(now_us, Instant::now()),
            Duration::from_micros(remaining)
        );
    }

    #[test]
    fn ui_fonts_map_literata() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aristotle-render/assets/fonts");
        let fonts = ui_fonts(&dir, &dir).unwrap();
        assert_eq!(fonts.family_by_name("Literata"), Some(FamilyId(0)));
        assert!(ui_fonts(&dir.join("missing"), &dir.join("missing")).is_err());
    }

    /// The runtime data of `tools/build-render.sh host`, found as the
    /// renderer's tests find it.
    fn render_data() -> Option<PathBuf> {
        let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
        let dir = var("ARISTOTLE_RENDER_DATA")
            .map(PathBuf::from)
            .or_else(|| {
                var("ARISTOTLE_CACHE")
                    .map(PathBuf::from)
                    .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".cache/aristotle")))
                    .map(|c| c.join("render/host/data"))
            })?;
        dir.join("serif").is_dir().then_some(dir)
    }

    /// The app on an in-memory panel, as `--preview` runs it.
    fn memory_app(library: &Path, root: &Path) -> App {
        memory_app_sized(library, root, 1072, 1448)
    }

    fn memory_app_sized(library: &Path, root: &Path, w: u32, h: u32) -> App {
        memory_app_profile(library, root, ScreenProfile::new(w, h, 300).unwrap())
    }

    fn memory_app_profile(library: &Path, root: &Path, profile: ScreenProfile) -> App {
        let args = Args {
            data: PathBuf::new(),
            library: library.to_path_buf(),
            root: root.to_path_buf(),
            turn_mode: None,
            night_method: None,
            scan: true,
            bench_open: None,
            dicts: Vec::new(),
            preview: None,
            preview_size: None,
            preview_density: profile.density,
            open: None,
            index_dicts: false,
            test_clippings: None,
            lang: None,
            bundle: library.to_path_buf(),
            accounts: library.join("accounts"),
            fonts: library.join("fonts"),
        };
        let panel = Panel::memory(profile.width, profile.height, prefs::STOCK_TURN);
        build_app(
            &args,
            panel,
            Prefs::default(),
            start_dictionaries(&args),
            powerd::Sleep::none(),
        )
        .unwrap()
    }

    #[test]
    fn settings_can_leave_without_an_agent_and_report_unavailable_kual() {
        let library = testdir("settings-exit");
        let root = library.join("documents");
        std::fs::create_dir_all(&root).unwrap();
        let mut app = memory_app(&library, &root);
        app.show_settings(Refresh::Full, 0).unwrap();
        let event = |app: &App, action| {
            let node = (0..app.ui.tree.len())
                .map(|i| NodeId(i as u32))
                .find(|id| app.ui.tree.node(*id).action == Some(action))
                .unwrap();
            UiEvent::Action {
                id: action,
                node,
                at: aristotle_ui::geom::Point { x: 200, y: 240 },
            }
        };
        let kual = event(&app, set_ui::action::OPEN_KUAL);
        assert!(app.on_settings_event(kual).unwrap() == Flow::Continue);
        assert!(app.settings_message.is_some());
        assert!(app.exit_target == launcher::Target::Kindle);
        app.kual_available = true;
        let kual = event(&app, set_ui::action::OPEN_KUAL);
        assert!(app.on_settings_event(kual).unwrap() == Flow::Quit);
        assert_eq!(app.exit_target.exit_code(), launcher::KUAL_EXIT_CODE);
        let stock = event(&app, set_ui::action::RETURN_KINDLE);
        assert!(app.on_settings_event(stock).unwrap() == Flow::Quit);
        assert_eq!(app.exit_target.exit_code(), 0);
        app.leave().unwrap();
        std::fs::remove_dir_all(&library).unwrap();
    }

    #[test]
    fn the_source_picker_pages_and_survives_a_background_scan() {
        let library = testdir("source-picker");
        let root = library.join("books");
        for i in 0..7 {
            std::fs::create_dir_all(root.join(format!("folder-{i}"))).unwrap();
        }
        let mut app = memory_app(&library.join("index"), &root);
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Sources, None)
            .unwrap();
        let node = (0..app.ui.tree.len())
            .map(|i| NodeId(i as u32))
            .find(|&node| app.ui.tree.node(node).action == Some(ui_scan::action::NEXT_PAGE))
            .unwrap();
        app.on_scan_event(UiEvent::Action {
            id: ui_scan::action::NEXT_PAGE,
            node,
            at: aristotle_ui::geom::Point::default(),
        })
        .unwrap();
        let Screen::Scan {
            view,
            state: ui_scan::ScanState::Sources { path, .. },
        } = &app.screen
        else {
            panic!("pager must keep the source picker");
        };
        assert!(path.is_empty());
        assert_eq!(app.ui.tree.page(view.folder_list.unwrap()).0, 1);
        // A background scan finishing (or failing) behind the picker leaves
        // the reader's place and checks alone.
        let (tx, rx) = crate::wake::channel();
        drop(tx);
        app.scan_task = Some(ScanTask {
            confirmed_removed: Default::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
            purpose: ScanPurpose::Background { admit: Vec::new() },
            ack: None,
            _handle: None,
        });
        let index = app.lib.index().clone();
        app.handle_scan_done(Ok(ScanDone::Report(Default::default(), index)))
            .unwrap();
        assert!(matches!(
            app.screen,
            Screen::Scan {
                state: ui_scan::ScanState::Sources { .. },
                ..
            }
        ));
        app.leave().unwrap();
    }

    #[test]
    fn inline_push_queues_while_a_network_worker_is_running() {
        let library = testdir("inline-sync-queue");
        let root = library.join("books");
        std::fs::create_dir_all(&root).unwrap();
        let mut app = memory_app(&library.join("index"), &root);
        let session = inkread_core::sync::Session::new("test-access", "test-refresh", i64::MAX);
        app.net.accounts.readest = Some(aristotle_net::store::ReadestSettings::after_login(
            "test@example.com",
            &session,
            None,
        ));
        app.net.readest_session = inkread_core::sync::SessionStore::with_session(Some(session));
        app.readest.opened_book = Some("queued-book".into());
        let (release, blocked) = std::sync::mpsc::channel();
        app.net.spawn("queue-test", move |_| {
            blocked.recv().unwrap();
            net::Done::Login {
                register: false,
                result: Ok(()),
            }
        });
        app.readest_push(true, true);
        release.send(()).unwrap();
        while app.net.busy() {
            app.net.poll();
            std::thread::yield_now();
        }
        assert_eq!(
            app.net.accounts.readest.as_ref().unwrap().pending_queue,
            ["queued-book"]
        );
        assert!(app.readest.opened_book.is_none());
        let saved = aristotle_net::store::Accounts::load(&app.net.paths.state);
        assert_eq!(saved.readest.unwrap().pending_queue, ["queued-book"]);
        app.leave().unwrap();
    }

    fn updates(app: &App) -> Vec<String> {
        match &app.panel {
            Panel::Memory(m) => m.updates.iter().map(|u| u.label.clone()).collect(),
            Panel::Device(_) => Vec::new(),
        }
    }

    fn pixels(app: &App) -> &[u8] {
        match &app.panel {
            Panel::Memory(panel) => &panel.pixels,
            Panel::Device(_) => panic!("memory panel required"),
        }
    }

    #[test]
    fn last_screen_sleep_preserves_library_and_settings_pixels() {
        for settings in [false, true] {
            for mode in [
                prefs::SleepScreen::LastScreen,
                prefs::SleepScreen::BookCover,
            ] {
                let dir = testdir("sleep-screen");
                let mut app = memory_app(&dir.join("library"), &dir.join("books"));
                app.show_library(Refresh::Full).unwrap();
                if settings {
                    app.show_settings(Refresh::Full, 0).unwrap();
                    // One row cycles through the four screensaver modes: tap
                    // it until the wanted one shows, at least once.
                    let action = set_ui::action::SLEEP_SCREEN;
                    let mut taps = 0;
                    loop {
                        let node = (0..app.ui.tree.len())
                            .map(|n| NodeId(n as u32))
                            .find(|&n| app.ui.tree.node(n).action == Some(action))
                            .expect("the sleep-screen row is on the settings page");
                        app.on_settings_event(UiEvent::Action {
                            id: action,
                            node,
                            at: aristotle_ui::geom::Point::default(),
                        })
                        .unwrap();
                        taps += 1;
                        if app.prefs.sleep_screen == mode {
                            break;
                        }
                        assert!(taps < 4, "the row cycles through every sleep screen");
                    }
                    assert!(taps >= 1);
                    assert_eq!(Prefs::load(&dir.join("library")).sleep_screen, mode);
                } else {
                    app.prefs.sleep_screen = mode;
                }
                let held = if settings && mode == prefs::SleepScreen::LastScreen {
                    let night = (0..app.ui.tree.len())
                        .map(|n| NodeId(n as u32))
                        .find(|&n| app.ui.tree.node(n).action == Some(set_ui::action::NIGHT))
                        .unwrap();
                    let at = app.ui.tree.rect(night).center();
                    touch(
                        &mut app,
                        HalPhase::Down,
                        at.x as u32,
                        at.y as u32,
                        monotonic_us(),
                    );
                    Some(at)
                } else {
                    None
                };
                let before = updates(&app).len();
                let shown = pixels(&app).to_vec();
                app.fall_asleep(true).unwrap();
                assert!(app.asleep.is_some());
                assert!(app.debounce.deadline().is_none());
                if mode == prefs::SleepScreen::LastScreen {
                    assert_eq!(updates(&app).len(), before);
                    assert_eq!(pixels(&app), shown);
                } else {
                    assert_eq!(updates(&app)[before..], ["flash"]);
                    assert_ne!(pixels(&app), shown);
                }
                let asleep = updates(&app).len();
                app.background().unwrap();
                assert_eq!(updates(&app).len(), asleep);
                app.wake(true).unwrap();
                if let Some(at) = held {
                    touch(
                        &mut app,
                        HalPhase::Up,
                        at.x as u32,
                        at.y as u32,
                        monotonic_us(),
                    );
                    assert!(!app.prefs.night, "a pre-sleep press must not fire on wake");
                }
                assert!(app.asleep.is_none());
                if mode == prefs::SleepScreen::BookCover {
                    assert!(updates(&app).len() > asleep);
                } else {
                    assert!(!updates(&app)[asleep..].iter().any(|u| u == "flash"));
                }
                assert_eq!(matches!(app.screen, Screen::Settings { .. }), settings);
            }
        }
    }

    /// A tiny gray PNG for the screensaver folder.
    fn write_picture(path: &Path, gray: u8) {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 8, 8);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[gray; 64])
                .unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// The first node with this action on the screen.
    fn node_with_action(app: &App, action: aristotle_ui::tree::ActionId) -> Option<NodeId> {
        (0..app.ui.tree.len())
            .map(|i| NodeId(i as u32))
            .find(|&id| app.ui.tree.node(id).action == Some(action))
    }

    /// Every text on the screen, in node order.
    fn screen_texts(app: &App) -> Vec<String> {
        (0..app.ui.tree.len())
            .filter_map(|i| match &app.ui.tree.node(NodeId(i as u32)).kind {
                Kind::Text { para, .. } => Some(para.text()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_sleep_picker_keeps_the_rotation_and_reaches_the_empty_state() {
        let dir = testdir("sleep-picker-state");
        let root = dir.join("documents");
        let screensavers = dir.join("screensavers");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&screensavers).unwrap();
        write_picture(&screensavers.join("a.png"), 0x10);
        write_picture(&screensavers.join("b.png"), 0x20);
        let mut app = memory_app(&dir.join("library"), &root);

        // R2: a picked picture no longer removes the rotation cell. The
        // reader can always go back to walking through the folder.
        app.prefs.sleep_image = Some("a.png".into());
        app.show_sleep_images(Refresh::Full, 0).unwrap();
        assert!(
            node_with_action(&app, ui_sleep_images::action::ROTATE_ALL).is_some(),
            "the rotation cell disappeared when a picture was picked"
        );

        // R12: a picked picture that is gone is cleared from the settings,
        // so a later file of the same name is not picked by itself.
        app.prefs.sleep_image = Some("gone.png".into());
        app.show_sleep_images(Refresh::Full, 0).unwrap();
        assert_eq!(app.prefs.sleep_image, None);

        // R1: an empty folder shows the empty state and the way to the
        // Wi-Fi transfer page, instead of a grid with only the rotation.
        std::fs::remove_file(screensavers.join("a.png")).unwrap();
        std::fs::remove_file(screensavers.join("b.png")).unwrap();
        app.show_sleep_images(Refresh::Full, 0).unwrap();
        assert!(
            node_with_action(&app, ui_sleep_images::action::WIFI_TRANSFER).is_some(),
            "the empty state has no way to the transfer page"
        );
        assert!(
            node_with_action(&app, ui_sleep_images::action::ROTATE_ALL).is_none(),
            "the empty state still draws the rotation cell"
        );
    }

    /// R3: the header is built from the settings, not from the pictures of
    /// the page, so a picked picture on another page is still named.
    #[test]
    fn the_sleep_picker_header_keeps_the_picked_name_across_pages() {
        let dir = testdir("sleep-picker-header");
        let root = dir.join("documents");
        let screensavers = dir.join("screensavers");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&screensavers).unwrap();
        for i in 0..10 {
            write_picture(&screensavers.join(format!("{i:02}.png")), 0x10 + i);
        }
        let mut app = memory_app(&dir.join("library"), &root);
        app.prefs.sleep_image = Some("00.png".into());
        app.show_sleep_images(Refresh::Full, 1).unwrap();
        let wanted = aristotle_i18n::tf!(Msg::SleepImagesCurrent, name = "00.png");
        assert!(
            screen_texts(&app).iter().any(|t| t == &wanted),
            "the header fell back to the rotation on page 2"
        );
    }

    /// R13: the hardware page keys take the same route as the swipe and
    /// the pager arrows, with the same page edges.
    #[test]
    fn the_sleep_picker_page_keys_turn_pages() {
        let dir = testdir("sleep-picker-keys");
        let root = dir.join("documents");
        let screensavers = dir.join("screensavers");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&screensavers).unwrap();
        for i in 0..10 {
            write_picture(&screensavers.join(format!("{i:02}.png")), 0x10 + i);
        }
        let mut app = memory_app(&dir.join("library"), &root);
        app.show_sleep_images(Refresh::Full, 0).unwrap();
        let page = |app: &App| match app.screen {
            Screen::SleepImages { page, .. } => page,
            _ => panic!("not on the picker"),
        };
        assert_eq!(page(&app), 0);
        app.on_key(Key::PageForward).unwrap();
        assert_eq!(page(&app), 1);
        app.on_key(Key::PageForward).unwrap();
        assert_eq!(page(&app), 1, "the forward key ran past the last page");
        app.on_key(Key::PageBack).unwrap();
        assert_eq!(page(&app), 0);
        app.on_key(Key::PageBack).unwrap();
        assert_eq!(page(&app), 0, "the back key ran past the first page");
    }

    #[test]
    fn last_screen_sleep_finishes_snapshot_open_without_updates_and_saves_progress() {
        let Some((library, root, id)) = reopen_fixture(true) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        app.open_by_title("Reopened").unwrap();
        assert!(matches!(app.screen, Screen::Opening));
        app.opening.as_mut().unwrap().menu_open = true;
        app.sync.before_open = Some((id.clone(), None));
        let before = updates(&app).len();
        let shown = pixels(&app).to_vec();
        app.fall_asleep(true).unwrap();
        assert!(app.opening.is_none());
        assert!(app.pending.is_none());
        assert!(app.asleep.is_some());
        assert_eq!(updates(&app).len(), before);
        assert_eq!(pixels(&app), shown);
        assert_eq!(app.sync.before_open.as_ref().map(|p| &p.0), Some(&id));
        let Screen::Reader(reader) = &app.screen else {
            panic!("reader adopted");
        };
        let locator = reader
            .session
            .progress(aristotle_library::library::now())
            .locator;
        app.wake(true).unwrap();
        let Screen::Reader(reader) = &app.screen else {
            panic!("reader restored");
        };
        assert!(!reader.menu_open);
        assert!(reader.overlay.is_none());
        app.saver.flush();
        let saved = Library::open(&library).unwrap();
        assert_eq!(
            saved.book(&id).unwrap().progress.as_ref().unwrap().locator,
            locator
        );
    }

    #[test]
    fn failed_snapshot_open_defers_library_repaint_until_wake() {
        let Some((library, root, _)) = reopen_fixture(true) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        app.open_by_title("Reopened").unwrap();
        let before = updates(&app).len();
        let shown = pixels(&app).to_vec();
        app.adopt(Err(Error::msg("synthetic failed open")), true)
            .unwrap();
        app.fall_asleep(true).unwrap();
        assert_eq!(updates(&app).len(), before);
        assert_eq!(pixels(&app), shown);
        assert!(matches!(app.screen, Screen::Library { .. }));
        app.wake(true).unwrap();
        assert!(updates(&app).len() > before);
        assert!(matches!(app.screen, Screen::Library { .. }));
    }

    #[test]
    fn network_completions_update_state_without_repainting_or_retaining_busy_while_asleep() {
        for mode in [
            prefs::SleepScreen::LastScreen,
            prefs::SleepScreen::BookCover,
        ] {
            let dir = testdir("sleep-network");
            let mut app = memory_app(&dir.join("library"), &dir.join("books"));
            app.open_network(catalogs::NetView::Sync, 0).unwrap();
            app.prefs.sleep_screen = mode;
            app.fall_asleep(true).unwrap();
            let before = updates(&app).len();
            let shown = pixels(&app).to_vec();
            app.sync.checking = true;
            app.net.spawn("sleep-test", |_| net::Done::Login {
                register: false,
                result: Err(aristotle_net::kosync::Error::Unauthorized),
            });
            wait(&mut app, |app| !app.net.busy());
            assert!(!app.sync.checking);
            assert!(!app.sync.message.is_empty());
            assert!(app.asleep.is_some());
            assert_eq!(updates(&app).len(), before);
            assert_eq!(pixels(&app), shown);
            app.wake(true).unwrap();
            assert!(updates(&app).len() > before);
            assert!(matches!(app.screen, Screen::Network(_)));
        }
    }

    #[test]
    fn workers_do_not_arm_polling_timers_awake_or_asleep() {
        let Some((library, root, id)) = reopen_fixture(true) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        app.open_by_title("Reopened").unwrap();
        wait(&mut app, |app| matches!(app.screen, Screen::Reader(_)));
        let mut source =
            Source::new(&app.lib, app.lib.book(&id).unwrap(), &app.mobi_cache).unwrap();
        source.data_dir = testdir("sleep-paginator");
        let prepared = Session::prepare(source, app.prefs.style(), false).unwrap();
        let Screen::Reader(reader) = &mut app.screen else {
            panic!("reader required");
        };
        reader.session = Session::from_prepared(prepared).unwrap();
        assert!(reader.session.paginating());

        for waiting in [false, true] {
            for by_powerd in [false, true] {
                let Screen::Reader(reader) = &mut app.screen else {
                    panic!("reader required");
                };
                reader.overlay = if waiting {
                    Overlay::Waiting {
                        sel: selection::Selection {
                            anchor: (0, 0),
                            start: 0,
                            end: 0,
                        },
                        words: Rc::new(Vec::new()),
                    }
                } else {
                    Overlay::None
                };
                let now_us = monotonic_us();
                let now = Instant::now();
                app.prewarm.input();
                app.debounce.saved();
                app.last_touch = now;
                app.prefs.set_sleep(None);
                assert_eq!(app.poll_timeout(now_us, now), Duration::MAX);

                app.fall_asleep(by_powerd).unwrap();
                assert_eq!(app.poll_timeout(now_us, now), Duration::MAX);
                let (release, blocked) = std::sync::mpsc::channel();
                app.net.spawn("sleep-timeout", move |_| {
                    blocked.recv().unwrap();
                    net::Done::Login {
                        register: false,
                        result: Ok(()),
                    }
                });
                assert_eq!(app.poll_timeout(now_us, now), Duration::MAX);
                release.send(()).unwrap();
                wait(&mut app, |app| !app.net.busy());
                assert_eq!(app.poll_timeout(now_us, now), Duration::MAX);
                let Screen::Reader(reader) = &app.screen else {
                    panic!("reader retained");
                };
                assert!(reader.session.paginating());
                assert_eq!(matches!(reader.overlay, Overlay::Waiting { .. }), waiting);
                app.wake(by_powerd).unwrap();
            }
        }
    }

    /// Run the loop's background work until `done`, for at most 20 s.
    fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
        let t = Instant::now();
        while !done(app) {
            assert!(t.elapsed() < Duration::from_secs(20), "timed out");
            std::thread::sleep(Duration::from_millis(10));
            app.background().unwrap();
        }
    }

    /// Open the book from its snapshot and return the panel updates that
    /// took, the snapshot's flash included.
    fn reopen(app: &mut App, title: &str) -> Vec<String> {
        let before = updates(app).len();
        app.open_by_title(title).unwrap();
        assert!(matches!(app.screen, Screen::Opening), "no snapshot open");
        wait(app, |a| matches!(a.screen, Screen::Reader(_)));
        updates(app)[before..].to_vec()
    }

    /// An EPUB titled "Reopened" with two chapters in its contents (the
    /// footer's progress line needs chapters).
    fn write_book(path: &Path) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut put = |name: &str, data: String| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        };
        put("mimetype", "application/epub+zip".into());
        put(
            "META-INF/container.xml",
            "<?xml version=\"1.0\"?><container version=\"1.0\" \
             xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles>\
             <rootfile full-path=\"content.opf\" media-type=\"application/oebps-package+xml\"/>\
             </rootfiles></container>"
                .into(),
        );
        for c in 0..2 {
            let mut body = format!("<h1>Chapter {c}</h1>");
            for p in 0..40 {
                body.push_str(&format!(
                    "<p>Paragraph {p}. Every art, and every science reduced to a \
                     teachable form, aims, it is thought, at some good.</p>"
                ));
            }
            put(
                &format!("c{c}.xhtml"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?><html \
                     xmlns=\"http://www.w3.org/1999/xhtml\" lang=\"en\"><head><title>{c}\
                     </title></head><body>{body}</body></html>"
                ),
            );
        }
        put(
            "nav.xhtml",
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\" \
             xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>Contents</title></head>\
             <body><nav epub:type=\"toc\"><ol><li><a href=\"c0.xhtml\">Chapter 0</a></li>\
             <li><a href=\"c1.xhtml\">Chapter 1</a></li></ol></nav></body></html>"
                .into(),
        );
        put(
            "content.opf",
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><package \
             xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"id\">\
             <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier \
             id=\"id\">reopened</dc:identifier><dc:title>Reopened</dc:title>\
             <dc:language>en</dc:language></metadata><manifest>\
             <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" \
             properties=\"nav\"/>\
             <item id=\"c0\" href=\"c0.xhtml\" media-type=\"application/xhtml+xml\"/>\
             <item id=\"c1\" href=\"c1.xhtml\" media-type=\"application/xhtml+xml\"/>\
             </manifest><spine><itemref idref=\"c0\"/><itemref idref=\"c1\"/></spine></package>"
                .into(),
        );
        zip.finish().unwrap();
    }

    /// A library in a fresh directory with the "Reopened" book at 40 %,
    /// its page numbers on disk and a snapshot made ahead of time: before
    /// the page numbers were known (its footer lacks "x / y", so the open
    /// finds [`open::Verdict::Chrome`]), or after (`Same`). Returns the
    /// library, the books' root and the book's id.
    fn reopen_fixture(numbers_first: bool) -> Option<(PathBuf, PathBuf, String)> {
        let Some(data) = render_data() else {
            assert!(
                std::env::var_os("CI").is_none(),
                "no render data directory in CI"
            );
            eprintln!("skipped: no render data directory (run tools/build-render.sh host)");
            return None;
        };
        aristotle_render::use_data_dir(&data).unwrap();
        let dir = testdir("reopen");
        let (root, library) = (dir.join("books"), dir.join("library"));
        std::fs::create_dir_all(&root).unwrap();
        let book = root.join("reopened.epub");
        write_book(&book);
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        let id = lib.books()[0].id.clone();
        let at = progress::progress_at(&aristotle_render::book::Position::Fraction(0.4), 0.4, 1);
        lib.set_progress(&id, at);
        lib.save().unwrap();
        // A reading speed of its own: the footer's minutes depend on it.
        std::fs::write(
            library.join("reading.json"),
            r#"{"secs_per_page": 10.0, "samples": 2}"#,
        )
        .unwrap();
        let mut app = memory_app(&library, &root);
        let style = app.prefs.style();
        // The page numbers, as an earlier read leaves them.
        let paginate = || {
            let stamp = snapshot::file_stamp(&book).unwrap();
            let mut b = aristotle_render::book::Book::open(&book, &style, None).unwrap();
            b.paginate();
            let cache = pagecache::cache_path(&library, &id, stamp.0, stamp.1, &style);
            pagecache::store(&cache, b.pagination().unwrap()).unwrap();
        };
        if numbers_first {
            paginate();
        }
        let snap = snapshot::path(&library, &id);
        std::thread::sleep(prewarm::QUIET + Duration::from_millis(100));
        wait(&mut app, |_| snap.exists());
        // Let the job finish and be reaped.
        wait(&mut app, |a| a.prewarm.deadline().is_none());
        if !numbers_first {
            paginate();
        }
        // What the open will find.
        let rec = app.lib.book(&id).unwrap().clone();
        let mut src = Source::new(&app.lib, &rec, &app.mobi_cache).unwrap();
        src.secs_per_page = app.speed.secs_per_page();
        let shown = open::find_snapshot(&src, &style).expect("a valid snapshot");
        src.exact = Some(shown.info.location);
        let mut s =
            Session::from_prepared(Session::prepare(src, style.clone(), false).unwrap()).unwrap();
        let real = s.page(s.current).unwrap();
        let expected = if numbers_first {
            open::Verdict::Same
        } else {
            open::Verdict::Chrome
        };
        assert_eq!(open::compare(&shown.page, &real, &style), expected);
        app.leave().unwrap();
        Some((library, root, id))
    }

    /// Page-turn cost with the statistics recorder on and off. Run it on an
    /// ARM host with the render data (not on the Mac):
    ///
    /// ```text
    /// ARISTOTLE_RENDER_DATA=<...>/render/device/data \
    /// cargo test --release --target armv7-unknown-linux-musleabihf \
    ///     -p aristotle page_turn -- --ignored --nocapture
    /// ```
    ///
    /// It also checks that the recorded stays reach the file.
    #[test]
    #[ignore = "benchmark; run it on the device or an ARM host"]
    fn page_turn_cost_with_and_without_stats() {
        let Some((library, root, _)) = reopen_fixture(true) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        reopen(&mut app, "Reopened");
        const ROUNDS: usize = 3;
        const PAIRS: usize = 300;
        // Both neighbours are rendered and kept on the first pair.
        for _ in 0..10 {
            app.turn(Turn::Forward, monotonic_us()).unwrap();
            app.turn(Turn::Back, monotonic_us()).unwrap();
        }
        let mut off = Vec::with_capacity(ROUNDS * PAIRS * 2);
        let mut on = Vec::with_capacity(ROUNDS * PAIRS * 2);
        // Alternate the two modes so a drift of the machine hits both.
        for _ in 0..ROUNDS {
            for recording in [false, true] {
                app.stats.set_recording(recording);
                let times = if recording { &mut on } else { &mut off };
                for _ in 0..PAIRS {
                    // A real page was on the panel long enough to be recorded.
                    if recording {
                        app.stats.age_page(30);
                    }
                    let t = Instant::now();
                    app.turn(Turn::Forward, monotonic_us()).unwrap();
                    times.push(t.elapsed());
                    if recording {
                        app.stats.age_page(30);
                    }
                    let t = Instant::now();
                    app.turn(Turn::Back, monotonic_us()).unwrap();
                    times.push(t.elapsed());
                }
                app.stats.flush();
            }
        }
        for (recording, times) in [("off", &mut off), ("on", &mut on)] {
            times.sort_unstable();
            let mean = times.iter().map(|d| d.as_secs_f64()).sum::<f64>() / times.len() as f64;
            let p95 = times[times.len() * 95 / 100].as_secs_f64();
            eprintln!(
                "stats {recording}: {} turns, mean {:.0} µs, p95 {:.0} µs",
                times.len(),
                mean * 1e6,
                p95 * 1e6
            );
        }
        let records = crate::stats::Store::new(&library).records();
        assert!(!records.is_empty(), "the recorder wrote nothing");
        app.leave().unwrap();
        std::fs::remove_dir_all(library.parent().unwrap()).unwrap();
    }

    /// A copy of `library`: the fixture stays as it is for the next run.
    fn copy_library(library: &Path) -> PathBuf {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    copy(&e.path(), &to.join(e.file_name()));
                } else {
                    std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
                }
            }
        }
        let to = testdir("library");
        copy(library, &to);
        to
    }

    #[test]
    fn reading_and_restyle_use_the_panel_size_and_stride() {
        let Some(data) = render_data() else {
            assert!(
                std::env::var_os("CI").is_none(),
                "no render data directory in CI"
            );
            eprintln!("skipped: no render data directory (run tools/build-render.sh host)");
            return;
        };
        aristotle_render::use_data_dir(&data).unwrap();
        let dir = testdir("screen-sizes");
        let root = dir.join("books");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        write_book(&root.join("reopened.epub"));
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        lib.save().unwrap();
        for (w, h, dpi) in [
            (800, 1200, 300),
            (1072, 1448, 300),
            (1236, 1648, 300),
            (600, 800, 167),
            (758, 1024, 212),
        ] {
            let profile = ScreenProfile::new(w, h, dpi).unwrap();
            let mut app = memory_app_profile(&library, &root, profile);
            app.open_by_title("Reopened").unwrap();
            wait(&mut app, |a| matches!(a.screen, Screen::Reader(_)));
            for size in [3, 2] {
                app.prefs.size = size;
                let settings = app.prefs.menu_settings(0.5);
                // The layout happens on a worker thread: wait for it.
                app.request_restyle(settings).unwrap();
                wait(&mut app, |a| !a.restyler.busy());
                let stride = app.panel.stride();
                let Screen::Reader(r) = &mut app.screen else {
                    panic!("no book open")
                };
                assert_eq!((r.session.style.width, r.session.style.height), (w, h));
                assert_eq!(r.session.style.stride, stride);
                assert_eq!(r.session.style.dpi, dpi);
                assert_eq!(r.session.style.text_top, profile.density.px(104) as u32);
                let page = r.session.page(r.session.current).unwrap();
                assert_eq!((page.width, page.height, page.stride), (w, h, stride));
                let bounds = Rect::new(0, 0, w as i32, h as i32);
                if let Panel::Memory(m) = &app.panel {
                    assert!(m
                        .updates
                        .iter()
                        .all(|u| u.rect.intersect(&bounds) == Some(u.rect)));
                }
            }
            app.leave().unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn large_photo_library_can_open_a_book_and_show_a_sleep_screen() {
        let Some((library, root, id)) = reopen_fixture(true) else {
            return;
        };
        let lib = Library::open(&library).unwrap();
        let mut index = lib.index().clone();
        let original = lib.book(&id).unwrap();
        for i in 0..40 {
            let mut extra = original.clone();
            extra.id = format!("{:032x}", i + 1000);
            extra.meta.title = format!("Additional book {i}");
            index.books.push(extra);
        }
        index.save(&library.join("library.json")).unwrap();
        for sleep in [false, true] {
            let mut app = memory_app(&library, &root);
            app.prefs.covers = CoverPref::Photos;
            app.show_library(Refresh::Full).unwrap();
            if sleep {
                app.prefs.sleep_screen = prefs::SleepScreen::BookCover;
                app.fall_asleep(false).unwrap();
            } else {
                app.open_book(&id, None).unwrap();
                wait(&mut app, |a| matches!(a.screen, Screen::Reader(_)));
                app.render_ui().unwrap();
                app.prefs.sleep_screen = prefs::SleepScreen::BookCover;
                app.fall_asleep(false).unwrap();
                app.readest.manual_message = Some("Completed".into());
                app.refresh_reader_sync_status().unwrap();
            }
            app.leave().unwrap();
        }
    }

    #[test]
    fn pulled_progress_waits_for_the_borrowed_book_and_wake() {
        let Some((library, root, id)) = reopen_fixture(true) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        app.open_book(&id, None).unwrap();
        wait(&mut app, |a| matches!(a.screen, Screen::Reader(_)));
        app.settle_restyle().unwrap();
        let Screen::Reader(reader) = &mut app.screen else {
            panic!("reader required")
        };
        let book = reader.session.take_book().unwrap();
        app.readest.pending_pull = Some((id, Default::default()));
        app.apply_pending_readest_pull().unwrap();
        assert!(app.readest.pending_pull.is_some());
        let Screen::Reader(reader) = &mut app.screen else {
            panic!("reader required")
        };
        reader.session.restore_book(book);
        app.asleep = Some(0);
        app.apply_pending_readest_pull().unwrap();
        assert!(app.readest.pending_pull.is_some());
        app.asleep = None;
        app.apply_pending_readest_pull().unwrap();
        assert!(app.readest.pending_pull.is_none());
        app.leave().unwrap();
    }

    /// Reopening from a snapshot takes one update, even with an outdated footer.
    #[test]
    fn reopening_from_a_snapshot_is_one_update() {
        // A snapshot made before the page numbers were known (Chrome), and
        // one made with them known (Same).
        for numbers_first in [false, true] {
            let Some((library, root, _)) = reopen_fixture(numbers_first) else {
                return;
            };
            let mut app = memory_app(&library, &root);
            assert_eq!(reopen(&mut app, "Reopened"), ["flash"]);
            app.leave().unwrap();
        }
    }

    #[test]
    fn imported_annotation_opens_read_only_without_changing_the_reading_position() {
        use aristotle_library::index::{Annotation, AnnotationKind};
        fn reader(app: &App) -> &Reading {
            match &app.screen {
                Screen::Reader(r) => r,
                _ => panic!("not reading"),
            }
        }
        let Some((library, root, id)) = reopen_fixture(false) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        reopen(&mut app, "Reopened");
        let imported = Annotation {
            id: aristotle_library::index::generate_annotation_id(),
            kind: AnnotationKind::Highlight,
            text: Some("A passage written for this test.".into()),
            page: Some("xii".into()),
            location: Some((10, 11)),
            position: None,
            created: Some("2026-01-02 03:04:05".into()),
            modified: None,
            color: None,
            style: None,
            source: "kindle-clippings".into(),
            chapter: None,
            note: Some("An imported note.".into()),
        };
        app.lib.add_annotation(&id, imported.clone());
        let before = reader(&app).session.current;
        app.open_marks().unwrap();
        let Overlay::Marks { rows, .. } = &reader(&app).overlay else {
            panic!("no annotation list");
        };
        assert_eq!(rows.len(), 1);
        assert!(rows[0].target.is_none());
        app.on_overlay_event(UiEvent::Action {
            id: overlay::act::ITEM,
            node: NodeId(0),
            at: aristotle_ui::geom::Point { x: 0, y: 0 },
        })
        .unwrap();
        let Overlay::Annotation { text, target, .. } = &reader(&app).overlay else {
            panic!("no annotation detail");
        };
        assert!(text.text().contains("A passage written for this test."));
        assert!(text.text().contains("An imported note."));
        assert!(text.text().contains("xii"));
        assert!(target.is_none());
        assert_eq!(reader(&app).session.current, before);
        assert_eq!(app.lib.book(&id).unwrap().annotations, [imported]);
        app.leave().unwrap();
    }

    #[test]
    fn a_pull_from_a_previous_open_of_the_same_book_is_ignored() {
        let Some((library, root, id)) = reopen_fixture(false) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        reopen(&mut app, "Reopened");
        let old_token = app.sync.opened.as_ref().unwrap().token;
        app.back_to_library().unwrap();
        app.open_by_title("Reopened").unwrap();
        wait(&mut app, |a| matches!(a.screen, Screen::Reader(_)));
        let current_token = app.sync.opened.as_ref().unwrap().token;
        assert_ne!(old_token, current_token);
        app.sync.message = "current status".into();
        app.on_pulled(
            id.clone(),
            old_token,
            Err(aristotle_net::kosync::Error::Invalid("stale")),
        )
        .unwrap();
        assert_eq!(app.sync.message, "current status");
        assert!(app.sync.offer.is_none());
        app.on_pulled(
            id,
            current_token,
            Err(aristotle_net::kosync::Error::Invalid("current")),
        )
        .unwrap();
        assert_ne!(app.sync.message, "current status");
        app.leave().unwrap();
    }

    #[test]
    fn a_failed_screensaver_push_retries_without_more_reading() {
        use crate::sync::{PushOutcome, PushReceipt};
        let Some((library, root, id)) = reopen_fixture(false) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        reopen(&mut app, "Reopened");
        let token = app.sync.opened.as_ref().unwrap().token;
        let baseline = app.sync.opened.as_ref().unwrap().at_open.clone();
        let moved = aristotle_library::Locator::Epub {
            spine: 0,
            offset: 1,
        };
        assert_ne!(baseline.as_ref(), Some(&moved));
        let mut p = app.lib.book(&id).unwrap().progress.clone().unwrap();
        p.locator = moved.clone();
        p.fraction = 0.1;
        app.lib.set_progress(&id, p);
        app.net.accounts.sync = Some(aristotle_net::store::SyncSettings {
            server: "not a URL".into(),
            username: "test".into(),
            key: "test".into(),
            auto: true,
        });
        for _ in 0..2 {
            app.sync_push(false, false);
            assert!(app.net.busy(), "unchanged progress was not retried");
            wait(&mut app, |a| !a.net.busy());
            assert_eq!(app.sync.opened.as_ref().unwrap().at_open, baseline);
        }
        let receipt = |token, sequence, locator| PushReceipt {
            book: id.clone(),
            token,
            sequence,
            locator,
        };
        app.on_pushed(
            receipt(token, 10, moved.clone()),
            PushOutcome::Finished(Ok(())),
        );
        assert_eq!(
            app.sync.opened.as_ref().unwrap().at_open.as_ref(),
            Some(&moved)
        );
        let stale = aristotle_library::Locator::Epub {
            spine: 0,
            offset: 0,
        };
        app.on_pushed(
            receipt(token, 9, stale.clone()),
            PushOutcome::Finished(Ok(())),
        );
        app.on_pushed(
            receipt(token.wrapping_sub(1), 11, stale),
            PushOutcome::Finished(Ok(())),
        );
        assert_eq!(
            app.sync.opened.as_ref().unwrap().at_open.as_ref(),
            Some(&moved)
        );
        app.net.accounts.sync = None;
        app.leave().unwrap();
    }

    fn touch(app: &mut App, phase: HalPhase, x: u32, y: u32, time_us: u64) {
        let ev = aristotle_hal::input::TouchEvent {
            phase,
            id: 0,
            x,
            y,
            raw_x: x as i32,
            raw_y: y as i32,
            time_us,
        };
        app.on_touch(&ev).unwrap();
    }

    /// A 60 ms tap at (`x`, `y`).
    fn tap(app: &mut App, x: u32, y: u32) {
        let t = monotonic_us();
        touch(app, HalPhase::Down, x, y, t);
        touch(app, HalPhase::Up, x, y, t + 60_000);
    }

    /// A finger resting 600 ms at (`x`, `y`), then lifted.
    fn hold(app: &mut App, x: u32, y: u32) {
        let t = monotonic_us();
        touch(app, HalPhase::Down, x, y, t);
        app.tick_reading(t + 600_000).unwrap();
        touch(app, HalPhase::Up, x, y, t + 700_000);
    }

    /// What one step of input left: the panel's updates, its pixels after
    /// a full-screen update, the page, the menu and the overlay.
    #[derive(Debug, PartialEq)]
    struct After {
        updates: Vec<String>,
        pixels: Option<u64>,
        location: usize,
        menu_open: bool,
        overlay: String,
        bookmarked: bool,
    }

    fn after(app: &mut App, before: usize) -> After {
        use std::hash::{Hash, Hasher};
        let updates = updates(app)[before..].to_vec();
        let pixels = match &app.panel {
            Panel::Memory(m) if m.updates[before..].iter().any(|u| u.full_screen) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                m.pixels.hash(&mut h);
                Some(h.finish())
            }
            _ => None,
        };
        let Screen::Reader(r) = &mut app.screen else {
            panic!("not reading");
        };
        let overlay = match r.overlay {
            Overlay::None => "none",
            Overlay::Waiting { .. } => "waiting",
            Overlay::Dict { .. } => "dict",
            Overlay::Dragging { .. } => "dragging",
            Overlay::Actions { .. } => "actions",
            Overlay::Gloss { .. } => "gloss",
            Overlay::Note { .. } => "note",
            Overlay::Toc { .. } => "toc",
            Overlay::Marks { .. } => "marks",
            Overlay::Annotation { .. } => "annotation",
            Overlay::Fonts { .. } => "fonts",
            Overlay::GoTo { .. } => "go to",
            Overlay::Sync { .. } => "sync",
            Overlay::Typeset { .. } => "typeset",
        }
        .to_string();
        let location = r.session.current;
        After {
            updates,
            pixels,
            location,
            menu_open: r.menu_open,
            overlay,
            bookmarked: r.session.bookmarked(location).is_some(),
        }
    }

    type Step<'a> = &'a dyn Fn(&mut App);

    /// Reopen a copy of `library` and run `steps` on it, recording each.
    /// Returns the page reopened and what each step left.
    fn reopen_and_run(library: &Path, root: &Path, steps: &[Step]) -> (usize, Vec<After>) {
        let library = copy_library(library);
        let mut app = memory_app(&library, root);
        assert_eq!(reopen(&mut app, "Reopened"), ["flash"]);
        let Screen::Reader(r) = &app.screen else {
            panic!("not reading");
        };
        let start = r.session.current;
        let mut out = Vec::new();
        for step in steps {
            let before = updates(&app).len();
            step(&mut app);
            out.push(after(&mut app, before));
        }
        app.leave().unwrap();
        (start, out)
    }

    /// The centre of a word in the middle of the page.
    fn some_word(app: &mut App) -> (u32, u32) {
        let Screen::Reader(r) = &mut app.screen else {
            panic!("not reading");
        };
        let words = r.session.words(r.session.current);
        let w = &words[words.len() / 2];
        (
            ((w.rect.x0 + w.rect.x1) / 2) as u32,
            ((w.rect.y0 + w.rect.y1) / 2) as u32,
        )
    }

    /// After a reopen that kept the snapshot's page (its footer out of
    /// date), every input does what it does after a reopen that showed the
    /// real page: taps turn and open the menu by their zones, a hold
    /// selects, the bookmark corner marks the page, night mode switches,
    /// and once a turn or a mark redrew the pages, every page shown has the
    /// same pixels (coming back to the reopened page too).
    #[test]
    fn input_after_a_kept_snapshot_page_is_as_after_an_exact_one() {
        let Some(chrome) = reopen_fixture(false) else {
            return;
        };
        let same = reopen_fixture(true).unwrap();
        let corner = turns::bookmark_corner(Rect::screen());
        let (cx, cy) = ((corner.x + corner.w / 2) as u32, (corner.y + 40) as u32);
        let forward: Step = &|a| tap(a, 950, 800);
        let back: Step = &|a| tap(a, 150, 800);
        let middle: Step = &|a| tap(a, 536, 724);
        let top: Step = &|a| tap(a, 536, 80);
        let mark: Step = &|a| tap(a, cx, cy);
        let select: Step = &|a| {
            let (x, y) = some_word(a);
            hold(a, x, y);
        };
        // The menu's night switch.
        let night: Step = &|a| {
            let ev = UiEvent::Action {
                id: menu_action::NIGHT,
                node: NodeId(0),
                at: aristotle_ui::geom::Point { x: 0, y: 0 },
            };
            a.on_menu_event(ev).unwrap();
        };
        let scripts: [&[Step]; 7] = [
            // The tap seen opening the menu on the device, then back onto
            // the reopened page.
            &[forward, back],
            &[back, forward, forward],
            // The middle of the page turns forward too.
            &[middle, back],
            // The top strip opens the menu; a tap on the page closes it.
            &[top, middle, forward],
            &[select],
            &[mark, forward, back, mark],
            &[top, night, middle, forward, back],
        ];
        for (i, script) in scripts.iter().enumerate() {
            let (start_c, mut kept) = reopen_and_run(&chrome.0, &chrome.1, script);
            let (start_s, mut exact) = reopen_and_run(&same.0, &same.1, script);
            assert_eq!(start_c, start_s, "script {i}: the same page reopened");
            // Until the first turn, a full redraw of the reopened page (a
            // popup, night mode) still has the snapshot's header and footer.
            let mut turned = false;
            for (k, e) in kept.iter_mut().zip(&mut exact) {
                turned |= k.location != start_c || k.bookmarked;
                if !turned {
                    k.pixels = None;
                    e.pixels = None;
                }
            }
            assert_eq!(kept, exact, "script {i}");
        }
        // What the first script does: one turn forward, one back.
        let (start, steps) = reopen_and_run(&chrome.0, &chrome.1, scripts[0]);
        assert_eq!(steps[0].updates, ["turn"]);
        assert!(steps[0].location > start && !steps[0].menu_open);
        assert_eq!(steps[1].updates, ["turn"]);
        assert_eq!(steps[1].location, start);
    }

    /// Dragging a selection over several words keeps the page on the
    /// panel, with the selection shaded over it: each step is at most one
    /// update, never a blank page, and the lift settles the screen with one
    /// update that cleans what the drag's A2 steps left.
    #[test]
    fn dragging_a_selection_keeps_the_page() {
        let Some((library, root, _)) = reopen_fixture(true) else {
            return;
        };
        let library = copy_library(&library);
        let mut app = memory_app(&library, &root);
        assert_eq!(reopen(&mut app, "Reopened"), ["flash"]);
        let Screen::Reader(r) = &mut app.screen else {
            panic!("not reading");
        };
        let words = r.session.words(r.session.current);
        let page = r.session.page(r.session.current).unwrap();
        let ink = |data: &[u8], stride: usize| {
            (0..1448usize)
                .map(|y| {
                    data[y * stride..y * stride + 1072]
                        .iter()
                        .filter(|&&p| p < 0x80)
                        .count()
                })
                .sum::<usize>()
        };
        let page_ink = ink(&page.data, page.stride);
        assert!(page_ink > 1000, "a page with text");
        let centre = |i: usize| {
            let w = &words[i];
            (
                ((w.rect.x0 + w.rect.x1) / 2) as u32,
                ((w.rect.y0 + w.rect.y1) / 2) as u32,
            )
        };
        let first = words.len() / 3;
        let (x, y) = centre(first);
        let t = monotonic_us();
        touch(&mut app, HalPhase::Down, x, y, t);
        app.tick_reading(t + 600_000).unwrap();
        let mut steps = Vec::new();
        for k in 1..=6u64 {
            let before = updates(&app).len();
            let (x, y) = centre(first + k as usize * 2);
            touch(&mut app, HalPhase::Move, x, y, t + 600_000 + k * 60_000);
            app.render_ui().unwrap();
            let Panel::Memory(m) = &app.panel else {
                unreachable!()
            };
            let shown = ink(&m.pixels, m.stride);
            assert!(
                shown * 2 > page_ink,
                "step {k}: the page is on the panel ({shown} of {page_ink} dark pixels)"
            );
            let new: Vec<String> = m.updates[before..]
                .iter()
                .map(|u| format!("{} {}", u.mode, u.label))
                .collect();
            assert!(new.len() <= 1, "step {k}: one update at most: {new:?}");
            steps.push(new);
        }
        let before = updates(&app).len();
        let (x, y) = centre(first + 12);
        touch(&mut app, HalPhase::Up, x, y, t + 1_100_000);
        let Panel::Memory(m) = &app.panel else {
            unreachable!()
        };
        let last: Vec<(u32, bool)> = m.updates[before..]
            .iter()
            .map(|u| (u.mode, u.full_screen))
            .collect();
        assert_eq!(last, [(9, true)], "the lift: one clean update ({steps:?})");
        assert!(ink(&m.pixels, m.stride) * 2 > page_ink);
        let Screen::Reader(r) = &app.screen else {
            panic!("not reading");
        };
        assert!(matches!(r.overlay, Overlay::Actions { .. }));
        app.leave().unwrap();
    }

    /// Back to the library at once after a reopen that kept the snapshot's
    /// page: the progress is saved as after an exact reopen, and the next
    /// open is again one update, on the same page.
    #[test]
    fn leaving_at_once_after_a_kept_snapshot_page() {
        let Some(chrome) = reopen_fixture(false) else {
            return;
        };
        let same = reopen_fixture(true).unwrap();
        let run = |(library, root, id): &(PathBuf, PathBuf, String)| {
            let library = copy_library(library);
            let mut app = memory_app(&library, root);
            assert_eq!(reopen(&mut app, "Reopened"), ["flash"]);
            app.back_to_library().unwrap();
            assert!(matches!(app.screen, Screen::Library { .. }));
            let progress = app.lib.book(id).unwrap().progress.clone().unwrap();
            app.leave().unwrap();
            let mut app = memory_app(&library, root);
            assert_eq!(reopen(&mut app, "Reopened"), ["flash"]);
            let Screen::Reader(r) = &app.screen else {
                panic!("not reading");
            };
            let at = r.session.current;
            app.leave().unwrap();
            (progress.locator, progress.fraction, at)
        };
        assert_eq!(run(&chrome), run(&same));
    }

    /// Exiting a book in degraded mode preserves the previous accurate fraction
    /// instead of zeroing it out.
    #[test]
    fn degraded_mode_leave_preserves_library_fraction() {
        let Some((library, root, id)) = reopen_fixture(true) else {
            return;
        };
        let mut app = memory_app(&library, &root);
        reopen(&mut app, "Reopened");
        let initial_fraction = app
            .lib
            .book(&id)
            .unwrap()
            .progress
            .as_ref()
            .unwrap()
            .fraction;
        assert!(initial_fraction > 0.0, "initial fraction should be > 0");

        if let Screen::Reader(r) = &mut app.screen {
            r.session.degraded = true;
        }

        app.back_to_library().unwrap();
        assert!(matches!(app.screen, Screen::Library { .. }));

        let after_fraction = app
            .lib
            .book(&id)
            .unwrap()
            .progress
            .as_ref()
            .unwrap()
            .fraction;
        assert_eq!(
            after_fraction, initial_fraction,
            "fraction in library must be preserved across degraded exit"
        );
        app.leave().unwrap();
    }

    /// FIX-105: a reader whose library is empty still gets the shelf's
    /// Local / Cloud control, so the cloud shelf is one tap away. Signed out,
    /// that shelf stays the login guide.
    #[test]
    fn empty_library_reaches_the_cloud_shelf() {
        use aristotle_ui::geom::Point;
        use aristotle_ui::tree::ActionId;

        let dir = testdir("empty-library-cloud");
        let (root, library) = (dir.join("books"), dir.join("library"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&library).unwrap();

        let mut app = memory_app(&library, &root);
        app.show_library(Refresh::Full).unwrap();
        assert!(matches!(
            app.screen,
            Screen::Scan {
                state: ui_scan::ScanState::Empty,
                ..
            }
        ));

        let find_action = |app: &App, action: ActionId| {
            (0..app.ui.tree.len())
                .map(|i| NodeId(i as u32))
                .find(|id| app.ui.tree.node(*id).action == Some(action))
                .unwrap_or_else(|| panic!("action {action} not found"))
        };
        let cloud = find_action(&app, ui_scan::action::CLOUD_LIBRARY);
        app.on_ui_event(UiEvent::Action {
            id: ui_scan::action::CLOUD_LIBRARY,
            node: cloud,
            at: Point { x: 100, y: 100 },
        })
        .unwrap();
        assert!(matches!(app.screen, Screen::Cloud(_)));
        let login = find_action(&app, ui_cloud::action::LOGIN);
        assert!(app.ui.tree.rect(login).h > 0, "the login guide is drawn");

        app.leave().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn library_grid_view_toggle_filter_and_open() {
        use aristotle_ui::geom::Point;
        use aristotle_ui::tree::ActionId;

        let dir = testdir("grid-view-int");
        let (root, library) = (dir.join("books"), dir.join("library"));
        std::fs::create_dir_all(&root).unwrap();
        // Create 8 sample books so we have enough for pagination (6 items per page in grid view).
        for i in 0..8 {
            write_book(&root.join(format!("book_{i}.epub")));
        }
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        // Mark first book as reading.
        let id0 = lib.books()[0].id.clone();
        let at = progress::progress_at(&aristotle_render::book::Position::Fraction(0.4), 0.4, 1);
        lib.set_progress(&id0, at);
        lib.save().unwrap();

        let mut app = memory_app(&library, &root);
        app.show_library(Refresh::Full).unwrap();
        assert!(matches!(app.screen, Screen::Library { .. }));
        assert_eq!(app.prefs.library_layout, Layout::List);

        // 1. Toggle view to Grid.
        let find_action = |app: &App, action: ActionId| {
            (0..app.ui.tree.len())
                .map(|i| NodeId(i as u32))
                .find(|id| app.ui.tree.node(*id).action == Some(action))
                .unwrap_or_else(|| panic!("action {action} not found"))
        };

        let toggle_node = find_action(&app, ui_lib::action::TOGGLE_LAYOUT);
        app.on_ui_event(UiEvent::Action {
            id: ui_lib::action::TOGGLE_LAYOUT,
            node: toggle_node,
            at: Point { x: 100, y: 100 },
        })
        .unwrap();
        assert_eq!(app.prefs.library_layout, Layout::Grid);

        // In Grid mode with 8 books: 6 items on page 1, 2 on page 2.
        let Screen::Library { ref view, .. } = app.screen else {
            panic!("not in library");
        };
        let (page, count) = app.ui.tree.page(view.list);
        assert_eq!(page, 0);
        assert_eq!(count, 2);

        // 2. Next page in grid view.
        let next_node = find_action(&app, ui_lib::action::NEXT_PAGE);
        app.on_ui_event(UiEvent::Action {
            id: ui_lib::action::NEXT_PAGE,
            node: next_node,
            at: Point { x: 100, y: 100 },
        })
        .unwrap();
        let Screen::Library { ref view, .. } = app.screen else {
            panic!("not in library");
        };
        assert_eq!(app.ui.tree.page(view.list).0, 1);

        // 3. Filter in grid view (e.g. Reading).
        let reading_filter_node = find_action(&app, ui_lib::action::FILTER + 1);
        app.on_ui_event(UiEvent::Action {
            id: ui_lib::action::FILTER + 1,
            node: reading_filter_node,
            at: Point { x: 100, y: 100 },
        })
        .unwrap();
        assert_eq!(app.nav.filter, Filter::Reading);

        // 4. Reset filter to All and open a book in grid view.
        let all_filter_node = find_action(&app, ui_lib::action::FILTER);
        app.on_ui_event(UiEvent::Action {
            id: ui_lib::action::FILTER,
            node: all_filter_node,
            at: Point { x: 100, y: 100 },
        })
        .unwrap();
        assert_eq!(app.nav.filter, Filter::All);

        let book_node = find_action(&app, ui_lib::action::ITEM);
        let res = app.on_ui_event(UiEvent::Action {
            id: ui_lib::action::ITEM,
            node: book_node,
            at: Point { x: 100, y: 100 },
        });
        assert!(res.is_ok());

        // 5. Toggle view back to List.
        if !matches!(app.screen, Screen::Library { .. }) {
            app.show_library(Refresh::Full).unwrap();
        }
        let toggle_node2 = find_action(&app, ui_lib::action::TOGGLE_LAYOUT);
        app.on_ui_event(UiEvent::Action {
            id: ui_lib::action::TOGGLE_LAYOUT,
            node: toggle_node2,
            at: Point { x: 100, y: 100 },
        })
        .unwrap();
        assert_eq!(app.prefs.library_layout, Layout::List);

        app.leave().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn usb_drive_mode_lifecycle_and_background_freeze() {
        let dir = testdir("usb-drive-mode");
        let library = dir.join("library");
        let root = dir.join("books");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&root).unwrap();

        let mut app = memory_app(&library, &root);
        assert_eq!(app.usb_mode, UsbMode::Disconnected);

        // 1. Enter drive mode via DriveModeStarted
        app.on_usb_event(UsbEvent::DriveModeStarted, None).unwrap();
        assert_eq!(app.usb_mode, UsbMode::DriveMode);

        // In drive mode, poll_timeout must be 1s for fallback reconciliation
        let now_us = monotonic_us();
        let now = Instant::now();
        assert_eq!(app.poll_timeout(now_us, now), Duration::from_secs(1));

        // In drive mode, background() returns Ok immediately without writes
        assert!(app.background().is_ok());

        // In drive mode, on_touch and on_key are ignored and return Flow::Continue
        let ev = aristotle_hal::input::TouchEvent {
            phase: HalPhase::Down,
            id: 0,
            x: 100,
            y: 100,
            raw_x: 100,
            raw_y: 100,
            time_us: now_us,
        };
        assert_eq!(app.on_touch(&ev).unwrap(), Flow::Continue);
        assert_eq!(app.on_key(Key::PageForward).unwrap(), Flow::Continue);

        // 2. Duplicate start is idempotent
        app.on_usb_event(UsbEvent::DriveModeStarted, None).unwrap();
        assert_eq!(app.usb_mode, UsbMode::DriveMode);

        // 3. Exit drive mode via DriveModeEnded
        app.on_usb_event(UsbEvent::DriveModeEnded, None).unwrap();
        assert_eq!(app.usb_mode, UsbMode::Disconnected);

        // Normal poll timeout resumed
        let _ = app.poll_timeout(now_us, now);

        app.leave().unwrap();

        // 4. Test leave while in DriveMode (must not hang or fail on unmounted storage)
        let mut app_drive = memory_app(&library, &root);
        app_drive
            .on_usb_event(UsbEvent::DriveModeStarted, None)
            .unwrap();
        assert_eq!(app_drive.usb_mode, UsbMode::DriveMode);
        assert!(app_drive.leave().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Poll for a file the index saver writes on its own thread.
    fn wait_for_file(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        path.exists()
    }

    #[test]
    fn leave_flushes_the_library_except_in_drive_mode() {
        for drive in [false, true] {
            let dir = testdir(if drive { "leave-drive" } else { "leave-normal" });
            let (library, root) = (dir.join("library"), dir.join("books"));
            std::fs::create_dir_all(&root).unwrap();
            let mut app = memory_app(&library, &root);
            assert_eq!(app.usb_mode, UsbMode::Disconnected);
            if drive {
                app.on_usb_event(UsbEvent::DriveModeStarted, None).unwrap();
                assert_eq!(app.usb_mode, UsbMode::DriveMode);
            }
            app.leave().unwrap();
            let index = library.join(aristotle_library::index::INDEX_FILE);
            assert_eq!(
                index.exists(),
                !drive,
                "the index is left unsaved only in drive mode (drive mode: {drive})"
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn a_failed_open_saves_the_index_except_in_drive_mode() {
        for drive in [false, true] {
            let dir = testdir(if drive {
                "failed-open-drive"
            } else {
                "failed-open"
            });
            let (library, root) = (dir.join("library"), dir.join("books"));
            std::fs::create_dir_all(&root).unwrap();
            let mut app = memory_app(&library, &root);
            if drive {
                app.on_usb_event(UsbEvent::DriveModeStarted, None).unwrap();
            }
            app.save_after_failed_open();
            let index = library.join(aristotle_library::index::INDEX_FILE);
            // The saver writes in the background: give it time to appear
            // (and a moment to prove it does not, in drive mode).
            let written = wait_for_file(
                &index,
                if drive {
                    Duration::from_millis(200)
                } else {
                    Duration::from_secs(2)
                },
            );
            assert_eq!(
                written, !drive,
                "a failed open writes the index only outside drive mode (drive mode: {drive})"
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn import_worker_sees_a_book_only_present_in_the_live_library() {
        let dir = testdir("live-only-import");
        let root = dir.join("books");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("novel.txt"), b"A freshly imported local novel.\n").unwrap();
        let mut app = memory_app(&library, &root);
        app.lib.scan(&root, &ScanOptions::default()).unwrap();
        let id = app.lib.books()[0].id.clone();
        assert!(Library::open(&library).unwrap().books().is_empty());
        let destination = app.library_destination();
        app.spawn_scan(ScanPurpose::Import {
            sources: vec![root.clone()],
            new_paths: Vec::new(),
            indexed: vec![id.clone()],
            destination: destination.clone(),
        })
        .unwrap();
        wait_for_scan(&mut app);
        assert!(!root.join("novel.txt").exists());
        assert!(destination.join("novel.txt").exists());
        assert!(app
            .lib
            .book(&id)
            .unwrap()
            .file
            .path
            .ends_with("墨阅书库/novel.txt"));
        app.leave().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancelling_a_prepared_callback_queued_before_cancel_keeps_the_index_empty() {
        let dir = testdir("queued-import-cancel");
        let root = dir.join("books");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("book.txt"),
            b"An original queued-callback regression book.",
        )
        .unwrap();
        let mut app = memory_app(&library, &root);
        let mut worker = Library::open(&library).unwrap();
        worker.scan(&root, &ScanOptions::default()).unwrap();
        let destination = app.library_destination();
        let batch = aristotle_library::transfer::prepare(
            &root,
            &destination,
            worker.books(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(batch.books.len(), 1);
        let (_tx, rx) = crate::wake::channel();
        let (ack, reply) = std::sync::mpsc::channel();
        app.scan_task = Some(ScanTask {
            confirmed_removed: Default::default(),
            cancel: Arc::new(AtomicBool::new(true)),
            rx,
            purpose: ScanPurpose::Import {
                sources: Vec::new(),
                new_paths: vec!["book.txt".into()],
                indexed: Vec::new(),
                destination,
            },
            ack: Some(ack),
            _handle: None,
        });
        app.on_import_prepared(batch).unwrap();
        assert!(matches!(
            reply.recv_timeout(Duration::from_secs(1)).unwrap(),
            ImportDecision::Cancelled
        ));
        assert!(app.lib.books().is_empty());
        assert!(root.join("book.txt").exists());
        app.scan_task = None;
        app.leave().unwrap();
    }

    #[test]
    fn explicit_scan_confirmation_can_reimport_a_removed_book_with_progress() {
        let dir = testdir("reimport-history");
        let root = dir.join("books");
        let library = dir.join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("book.txt"),
            b"An original book with retained reading history.",
        )
        .unwrap();
        let mut lib = Library::open(&library).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        let id = lib.books()[0].id.clone();
        lib.set_progress(
            &id,
            aristotle_library::Progress {
                fraction: 0.4,
                locator: aristotle_library::Locator::Text { offset: 4 },
                updated: 4,
                source: None,
                page: None,
            },
        );
        lib.remove_book(&id);
        lib.save().unwrap();
        let mut app = memory_app(&library, &root);
        app.scan_whole = true;
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Sources, None)
            .unwrap();
        app.start_scan().unwrap();
        wait_for_scan(&mut app);
        app.toggle_review(0).unwrap();
        app.confirm_import().unwrap();
        wait_for_scan(&mut app);
        assert_eq!(app.lib.books().len(), 1);
        assert_eq!(
            app.lib
                .book(&id)
                .unwrap()
                .progress
                .as_ref()
                .unwrap()
                .fraction,
            0.4
        );
        assert!(!root.join("book.txt").exists());
        app.leave().unwrap();
    }

    #[test]
    fn a_failed_destination_setting_save_restores_the_previous_choice() {
        let dir = testdir("destination-save-failure");
        let root = dir.join("books");
        let library = dir.join("library");
        let backup = dir.join("library-before-failure");
        std::fs::create_dir_all(&root).unwrap();
        let mut app = memory_app(&library, &root);
        let previous = app.prefs.library_destination.clone();
        app.show_scan_sources(Refresh::Full, ui_scan::PickerMode::Destination, None)
            .unwrap();
        std::fs::rename(&library, &backup).unwrap();
        std::fs::write(&library, b"a file cannot hold preference files").unwrap();
        app.on_scan_event(UiEvent::Action {
            id: ui_scan::action::DEST_USE,
            node: NodeId(0),
            at: aristotle_ui::geom::Point::default(),
        })
        .unwrap();
        assert_eq!(app.prefs.library_destination, previous);
        assert!(matches!(
            &app.screen,
            Screen::Scan {
                state: ui_scan::ScanState::Done { error: Some(_), .. },
                ..
            }
        ));
        std::fs::remove_file(&library).unwrap();
        std::fs::rename(&backup, &library).unwrap();
        app.leave().unwrap();
    }
}
