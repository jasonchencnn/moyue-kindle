// SPDX-License-Identifier: AGPL-3.0-or-later
//! Preferences: the menu's typography steps (mapped onto aristotle-render's
//! [`Style`]), the page-turn waveform, night mode, library covers and view,
//! the screensaver delay, the UI language and whether highlights go to
//! `My Clippings.txt`.
//!
//! Stored as `reader.json` in the library data directory. They are global,
//! not per book: a reader sets a comfortable size once. Unknown or missing
//! fields fall back to the defaults, so the file can grow, and a file from
//! an older version keeps its values. The frontlight is not stored here: the
//! LM3695 keeps its level, and the app reads it back.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use aristotle_render::style::Style;
use aristotle_ui::screens::library::{CoverMode, LibraryLayout, View};
use aristotle_ui::screens::reader::Settings;
use serde::{Deserialize, Serialize};

use crate::fonts;

/// Body sizes in points for the menu's nine size steps. Step 3 is 10 pt,
/// the default chosen for Literata (`docs/reader.md`). The
/// steps match the UI prototype's pixel sizes (32 to 64 px at 300 DPI).
pub const FONT_PT: [f32; 9] = [7.5, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0];
/// Line heights in ems for tight, normal and loose. Normal is the 1.3 em
/// the renderer was tuned with.
pub const LINE_HEIGHT: [f32; 3] = [1.2, 1.3, 1.5];
/// Side margins in whole millimetres (Plato takes whole millimetres).
/// Normal is the renderer's default 5 mm.
pub const MARGIN_MM: [i32; 3] = [3, 5, 8];

/// The stock reader's page-turn waveform (mode 9, a full update of the
/// whole screen). Measured 503 ms from tap to page on the device.
pub const STOCK_TURN: u32 = 9;
/// DU4 (mode 7): 317 ms in the same measurement, four gray levels. Offered
/// as an option for readers who prefer speed.
pub const DU4_TURN: u32 = 7;

/// How night mode inverts the screen (`docs/ui-framework.md`, "Night mode").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NightMethod {
    /// Our pixels are inverted before they reach the framebuffer: the UI
    /// canvas by `Ui::set_night`, book pages in the copy to the framebuffer.
    #[default]
    Software,
    /// The framebuffer keeps day pixels and every update asks the PXP to
    /// invert (`EPDC_FLAG_ENABLE_INVERSION`).
    Hardware,
}

/// Library covers, as stored.
///
/// The default is the books' own covers: a fresh install shows them. A
/// `reader.json` from before that default says "none", and [`Prefs::load`]
/// moves it to "photos" once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Covers {
    None,
    Cards,
    #[default]
    Photos,
}

impl Covers {
    pub fn mode(self) -> CoverMode {
        match self {
            Covers::None => CoverMode::None,
            Covers::Cards => CoverMode::Cards,
            Covers::Photos => CoverMode::Photos,
        }
    }

    pub fn from_mode(m: CoverMode) -> Covers {
        match m {
            CoverMode::None => Covers::None,
            CoverMode::Cards => Covers::Cards,
            CoverMode::Photos => Covers::Photos,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepScreen {
    #[default]
    LastScreen,
    BookCover,
    CoverQuote,
    CoverClip,
    /// A picture of the reader's own, out of the `screensavers/` folder.
    CustomImage,
}

/// How the home screen quotation of the day is chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteMode {
    #[default]
    AllRandom,
    FavoritesRandom,
    Fixed,
}

impl From<QuoteMode> for aristotle_ui::screens::home::quotes::QuoteMode {
    fn from(m: QuoteMode) -> Self {
        match m {
            QuoteMode::AllRandom => aristotle_ui::screens::home::quotes::QuoteMode::AllRandom,
            QuoteMode::FavoritesRandom => {
                aristotle_ui::screens::home::quotes::QuoteMode::FavoritesRandom
            }
            QuoteMode::Fixed => aristotle_ui::screens::home::quotes::QuoteMode::Fixed,
        }
    }
}

impl From<aristotle_ui::screens::home::quotes::QuoteMode> for QuoteMode {
    fn from(m: aristotle_ui::screens::home::quotes::QuoteMode) -> Self {
        match m {
            aristotle_ui::screens::home::quotes::QuoteMode::AllRandom => QuoteMode::AllRandom,
            aristotle_ui::screens::home::quotes::QuoteMode::FavoritesRandom => {
                QuoteMode::FavoritesRandom
            }
            aristotle_ui::screens::home::quotes::QuoteMode::Fixed => QuoteMode::Fixed,
        }
    }
}

/// The layout of the library screen: list or 3x2 grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    List,
    Grid,
}

impl Layout {
    pub fn mode(self) -> LibraryLayout {
        match self {
            Layout::List => LibraryLayout::List,
            Layout::Grid => LibraryLayout::Grid,
        }
    }
}

fn default_latin_font() -> String {
    fonts::DEFAULT_LATIN.into()
}

fn default_cjk_font() -> String {
    fonts::DEFAULT_CJK.into()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub size: usize,
    pub spacing: usize,
    pub margins: usize,
    /// EPDC waveform mode for page turns.
    pub turn_mode: u32,
    pub night: bool,
    pub night_method: NightMethod,
    pub covers: Covers,
    /// Whether the one-time move of [`Prefs::covers`] has run: a library
    /// whose file said "none" when the default became "photos" is moved to
    /// photos once, and a reader who then picks "none" again keeps it.
    ///
    /// A `reader.json` from before the move has no such field. The field's
    /// own default keeps that `false` even though [`Prefs::default`] (a
    /// fresh install, with nothing to move) is `true`.
    #[serde(default = "covers_not_migrated")]
    pub covers_migrated: bool,
    /// Minutes without a touch before the screensaver; 0 is never.
    pub sleep_minutes: u32,
    pub sleep_screen: SleepScreen,
    /// The picture the screensaver shows when the sleep screen is
    /// [`SleepScreen::CustomImage`]: the file name inside the
    /// `screensavers/` folder.
    ///
    /// `None` means the rotation: the screensaver walks through the
    /// folder's pictures, in the order of their file names. A file
    /// written before the custom sleep screen keeps its meaning: the
    /// field is simply absent.
    #[serde(default)]
    pub sleep_image: Option<String>,
    /// Where the rotation stands: the number of times the screensaver
    /// has moved on to the next picture.
    ///
    /// Not stored in a file that was written before the custom sleep
    /// screen: the field is simply absent, and the rotation starts
    /// again at its first picture.
    #[serde(default)]
    pub sleep_image_turn: usize,
    /// Library view: 0 books, 1 folders, 2 series.
    pub library_view: usize,
    /// Library layout: list or 3x2 grid.
    #[serde(default)]
    pub library_layout: Layout,
    /// Index into `shelf::SORTS`.
    pub sort: usize,
    /// The UI language, a locale code of `aristotle-i18n` ("en", "pl",
    /// "zh").
    ///
    /// A file written before Chinese was the default keeps English: only
    /// a fresh install starts in Chinese.
    #[serde(default = "legacy_language")]
    pub language: String,
    /// The minutes of reading the daily goal asks for. The bar under
    /// "today" on the "mine" screen fills up to it.
    #[serde(default = "default_daily_goal")]
    pub daily_goal_minutes: u32,
    /// Append new highlights to the library root's `My Clippings.txt`, as
    /// the stock reader does. Off, highlights stay in the library index and
    /// the Markdown notes, and the user's file is never written.
    pub clippings: bool,
    /// The Latin reading font's family name: [`fonts::DEFAULT_LATIN`] or a family from
    /// the user's fonts folder (`fonts.rs`).
    #[serde(alias = "font", default = "default_latin_font")]
    pub font_latin: String,
    /// Where Plato loads [`Prefs::font_latin`] from, once the app has prepared
    /// it; `None` reads in Literata. Not stored: found again at each start.
    #[serde(skip)]
    pub font_dir: Option<String>,
    /// The UI's family for drawing the Latin font's name in the menu.
    #[serde(skip)]
    pub font_ui: usize,
    /// The CJK reading font's family name: [`fonts::DEFAULT_CJK`] or a family from
    /// the user's fonts folder (`fonts.rs`).
    #[serde(default = "default_cjk_font")]
    pub font_cjk: String,
    /// Where Plato loads [`Prefs::font_cjk`] from, once the app has prepared
    /// it; `None` reads in Noto Sans SC. Not stored: found again at each start.
    #[serde(skip)]
    pub font_cjk_dir: Option<String>,
    /// The UI's family for drawing the CJK font's name in the menu.
    #[serde(skip)]
    pub font_cjk_ui: usize,
    /// The style of the clock card of the home screen: an index into
    /// `aristotle_ui::screens::home::ClockStyle::ALL`.
    #[serde(default)]
    pub clock_style: u8,
    /// Whether the home screen shows the row of statistics.
    #[serde(default = "yes")]
    pub home_stats: bool,
    /// Whether the home screen shows the quotation of the day.
    #[serde(default = "yes")]
    pub home_quote: bool,
    /// Whether the desk cat fishes from the reading card.
    #[serde(default = "yes")]
    pub home_pet: bool,
    /// Page turn animation: 0: 关, 1: 开 · 慢, 2: 开 · 标准, 3: 开 · 快
    #[serde(default)]
    pub page_anim: u8,
    /// Full refresh interval: 0: 每页, 1: 每 10 页, 2: 每 30 页
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval: u8,
    /// How the home screen quotation of the day is chosen.
    #[serde(default)]
    pub quote_mode: QuoteMode,
    /// Quotations the reader has favorited, by their 1-based id.
    #[serde(default)]
    pub favorite_quotes: BTreeSet<u32>,
    /// Quotations the reader has hidden from the random pool, by their 1-based id.
    #[serde(default)]
    pub hidden_quotes: BTreeSet<u32>,
    /// The quotation to show when [`Prefs::quote_mode`] is [`QuoteMode::Fixed`].
    #[serde(default)]
    pub fixed_quote_id: Option<u32>,
    /// CJK first-line indent: 0: 2 字, 1: 无
    #[serde(default)]
    pub cjk_indent: usize,
    /// CJK script conversion: 0: 简体, 1: 繁体
    #[serde(default)]
    pub cjk_script: usize,
    /// CJK paragraph gap: 0: 无, 1: 小, 2: 大
    #[serde(default = "default_cjk_gap")]
    pub cjk_gap: usize,
    /// CJK punctuation trim: true: 开, false: 关
    #[serde(default = "yes")]
    pub cjk_trim: bool,
    /// The physical folder an explicit import moves books into (UX-112).
    /// `None` means the default, `墨阅书库` beside the app's own library
    /// folder. The folder is created only when a confirmed import runs.
    #[serde(default)]
    pub library_destination: Option<PathBuf>,
}

fn default_cjk_gap() -> usize {
    2
}

fn default_refresh_interval() -> u8 {
    1
}

/// The page interval the setting index offers: every page, ten, thirty.
pub fn refresh_interval_pages(index: u8) -> u32 {
    match index {
        0 => 1,
        1 => 10,
        _ => 30,
    }
}

/// A `reader.json` written before [`Prefs::covers`] defaulted to the books'
/// own covers has no marker, so it is read as not migrated yet. See
/// [`Prefs::covers_migrated`].
fn covers_not_migrated() -> bool {
    false
}

/// The default of a setting that is on unless the user turns it off.
fn yes() -> bool {
    true
}

/// The UI language of a `reader.json` written before Chinese existed: it
/// has no `language` field, and the reader it belonged to spoke English.
fn legacy_language() -> String {
    "en".into()
}

/// The daily reading goal: half an hour, as the prototype.
fn default_daily_goal() -> u32 {
    30
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            size: 3,
            spacing: 1,
            margins: 1,
            turn_mode: STOCK_TURN,
            night: false,
            night_method: NightMethod::Software,
            covers: Covers::Photos,
            covers_migrated: true,
            sleep_minutes: 10,
            sleep_screen: SleepScreen::LastScreen,
            sleep_image: None,
            sleep_image_turn: 0,
            library_view: 0,
            library_layout: Layout::List,
            sort: 0,
            language: "zh".into(),
            daily_goal_minutes: default_daily_goal(),
            clippings: true,
            font_latin: fonts::DEFAULT_LATIN.into(),
            font_dir: None,
            font_ui: 0,
            font_cjk: fonts::DEFAULT_CJK.into(),
            font_cjk_dir: None,
            font_cjk_ui: 0,
            clock_style: 0,
            home_stats: true,
            home_quote: true,
            home_pet: true,
            page_anim: 0,
            refresh_interval: 1,
            quote_mode: QuoteMode::AllRandom,
            favorite_quotes: BTreeSet::new(),
            hidden_quotes: BTreeSet::new(),
            fixed_quote_id: None,
            cjk_indent: 0,
            cjk_script: 0,
            cjk_gap: 2,
            cjk_trim: true,
            library_destination: None,
        }
    }
}

impl Prefs {
    pub const FILE: &'static str = "reader.json";

    /// Load from `dir`, or the defaults if there is no readable file.
    ///
    /// A file from before the library defaulted to the books' own covers is
    /// moved over once and written back, so that the marker stops a later
    /// choice of "none" from being moved again.
    pub fn load(dir: &Path) -> Prefs {
        let Ok(data) = std::fs::read(dir.join(Self::FILE)) else {
            return Prefs::default();
        };
        let Ok(prefs) = serde_json::from_slice::<Prefs>(&data) else {
            return Prefs::default();
        };
        let mut prefs = prefs.clamped();
        if prefs.migrate_covers() {
            // Best effort: a failed write only means the move is tried again
            // next start, with the same result.
            let _ = prefs.save(dir);
        }
        prefs
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let data = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        aristotle_library::index::write_atomic(&dir.join(Self::FILE), &data)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Moves a "none" cover setting to "photos" once, for a `reader.json`
    /// written before that was the default. Returns whether the file needs
    /// writing back. "cards" is a choice of its own and stays.
    fn migrate_covers(&mut self) -> bool {
        if self.covers_migrated {
            return false;
        }
        if self.covers == Covers::None {
            self.covers = Covers::Photos;
        }
        self.covers_migrated = true;
        true
    }

    fn clamped(mut self) -> Prefs {
        self.size = self.size.min(FONT_PT.len() - 1);
        self.spacing = self.spacing.min(LINE_HEIGHT.len() - 1);
        self.margins = self.margins.min(MARGIN_MM.len() - 1);
        self.library_view = self.library_view.min(View::ALL.len() - 1);
        self.sort = self.sort.min(crate::shelf::SORTS.len() - 1);
        self.daily_goal_minutes = self.daily_goal_minutes.clamp(1, 600);
        self.clock_style = self
            .clock_style
            .min(aristotle_ui::screens::home::ClockStyle::ALL.len() as u8 - 1);
        if self.font_latin.trim().is_empty() {
            self.font_latin = fonts::DEFAULT_LATIN.into();
        }
        if self.font_cjk.trim().is_empty() {
            self.font_cjk = fonts::DEFAULT_CJK.into();
        }
        self.cjk_indent = self.cjk_indent.min(1);
        self.cjk_script = self.cjk_script.min(1);
        self.cjk_gap = self.cjk_gap.min(2);
        if !aristotle_i18n::locales()
            .iter()
            .any(|l| l.code == self.language)
        {
            self.language = Prefs::default().language;
        }
        self
    }

    /// The page style for these preferences.
    pub fn style(&self) -> Style {
        let p = self.clone().clamped();
        let mut s = Style::pw4();
        s.font_size = FONT_PT[p.size];
        s.line_height = LINE_HEIGHT[p.spacing];
        s.margin_mm = MARGIN_MM[p.margins];
        s.font_family = p.font_latin.clone();
        s.cjk_font_family = p.font_cjk.clone();
        s.cjk_indent = aristotle_ui::screens::typeset::indent_chars(p.cjk_indent);
        s.cjk_gap = aristotle_ui::screens::typeset::gap_ratio(p.cjk_gap);
        if let Some(dir) = p.font_dir.filter(|_| p.font_latin != fonts::DEFAULT_LATIN) {
            s.font_dir = dir;
        }
        s
    }

    /// The Latin reading font in use: [`Prefs::font_latin`] once prepared, else
    /// Literata.
    pub fn font_latin_in_use(&self) -> &str {
        match self.font_dir {
            Some(_) => &self.font_latin,
            None => fonts::DEFAULT_LATIN,
        }
    }

    /// The CJK reading font in use: [`Prefs::font_cjk`] once prepared, else
    /// Noto Sans SC.
    pub fn font_cjk_in_use(&self) -> &str {
        match self.font_cjk_dir {
            Some(_) => &self.font_cjk,
            None => fonts::DEFAULT_CJK,
        }
    }

    /// The reading font in use (alias for Latin font).
    #[allow(dead_code)]
    pub fn font_in_use(&self) -> &str {
        self.font_latin_in_use()
    }

    /// How many page turns go between two full flashes, from the setting:
    /// every page, every ten, or every thirty.
    pub fn refresh_pages(&self) -> u32 {
        refresh_interval_pages(self.refresh_interval)
    }

    /// The menu's view of these preferences, with the frontlight level
    /// (0 to 1).
    pub fn menu_settings(&self, light: f32) -> Settings {
        Settings {
            size: self.size,
            spacing: self.spacing,
            margins: self.margins,
            light,
            night: self.night,
            family: self.font_ui,
            cjk_family: self.font_cjk_ui,
        }
    }

    /// Take the typography steps from the menu.
    pub fn apply(&mut self, s: &Settings) {
        self.size = s.size;
        self.spacing = s.spacing;
        self.margins = s.margins;
        *self = self.clone().clamped();
    }

    /// The screensaver delay as the settings screen offers it.
    pub fn sleep(&self) -> Option<u32> {
        (self.sleep_minutes > 0).then_some(self.sleep_minutes)
    }

    pub fn set_sleep(&mut self, minutes: Option<u32>) {
        self.sleep_minutes = minutes.unwrap_or(0);
    }

    /// The library place to open at start.
    pub fn view(&self) -> View {
        View::ALL[self.library_view.min(View::ALL.len() - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aristotle_ui::screens::settings::SLEEP_MINUTES;

    /// Whether `minutes` is one of the delays the settings screen offers.
    fn offered_sleep(minutes: Option<u32>) -> bool {
        SLEEP_MINUTES.contains(&minutes)
    }

    #[test]
    fn defaults_are_the_renderer_defaults() {
        let s = Prefs::default().style();
        let d = Style::pw4();
        assert_eq!(s.font_size, d.font_size);
        assert_eq!(s.line_height, d.line_height);
        assert_eq!(s.margin_mm, d.margin_mm);
        let p = Prefs::default();
        assert_eq!(p.turn_mode, 9);
        assert!(!p.night);
        assert_eq!(
            p.covers,
            Covers::Photos,
            "a fresh install shows the books' own covers"
        );
        assert!(
            p.clippings,
            "highlights go to My Clippings.txt, as on stock"
        );
        assert!(offered_sleep(p.sleep()));
    }

    #[test]
    fn steps_follow_the_menu_tables() {
        use aristotle_ui::screens::reader::{MARGINS, SIZES, SPACING};
        assert_eq!(FONT_PT.len(), SIZES.len());
        assert_eq!(LINE_HEIGHT.len(), SPACING.len());
        assert_eq!(MARGIN_MM.len(), MARGINS.len());
        // Points grow with the step, like the prototype's pixel sizes.
        assert!(FONT_PT.windows(2).all(|w| w[0] < w[1]));
        let mut p = Prefs::default();
        p.apply(&Settings {
            size: 8,
            spacing: 2,
            margins: 0,
            ..Settings::default()
        });
        let s = p.style();
        assert_eq!((s.font_size, s.line_height, s.margin_mm), (15.0, 1.5, 3));
    }

    #[test]
    fn file_round_trip_and_bad_values() {
        let dir = crate::testdir("prefs");
        assert_eq!(Prefs::load(&dir), Prefs::default());
        let p = Prefs {
            size: 4,
            spacing: 0,
            margins: 2,
            turn_mode: DU4_TURN,
            night: true,
            night_method: NightMethod::Hardware,
            covers: Covers::Cards,
            covers_migrated: true,
            sleep_minutes: 30,
            sleep_screen: SleepScreen::BookCover,
            sleep_image: None,
            sleep_image_turn: 0,
            library_view: 1,
            library_layout: Layout::Grid,
            sort: 2,
            language: "pl".into(),
            daily_goal_minutes: 45,
            clippings: false,
            font_latin: "Gentium Book".into(),
            font_dir: None,
            font_ui: 0,
            font_cjk: fonts::DEFAULT_CJK.into(),
            font_cjk_dir: None,
            font_cjk_ui: 0,
            clock_style: 0,
            home_stats: true,
            home_quote: true,
            home_pet: true,
            page_anim: 0,
            refresh_interval: 1,
            quote_mode: QuoteMode::AllRandom,
            favorite_quotes: BTreeSet::new(),
            hidden_quotes: BTreeSet::new(),
            fixed_quote_id: None,
            cjk_indent: 0,
            cjk_script: 0,
            cjk_gap: 2,
            cjk_trim: true,
            library_destination: None,
        };
        p.save(&dir).unwrap();
        assert_eq!(Prefs::load(&dir), p);
        std::fs::write(dir.join(Prefs::FILE), br#"{"size": 99, "future": 1}"#).unwrap();
        let q = Prefs::load(&dir);
        assert_eq!(q.size, FONT_PT.len() - 1);
        assert_eq!(q.turn_mode, STOCK_TURN);
        assert!(!q.night);
        // A file from before Chinese has no language field, so it stays
        // English; a language that is not embedded falls back to the
        // default, which is Chinese.
        assert_eq!(q.language, "en");
        std::fs::write(dir.join(Prefs::FILE), br#"{"language": "xx"}"#).unwrap();
        assert_eq!(Prefs::load(&dir).language, "zh");
    }

    #[test]
    fn a_fresh_install_speaks_chinese_and_older_files_stay_english() {
        assert_eq!(Prefs::default().language, "zh");
        let dir = crate::testdir("prefs-language");
        // No file at all: a fresh install.
        assert_eq!(Prefs::load(&dir).language, "zh");
        // A file from before Chinese: no language field, so English.
        std::fs::write(dir.join(Prefs::FILE), br#"{"size": 3}"#).unwrap();
        assert_eq!(Prefs::load(&dir).language, "en");
        // A file that names a language keeps it.
        for code in ["en", "pl", "zh"] {
            std::fs::write(
                dir.join(Prefs::FILE),
                format!(r#"{{"language": "{code}"}}"#).as_bytes(),
            )
            .unwrap();
            assert_eq!(Prefs::load(&dir).language, code);
        }
    }

    #[test]
    fn sleep_screen_defaults_for_old_files_and_persists_all_choices() {
        let old: Prefs = serde_json::from_str(r#"{"sleep_minutes":30}"#).unwrap();
        assert_eq!(old.sleep_screen, SleepScreen::LastScreen);
        assert_eq!(old.sleep_minutes, 30);
        let legacy_cover: Prefs = serde_json::from_str(r#"{"sleep_screen":"book_cover"}"#).unwrap();
        assert_eq!(legacy_cover.sleep_screen, SleepScreen::BookCover);
        for mode in [
            SleepScreen::LastScreen,
            SleepScreen::BookCover,
            SleepScreen::CoverQuote,
            SleepScreen::CoverClip,
            SleepScreen::CustomImage,
        ] {
            let prefs = Prefs {
                sleep_screen: mode,
                ..Prefs::default()
            };
            let encoded = serde_json::to_string(&prefs).unwrap();
            let decoded: Prefs = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded.sleep_screen, mode);
        }
    }

    #[test]
    fn a_file_from_before_the_custom_sleep_screen_still_loads() {
        // The two new fields are simply absent from an older file: the
        // sleep screen stays as it was, the rotation starts at its first
        // picture and no picture is picked.
        let old: Prefs = serde_json::from_str(r#"{"sleep_screen":"cover_quote"}"#).unwrap();
        assert_eq!(old.sleep_screen, SleepScreen::CoverQuote);
        assert_eq!(old.sleep_image, None);
        assert_eq!(old.sleep_image_turn, 0);
    }

    #[test]
    fn the_picked_picture_and_the_turn_are_kept() {
        let prefs = Prefs {
            sleep_screen: SleepScreen::CustomImage,
            sleep_image: Some("sunset.jpg".into()),
            sleep_image_turn: 3,
            ..Prefs::default()
        };
        let encoded = serde_json::to_string(&prefs).unwrap();
        assert!(
            encoded.contains(r#""sleep_screen":"custom_image""#),
            "{encoded}"
        );
        assert!(
            encoded.contains(r#""sleep_image":"sunset.jpg""#),
            "{encoded}"
        );
        assert!(encoded.contains(r#""sleep_image_turn":3"#), "{encoded}");
        let decoded: Prefs = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.sleep_screen, SleepScreen::CustomImage);
        assert_eq!(decoded.sleep_image.as_deref(), Some("sunset.jpg"));
        assert_eq!(decoded.sleep_image_turn, 3);
        // A rotation without a picked picture: the folder is walked
        // through in the order of the file names.
        let rotation: Prefs = serde_json::from_str(r#"{"sleep_image_turn":7}"#).unwrap();
        assert_eq!(rotation.sleep_image, None);
        assert_eq!(rotation.sleep_image_turn, 7);
    }

    #[test]
    fn settings_persist_as_readable_json() {
        let dir = crate::testdir("prefs-json");
        let mut p = Prefs {
            night: true,
            covers: Covers::Photos,
            ..Prefs::default()
        };
        p.set_sleep(None);
        p.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(Prefs::FILE)).unwrap();
        assert!(text.contains(r#""night": true"#), "{text}");
        assert!(text.contains(r#""night_method": "software""#), "{text}");
        assert!(text.contains(r#""covers": "photos""#), "{text}");
        assert!(text.contains(r#""clippings": true"#), "{text}");
        let q = Prefs::load(&dir);
        assert_eq!(q.sleep(), None, "never is kept");
        assert_eq!(q.covers.mode(), CoverMode::Photos);
        assert_eq!(q.library_layout, Layout::List);
        p.library_layout = Layout::Grid;
        p.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(Prefs::FILE)).unwrap();
        assert!(text.contains(r#""library_layout": "grid""#), "{text}");
        let q = Prefs::load(&dir);
        assert_eq!(q.library_layout, Layout::Grid);
        // An older file without the new fields keeps its typography and
        // gets the defaults for the rest.
        std::fs::write(
            dir.join(Prefs::FILE),
            br#"{"size": 3, "spacing": 2, "margins": 0, "turn_mode": 7}"#,
        )
        .unwrap();
        let old = Prefs::load(&dir);
        assert_eq!((old.size, old.turn_mode), (3, 7));
        assert_eq!(old.sleep_minutes, Prefs::default().sleep_minutes);
        assert_eq!(old.night_method, NightMethod::Software);
        assert!(old.clippings, "a file from before the switch keeps writing");
        // Out-of-range library choices are clamped.
        std::fs::write(dir.join(Prefs::FILE), br#"{"library_view": 9, "sort": 9}"#).unwrap();
        let q = Prefs::load(&dir);
        assert_eq!(q.view(), View::Series);
        assert_eq!(q.sort, crate::shelf::SORTS.len() - 1);
    }

    #[test]
    fn reading_font() {
        let dir = crate::testdir("prefs-font");
        let mut p = Prefs::default();
        assert_eq!(p.font_latin, "Literata");
        assert_eq!(p.font_cjk, "Noto Sans SC");
        assert_eq!(p.style().font_family, "Literata");
        assert_eq!(p.style().cjk_font_family, "Noto Sans SC");
        // Chosen but not prepared (the files are gone): Literata.
        p.font_latin = "Charis".into();
        assert_eq!(p.style().font_dir, Style::pw4().font_dir);
        assert_eq!(p.font_latin_in_use(), "Literata");
        p.font_dir = Some("/data/reading-fonts/Charis-0123".into());
        let s = p.style();
        assert_eq!(
            (s.font_family.as_str(), s.font_dir.as_str()),
            ("Charis", "/data/reading-fonts/Charis-0123")
        );
        assert_eq!(p.font_latin_in_use(), "Charis");
        // Stored by name; the folder is found again at start.
        p.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(Prefs::FILE)).unwrap();
        assert!(text.contains(r#""font_latin": "Charis""#), "{text}");
        assert!(text.contains(r#""font_cjk": "Noto Sans SC""#), "{text}");
        assert!(!text.contains("reading-fonts"), "{text}");
        let q = Prefs::load(&dir);
        assert_eq!(
            (q.font_latin.as_str(), q.font_dir.as_deref()),
            ("Charis", None)
        );
        assert_eq!(
            (q.font_cjk.as_str(), q.font_cjk_dir.as_deref()),
            ("Noto Sans SC", None)
        );
        // Older files with "font" deserialize to font_latin and default font_cjk.
        std::fs::write(dir.join(Prefs::FILE), br#"{"size": 3, "font": "Charis"}"#).unwrap();
        let migrated = Prefs::load(&dir);
        assert_eq!(migrated.font_latin, "Charis");
        assert_eq!(migrated.font_cjk, "Noto Sans SC");
        std::fs::write(dir.join(Prefs::FILE), br#"{"font": " "}"#).unwrap();
        assert_eq!(Prefs::load(&dir).font_latin, "Literata");
        std::fs::write(dir.join(Prefs::FILE), br#"{"font_cjk": " "}"#).unwrap();
        assert_eq!(Prefs::load(&dir).font_cjk, "Noto Sans SC");
    }

    #[test]
    fn cover_modes_round_trip() {
        for m in CoverMode::ALL {
            assert_eq!(Covers::from_mode(m).mode(), m);
        }
    }

    #[test]
    fn covers_default_to_photos_and_an_old_none_moves_once() {
        let dir = crate::testdir("prefs-covers");
        // A fresh install has no file at all and shows the books' covers.
        assert_eq!(Prefs::default().covers, Covers::Photos);
        assert!(Prefs::default().covers_migrated);
        assert_eq!(Prefs::load(&dir).covers, Covers::Photos);

        // A file from before the default changed says "none": it is moved to
        // photos, and the move is written down with the setting.
        std::fs::write(dir.join(Prefs::FILE), br#"{"covers": "none"}"#).unwrap();
        let p = Prefs::load(&dir);
        assert_eq!(p.covers, Covers::Photos);
        assert!(p.covers_migrated);
        let saved: Prefs =
            serde_json::from_slice(&std::fs::read(dir.join(Prefs::FILE)).unwrap()).unwrap();
        assert!(saved.covers_migrated, "the marker is saved");
        assert_eq!(saved.covers, Covers::Photos);

        // A reader who picks "none" after the move keeps it.
        let mut p = p;
        p.covers = Covers::None;
        p.save(&dir).unwrap();
        assert_eq!(Prefs::load(&dir).covers, Covers::None);

        // "cards" is a choice of its own: untouched, marker and all.
        std::fs::write(dir.join(Prefs::FILE), br#"{"covers": "cards"}"#).unwrap();
        let p = Prefs::load(&dir);
        assert_eq!(p.covers, Covers::Cards);
        assert!(p.covers_migrated);
        assert_eq!(Prefs::load(&dir).covers, Covers::Cards);

        // The three choices of the prototype are still offered.
        assert_eq!(CoverMode::ALL.len(), 3);
    }

    #[test]
    fn quote_preferences_round_trip() {
        let dir = crate::testdir("prefs-quotes");
        let mut p = Prefs::default();
        assert_eq!(p.quote_mode, QuoteMode::AllRandom);
        assert!(p.favorite_quotes.is_empty());
        assert!(p.hidden_quotes.is_empty());
        assert_eq!(p.fixed_quote_id, None);

        p.quote_mode = QuoteMode::FavoritesRandom;
        p.favorite_quotes.insert(1);
        p.favorite_quotes.insert(19);
        p.hidden_quotes.insert(5);
        p.fixed_quote_id = Some(42);
        p.save(&dir).unwrap();

        let text = std::fs::read_to_string(dir.join(Prefs::FILE)).unwrap();
        assert!(
            text.contains(r#""quote_mode": "favorites_random""#),
            "{text}"
        );
        assert!(text.contains(r#""favorite_quotes": ["#), "{text}");
        assert!(text.contains(r#""hidden_quotes": ["#), "{text}");
        assert!(text.contains(r#""fixed_quote_id": 42"#), "{text}");

        let loaded = Prefs::load(&dir);
        assert_eq!(loaded.quote_mode, QuoteMode::FavoritesRandom);
        assert!(loaded.favorite_quotes.contains(&1));
        assert!(loaded.favorite_quotes.contains(&19));
        assert!(loaded.hidden_quotes.contains(&5));
        assert_eq!(loaded.fixed_quote_id, Some(42));

        // An older file without quote fields gets defaults
        std::fs::write(dir.join(Prefs::FILE), br#"{"size": 3}"#).unwrap();
        let old = Prefs::load(&dir);
        assert_eq!(old.quote_mode, QuoteMode::AllRandom);
        assert!(old.favorite_quotes.is_empty());
        assert!(old.hidden_quotes.is_empty());
        assert_eq!(old.fixed_quote_id, None);
    }

    #[test]
    fn refresh_interval_maps_to_pages() {
        assert_eq!(refresh_interval_pages(0), 1);
        assert_eq!(refresh_interval_pages(1), 10);
        assert_eq!(refresh_interval_pages(2), 30);
        // Older files and unknown values fall back to the default interval.
        assert_eq!(refresh_interval_pages(9), 30);
        let mut p = Prefs::default();
        assert_eq!(p.refresh_pages(), 10);
        p.refresh_interval = 0;
        assert_eq!(p.refresh_pages(), 1);
    }

    #[test]
    fn style_maps_cjk_indent_and_gap() {
        let mut p = Prefs::default();
        // Defaults: cjk_indent: 0 (2 字), cjk_gap: 2 (0.6 大)
        let s = p.style();
        assert_eq!(s.cjk_indent, 2);
        assert!((s.cjk_gap - 0.6).abs() < 1e-4);

        // cjk_indent: 1 (无), cjk_gap: 0 (无 0.0)
        p.cjk_indent = 1;
        p.cjk_gap = 0;
        let s = p.style();
        assert_eq!(s.cjk_indent, 0);
        assert!((s.cjk_gap - 0.0).abs() < 1e-4);

        // cjk_gap: 1 (小 0.3)
        p.cjk_gap = 1;
        let s = p.style();
        assert!((s.cjk_gap - 0.3).abs() < 1e-4);
    }
}
