// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reader typeset panel according to Prototype v2 (F2, F3, X5).
//!
//! Renders the bottom sheet popup for reader typesetting settings with a live
//! text preview above it.

use crate::geom::{Insets, Size, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::text::{Align, FamilyId, Paragraph, TextEngine, TextStyle};
use crate::theme::{Styles, CARD, INK, INK_2, PAPER};
use crate::tree::{ActionId, Cross, Feedback, Kind, Node, NodeId, Tree};
use aristotle_i18n::{tr, Msg};

/// Localized display name for a font (e.g. "Noto Sans SC" -> "思源黑体" in Chinese UI).
pub fn display_font_name(name: &str) -> &str {
    if aristotle_i18n::language().starts_with("zh") {
        match name {
            "Noto Sans SC" => "思源黑体",
            "Noto Serif SC" => "思源宋体",
            _ => name,
        }
    } else {
        name
    }
}

/// Available body font sizes in pixels (9 legacy steps from 32 to 64).
pub const SIZES_PX: [i32; 9] = [32, 36, 40, 44, 48, 52, 56, 60, 64];

/// Font size stepper bounds in half-points (`12` = 6.0pt, `20` = 10.0pt, `38` = 19.0pt),
/// matching KOReader's 0.5pt fine adjustment precision.
pub const FONT_SIZE_MIN_HALF_PT: u16 = 12;
pub const FONT_SIZE_DEFAULT_HALF_PT: u16 = 20;
pub const FONT_SIZE_MAX_HALF_PT: u16 = 38;
pub const FONT_SIZE_STEP_HALF_PT: u16 = 1;

/// Legacy line spacing / pitch ratios (紧, 标准, 疏) kept for migrating old preferences.
pub const SPACING_RATIO: [f32; 3] = [1.4, 1.7, 2.0];

/// Legacy line spacing stepper bounds in tenths of an em (`0` = 0.0, `30` = 3.0).
pub const SPACING_MIN_TENTHS: u8 = 0;
pub const SPACING_DEFAULT_TENTHS: u8 = 17;
pub const SPACING_MAX_TENTHS: u8 = 30;

/// Line spacing stepper bounds in percent (`0` = 0%, `170` = 170%, `300` = 300%),
/// matching KOReader's `copt_line_spacing` unit (`%`) and 5% step precision.
pub const SPACING_MIN_PCT: u16 = 0;
pub const SPACING_DEFAULT_PCT: u16 = 170;
pub const SPACING_MAX_PCT: u16 = 300;
pub const SPACING_STEP_PCT: u16 = 5;

/// Legacy page margin options in pixels (宽, 标准, 窄).
pub const MARGINS_PX: [i32; 3] = [120, 88, 56];

/// Page margin stepper bounds in 300-DPI reference pixels (`0`..=`200`px, step `5`px),
/// matching KOReader's `copt_h_page_margins` pixel unit and 5px fine step.
pub const MARGIN_MIN_PX: u16 = 0;
pub const MARGIN_DEFAULT_PX: u16 = 60;
pub const MARGIN_MAX_PX: u16 = 200;
pub const MARGIN_STEP_PX: u16 = 5;

/// First-line indent stepper bounds in tenths of an em (`0` = 0.0em, `20` = 2.0em, `40` = 4.0em),
/// stepping by `0.5em` (`5` tenths).
pub const INDENT_MIN_TENTHS: u8 = 0;
pub const INDENT_DEFAULT_TENTHS: u8 = 20;
pub const INDENT_MAX_TENTHS: u8 = 40;
pub const INDENT_STEP_TENTHS: u8 = 5;

/// Legacy paragraph gap ratios (无: 0.0, 小: 0.3, 大: 0.6).
pub const GAP_RATIO: [f32; 3] = [0.0, 0.3, 0.6];

/// Paragraph gap stepper bounds in tenths of an em (`0` = 0.0em, `6` = 0.6em, `20` = 2.0em),
/// stepping by `0.1em` (`1` tenth).
pub const GAP_MIN_TENTHS: u8 = 0;
pub const GAP_DEFAULT_TENTHS: u8 = 6;
pub const GAP_MAX_TENTHS: u8 = 20;
pub const GAP_STEP_TENTHS: u8 = 1;

/// Number of font buttons shown per page in a font row.
pub const FONTS_PER_PAGE: usize = 3;

/// Action identifiers for typeset controls.
pub mod action {
    use crate::tree::ActionId;
    /// Close / collapse the typeset panel and return to reader.
    pub const COLLAPSE: ActionId = 600;
    /// Open the font transfer page.
    pub const ADD_FONT: ActionId = 601;
    /// CJK font selection base: `CJK_FONT_BASE + i`.
    pub const CJK_FONT_BASE: ActionId = 610;
    /// Latin font selection base: `LATIN_FONT_BASE + i`.
    pub const LATIN_FONT_BASE: ActionId = 630;
    /// Font size stepper: decrease by 0.5pt.
    pub const SIZE_DOWN: ActionId = 650;
    /// Font size stepper: increase by 0.5pt.
    pub const SIZE_UP: ActionId = 651;
    /// Line spacing stepper: decrease by 5%.
    pub const SPACING_DOWN: ActionId = 660;
    /// Line spacing stepper: increase by 5%.
    pub const SPACING_UP: ActionId = 661;
    /// Legacy margin selection base (kept for compatibility).
    pub const MARGIN_BASE: ActionId = 670;
    /// Page margin stepper: decrease by 5px.
    pub const MARGIN_DOWN: ActionId = 670;
    /// Page margin stepper: increase by 5px.
    pub const MARGIN_UP: ActionId = 671;
    /// Legacy first-line indent base (kept for compatibility).
    pub const INDENT_BASE: ActionId = 680;
    /// First-line indent stepper: decrease by 0.5em.
    pub const INDENT_DOWN: ActionId = 680;
    /// First-line indent stepper: increase by 0.5em.
    pub const INDENT_UP: ActionId = 681;
    /// Script conversion: `SCRIPT_BASE + i` (0: 简体, 1: 繁体).
    pub const SCRIPT_BASE: ActionId = 690;
    /// Legacy paragraph gap base (kept for compatibility).
    pub const GAP_BASE: ActionId = 700;
    /// Paragraph gap stepper: decrease by 0.1em.
    pub const GAP_DOWN: ActionId = 700;
    /// Paragraph gap stepper: increase by 0.1em.
    pub const GAP_UP: ActionId = 701;
    /// Punctuation trim: `TRIM_BASE + i` (0: 开, 1: 关).
    pub const TRIM_BASE: ActionId = 710;
    /// Page animation toggle: `ANIM_BASE + i` (0: 关, 1: 开).
    pub const ANIM_BASE: ActionId = 720;
    /// Page animation speed: `ANIM_SPEED_BASE + i` (0: 慢, 1: 标准, 2: 快).
    pub const ANIM_SPEED_BASE: ActionId = 730;
    /// Prev page of CJK fonts.
    pub const PREV_CJK_PAGE: ActionId = 740;
    /// Next page of CJK fonts.
    pub const NEXT_CJK_PAGE: ActionId = 741;
    /// Prev page of Latin fonts.
    pub const PREV_LATIN_PAGE: ActionId = 742;
    /// Next page of Latin fonts.
    pub const NEXT_LATIN_PAGE: ActionId = 743;

    /// 正文每行下方的引导线：0 无、1 实线、2 虚线（KO2-UNDERLINE-001）。
    pub const UNDERLINE_BASE: ActionId = 750;
    /// Fine typesetting: 0 on, 1 off (TYPE-102).
    pub const FINE_BASE: ActionId = 760;
    /// Font weight presets: `WEIGHT_BASE + i` (0 light 300, 1 normal 400,
    /// 2 bold 600).
    pub const WEIGHT_BASE: ActionId = 770;
    /// One step (25) lighter.
    pub const WEIGHT_DOWN: ActionId = 773;
    /// One step (25) heavier.
    pub const WEIGHT_UP: ActionId = 774;
    /// How wide the underline is: `UNDERLINE_SCOPE_BASE + i` (0 text only,
    /// 1 full line).
    pub const UNDERLINE_SCOPE_BASE: ActionId = 780;
    /// Writing direction: `WRITING_MODE_BASE + i` (0 横排 horizontal, 1 竖排
    /// vertical).
    pub const WRITING_MODE_BASE: ActionId = 790;
    /// Embedded book fonts in fine typesetting: `EMBEDDED_FONTS_BASE + i`
    /// (0 on / 允许, 1 off / 关闭).
    pub const EMBEDDED_FONTS_BASE: ActionId = 795;
    /// Screen orientation in reader/typeset: `ORIENTATION_BASE + i`
    /// (0 Portrait, 1 PortraitInverted, 2 LandscapeLeft, 3 LandscapeRight).
    pub const ORIENTATION_BASE: ActionId = 800;
    /// Fixed-layout fit mode: `FIT_MODE_BASE + i` (0 FitPage, 1 FitWidth, 2 FitHeight).
    pub const FIT_MODE_BASE: ActionId = 810;
    /// Fixed-layout page-turn direction: `PAGE_DIR_BASE + i` (0 LTR, 1 RTL manga).
    pub const PAGE_DIR_BASE: ActionId = 815;
    /// Fixed-layout fullscreen toggle: `FULLSCREEN_BASE + i` (0 on, 1 off).
    pub const FULLSCREEN_BASE: ActionId = 820;
    /// Fixed-layout auto-crop white margins: `AUTO_CROP_BASE + i` (0 on, 1 off).
    pub const AUTO_CROP_BASE: ActionId = 825;
    /// Fixed-layout landscape spread splitting: `SPLIT_SPREAD_BASE + i` (0 on, 1 off).
    pub const SPLIT_SPREAD_BASE: ActionId = 830;
    /// Fixed-layout night mode image preservation: `NIGHT_KEEP_IMG_BASE + i` (0 on, 1 off).
    pub const NIGHT_KEEP_IMG_BASE: ActionId = 835;
}

/// A font family option in the typography sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontOption {
    pub name: String,
    pub sample_font: FamilyId,
}

/// The state of the typography panel and its preview.
#[derive(Clone, Debug, PartialEq)]
pub struct TypesetView {
    pub title: String,
    pub author: String,
    pub cjk_fonts: Vec<FontOption>,
    pub selected_cjk_font: usize,
    pub cjk_page: usize,
    pub latin_fonts: Vec<FontOption>,
    pub selected_latin_font: usize,
    pub latin_page: usize,
    pub size_index: usize,
    /// Body font size in half-points (`12..=38`, i.e., `6.0pt` to `19.0pt`).
    pub font_size_half_pt: u16,
    /// Legacy spacing preset index (0..3), kept in sync with `spacing_pct`.
    pub spacing_index: usize,
    /// Legacy line spacing in tenths of an em (`0..=30`), kept in sync with `spacing_pct`.
    pub spacing_tenths: u8,
    /// Line spacing in percent (`0..=300`, step `5%`, e.g., `170` = `170%`).
    pub spacing_pct: u16,
    /// Legacy margin preset index (0..3).
    pub margin_index: usize,
    /// Page margin in 300-DPI reference pixels (`0..=200`, step `5px`).
    pub margin_ref_px: u16,
    /// Legacy first-line indent preset index (0: 2 字, 1: 无).
    pub indent_index: usize,
    /// First-line indent in tenths of an em (`0..=40`, step `5` = `0.5em`).
    pub indent_tenths: u8,
    pub script_index: usize,
    /// Legacy paragraph gap preset index (0..3).
    pub gap_index: usize,
    /// Paragraph gap in tenths of an em (`0..=20`, step `1` = `0.1em`).
    pub gap_tenths: u8,
    /// 正文每行下方的引导线：0 无、1 实线、2 虚线。
    pub underline_index: usize,
    /// How wide that guide is: 0 under the text only, 1 across the whole
    /// text block.
    pub underline_scope: usize,
    /// Whether crengine's modern typesetting is on (TYPE-101, TYPE-102).
    pub fine_typesetting: bool,
    /// Whether fine-typeset books may use their own embedded `@font-face`
    /// fonts.
    pub embedded_fonts: bool,
    /// Whether the currently open book embeds `@font-face` fonts (locks font
    /// pickers when `fine_typesetting && embedded_fonts`).
    pub fine_locks_fonts: bool,
    /// Whether the currently open book's CSS sets body `line-height` (locks
    /// line spacing when `fine_typesetting`).
    pub fine_locks_spacing: bool,
    /// Body text weight as a CSS `font-weight` number (100..=900).
    pub font_weight: u16,
    pub trim_index: usize,
    pub anim_enabled: bool,
    pub anim_speed: usize,
    /// 书写方向（writing direction）：0 横排（horizontal）、1 竖排（vertical）。
    pub writing_mode: usize,
    /// Screen orientation index (`0..=3`: Portrait / Inverted / LandscapeLeft / LandscapeRight).
    pub orientation: usize,
    /// When `true`, render the fixed-layout / comic (CBZ, PDF) sheet instead of
    /// reflowable typography controls.
    pub fixed_layout: bool,
    /// Fixed-layout fit mode index (`0`: FitPage, `1`: FitWidth, `2`: FitHeight).
    pub fit_mode: usize,
    /// Fixed-layout page-turn direction (`0`: LTR, `1`: RTL manga).
    pub page_direction: usize,
    /// Fixed-layout fullscreen mode (hide header/footer and margins).
    pub fullscreen: bool,
    /// Fixed-layout automatic white-margin cropping.
    pub auto_crop: bool,
    /// Fixed-layout automatic landscape two-page spread splitting.
    pub split_spread: bool,
    /// Keep fixed-layout artwork uninverted in night mode.
    pub keep_images_in_night: bool,
}

impl Default for TypesetView {
    fn default() -> Self {
        Self {
            title: "红楼梦".into(),
            author: "曹雪芹".into(),
            cjk_fonts: vec![FontOption {
                name: "思源黑体".into(),
                sample_font: FamilyId(0),
            }],
            selected_cjk_font: 0,
            cjk_page: 0,
            latin_fonts: vec![
                FontOption {
                    name: "Literata".into(),
                    sample_font: FamilyId(0),
                },
                FontOption {
                    name: "Charis".into(),
                    sample_font: FamilyId(0),
                },
                FontOption {
                    name: "Gentium".into(),
                    sample_font: FamilyId(0),
                },
            ],
            selected_latin_font: 0,
            latin_page: 0,
            size_index: 3, // 44 px (prototype default)
            font_size_half_pt: FONT_SIZE_DEFAULT_HALF_PT,
            spacing_index: 1, // 标准 (1.7)
            spacing_tenths: SPACING_DEFAULT_TENTHS,
            spacing_pct: SPACING_DEFAULT_PCT,
            margin_index: 1, // 标准 (88 px)
            margin_ref_px: MARGIN_DEFAULT_PX,
            indent_index: 0, // 2 字
            indent_tenths: INDENT_DEFAULT_TENTHS,
            script_index: 0, // 简体
            gap_index: 2,    // 大 (0.6)
            gap_tenths: GAP_DEFAULT_TENTHS,
            underline_index: 0, // 无
            underline_scope: 0, // 文字划线
            fine_typesetting: true,
            embedded_fonts: true,
            fine_locks_fonts: false,
            fine_locks_spacing: false,
            font_weight: WEIGHT_NORMAL,
            trim_index: 0, // 开
            anim_enabled: false,
            anim_speed: 1,   // 标准
            writing_mode: 0, // 横排
            orientation: 0,  // 正竖屏
            fixed_layout: false,
            fit_mode: 0,
            page_direction: 0,
            fullscreen: false,
            auto_crop: false,
            split_spread: false,
            keep_images_in_night: true,
        }
    }
}

/// Handles to key interactive nodes of the typeset screen.
pub struct TypesetIds {
    pub root: NodeId,
    pub collapse: NodeId,
    pub preview: NodeId,
}

// -----------------------------------------------------------------------------
// Pure conversion and mapping functions
// -----------------------------------------------------------------------------

/// Get pixel size for a font size index (0..9). Clamps to valid range.
pub fn size_px(index: usize) -> i32 {
    SIZES_PX[index.min(SIZES_PX.len() - 1)]
}

/// Find closest font size index for given pixel size.
pub fn size_index_from_px(px: i32) -> usize {
    SIZES_PX
        .iter()
        .enumerate()
        .min_by_key(|(_, &s)| (s - px).abs())
        .map(|(idx, _)| idx)
        .unwrap_or(3)
}

/// Step font size index down (`up` false) or up (`up` true), clamped to valid range.
pub fn size_index_stepped(index: usize, up: bool) -> usize {
    let clamped = index.min(SIZES_PX.len() - 1);
    if up {
        (clamped + 1).min(SIZES_PX.len() - 1)
    } else {
        clamped.saturating_sub(1)
    }
}

/// Clamp body font size in half-points to [`FONT_SIZE_MIN_HALF_PT`]..=[`FONT_SIZE_MAX_HALF_PT`].
pub fn font_size_half_pt_clamped(half_pt: u16) -> u16 {
    half_pt.clamp(FONT_SIZE_MIN_HALF_PT, FONT_SIZE_MAX_HALF_PT)
}

/// Step body font size in half-points by 0.5pt (`1` half-point) down or up, clamped.
pub fn font_size_half_pt_stepped(half_pt: u16, up: bool) -> u16 {
    let clamped = font_size_half_pt_clamped(half_pt);
    if up {
        clamped
            .saturating_add(FONT_SIZE_STEP_HALF_PT)
            .min(FONT_SIZE_MAX_HALF_PT)
    } else {
        clamped
            .saturating_sub(FONT_SIZE_STEP_HALF_PT)
            .max(FONT_SIZE_MIN_HALF_PT)
    }
}

/// Convert half-points to points (e.g. `20` -> `10.0`, `21` -> `10.5`).
pub fn font_size_pt_from_half_pt(half_pt: u16) -> f32 {
    font_size_half_pt_clamped(half_pt) as f32 / 2.0
}

/// Convert points to clamped half-points (e.g. `10.5` -> `21`).
pub fn font_size_half_pt_from_pt(pt: f32) -> u16 {
    font_size_half_pt_clamped((pt * 2.0).round().max(0.0) as u16)
}

/// Format body font size in half-points for display (e.g. `20` -> `"10.0pt"`, `21` -> `"10.5pt"`).
pub fn format_font_size_half_pt(half_pt: u16) -> String {
    let h = font_size_half_pt_clamped(half_pt);
    let whole = h / 2;
    let frac = if h % 2 == 1 { 5 } else { 0 };
    format!("{whole}.{frac}pt")
}

/// Convert half-points to 300-DPI reference pixels for live preview.
pub fn font_size_preview_px(half_pt: u16) -> i32 {
    (font_size_pt_from_half_pt(half_pt) * 300.0 / 72.0).round() as i32
}

/// Get line spacing multiplier for legacy spacing index (0: 紧, 1: 标准, 2: 疏).
pub fn spacing_ratio(index: usize) -> f32 {
    SPACING_RATIO[index.min(SPACING_RATIO.len() - 1)]
}

/// Clamp line spacing in tenths of an em to [`SPACING_MIN_TENTHS`]..=[`SPACING_MAX_TENTHS`].
pub fn spacing_tenths_clamped(tenths: u8) -> u8 {
    tenths.clamp(SPACING_MIN_TENTHS, SPACING_MAX_TENTHS)
}

/// Step line spacing in tenths of an em by 1 (`0.1`) down or up, clamped.
pub fn spacing_tenths_stepped(tenths: u8, up: bool) -> u8 {
    let clamped = spacing_tenths_clamped(tenths);
    if up {
        clamped.saturating_add(1).min(SPACING_MAX_TENTHS)
    } else {
        clamped.saturating_sub(1)
    }
}

/// Convert line spacing in tenths of an em to a float multiplier (e.g. `17` -> `1.7`).
pub fn spacing_ratio_from_tenths(tenths: u8) -> f32 {
    spacing_tenths_clamped(tenths) as f32 / 10.0
}

/// Format line spacing in tenths of an em for display (e.g. `17` -> `"1.7"`).
pub fn format_spacing_tenths(tenths: u8) -> String {
    let t = spacing_tenths_clamped(tenths);
    format!("{}.{}", t / 10, t % 10)
}

/// Clamp line spacing in percent to [`SPACING_MIN_PCT`]..=[`SPACING_MAX_PCT`].
pub fn spacing_pct_clamped(pct: u16) -> u16 {
    pct.clamp(SPACING_MIN_PCT, SPACING_MAX_PCT)
}

/// Step line spacing in percent by [`SPACING_STEP_PCT`] (5%) down or up, clamped.
pub fn spacing_pct_stepped(pct: u16, up: bool) -> u16 {
    let clamped = spacing_pct_clamped(pct);
    if up {
        clamped
            .saturating_add(SPACING_STEP_PCT)
            .min(SPACING_MAX_PCT)
    } else {
        clamped.saturating_sub(SPACING_STEP_PCT)
    }
}

/// Convert line spacing percentage to a float multiplier (e.g. `170` -> `1.7`).
pub fn spacing_ratio_from_pct(pct: u16) -> f32 {
    spacing_pct_clamped(pct) as f32 / 100.0
}

/// Format line spacing percentage for display (e.g. `170` -> `"170%"`).
pub fn format_spacing_pct(pct: u16) -> String {
    format!("{}%", spacing_pct_clamped(pct))
}

/// Get side margin in pixels for legacy margin index (0: 宽, 1: 标准, 2: 窄).
pub fn margin_px(index: usize) -> i32 {
    MARGINS_PX[index.min(MARGINS_PX.len() - 1)]
}

/// Find closest legacy margin index for given pixel margin.
pub fn margin_index_from_px(px: i32) -> usize {
    MARGINS_PX
        .iter()
        .enumerate()
        .min_by_key(|(_, &m)| (m - px).abs())
        .map(|(idx, _)| idx)
        .unwrap_or(1)
}

/// Clamp page margin in 300-DPI reference pixels to [`MARGIN_MIN_PX`]..=[`MARGIN_MAX_PX`].
pub fn margin_ref_px_clamped(px: u16) -> u16 {
    px.clamp(MARGIN_MIN_PX, MARGIN_MAX_PX)
}

/// Step page margin in 300-DPI reference pixels by [`MARGIN_STEP_PX`] (5px) down or up, clamped.
pub fn margin_ref_px_stepped(px: u16, up: bool) -> u16 {
    let clamped = margin_ref_px_clamped(px);
    if up {
        clamped.saturating_add(MARGIN_STEP_PX).min(MARGIN_MAX_PX)
    } else {
        clamped.saturating_sub(MARGIN_STEP_PX)
    }
}

/// Format page margin in reference pixels for display (e.g. `60` -> `"60px"`).
pub fn format_margin_ref_px(px: u16) -> String {
    format!("{}px", margin_ref_px_clamped(px))
}

/// Indent characters count (0: 2 字, 1: 无).
pub fn indent_chars(index: usize) -> usize {
    if index == 0 {
        2
    } else {
        0
    }
}

/// Clamp first-line indent in tenths of an em to [`INDENT_MIN_TENTHS`]..=[`INDENT_MAX_TENTHS`].
pub fn indent_tenths_clamped(tenths: u8) -> u8 {
    tenths.clamp(INDENT_MIN_TENTHS, INDENT_MAX_TENTHS)
}

/// Step first-line indent in tenths of an em by [`INDENT_STEP_TENTHS`] (0.5em) down or up, clamped.
pub fn indent_tenths_stepped(tenths: u8, up: bool) -> u8 {
    let clamped = indent_tenths_clamped(tenths);
    if up {
        clamped
            .saturating_add(INDENT_STEP_TENTHS)
            .min(INDENT_MAX_TENTHS)
    } else {
        clamped.saturating_sub(INDENT_STEP_TENTHS)
    }
}

/// Convert first-line indent in tenths of an em to a float em value (e.g. `20` -> `2.0`).
pub fn indent_em_from_tenths(tenths: u8) -> f32 {
    indent_tenths_clamped(tenths) as f32 / 10.0
}

/// Format first-line indent in tenths of an em for display (e.g. `20` -> `"2.0em"`).
pub fn format_indent_tenths(tenths: u8) -> String {
    let t = indent_tenths_clamped(tenths);
    format!("{}.{}em", t / 10, t % 10)
}

/// Whether script is traditional (0: 简体, 1: 繁体).
pub fn script_is_traditional(index: usize) -> bool {
    index == 1
}

/// Legacy paragraph gap ratio (0: 无 0.0, 1: 小 0.3, 2: 大 0.6).
pub fn gap_ratio(index: usize) -> f32 {
    GAP_RATIO[index.min(GAP_RATIO.len() - 1)]
}

/// Clamp paragraph gap in tenths of an em to [`GAP_MIN_TENTHS`]..=[`GAP_MAX_TENTHS`].
pub fn gap_tenths_clamped(tenths: u8) -> u8 {
    tenths.clamp(GAP_MIN_TENTHS, GAP_MAX_TENTHS)
}

/// Step paragraph gap in tenths of an em by [`GAP_STEP_TENTHS`] (0.1em) down or up, clamped.
pub fn gap_tenths_stepped(tenths: u8, up: bool) -> u8 {
    let clamped = gap_tenths_clamped(tenths);
    if up {
        clamped.saturating_add(GAP_STEP_TENTHS).min(GAP_MAX_TENTHS)
    } else {
        clamped.saturating_sub(GAP_STEP_TENTHS)
    }
}

/// Convert paragraph gap in tenths of an em to a float em value (e.g. `6` -> `0.6`).
pub fn gap_em_from_tenths(tenths: u8) -> f32 {
    gap_tenths_clamped(tenths) as f32 / 10.0
}

/// Format paragraph gap in tenths of an em for display (e.g. `6` -> `"0.6em"`).
pub fn format_gap_tenths(tenths: u8) -> String {
    let t = gap_tenths_clamped(tenths);
    format!("{}.{}em", t / 10, t % 10)
}

/// The guide under every line of body text (0: 无, 1: 实线, 2: 虚线).
pub fn underline_index(index: usize) -> usize {
    index.min(2)
}

/// The number of underline widths offered: text only and the full line.
pub const UNDERLINE_SCOPE_RANGE: usize = 2;

/// How wide the guide under a line of body text is (0: 文字划线, 1: 整行划线).
pub fn underline_scope_index(index: usize) -> usize {
    index.min(UNDERLINE_SCOPE_RANGE - 1)
}

/// Whether the guide spans the whole line instead of the text only.
pub fn underline_is_full_line(index: usize) -> bool {
    underline_scope_index(index) == 1
}

/// The writing direction in use (0: 横排, 1: 竖排), clamped to the two.
pub fn writing_mode_index(mode: usize) -> usize {
    mode.min(1)
}

/// The weights the three quick buttons set: light, normal and bold.
pub const WEIGHT_PRESETS: [u16; 3] = [WEIGHT_LIGHT, WEIGHT_NORMAL, WEIGHT_BOLD];

/// Body weights the engine takes, and the step of the two fine buttons.
pub const WEIGHT_MIN: u16 = 100;
pub const WEIGHT_LIGHT: u16 = 300;
pub const WEIGHT_NORMAL: u16 = 400;
pub const WEIGHT_BOLD: u16 = 600;
pub const WEIGHT_MAX: u16 = 900;
pub const WEIGHT_STEP: u16 = 25;

/// A weight inside the range the engine takes (100 to 900).
pub fn weight_clamped(weight: u16) -> u16 {
    weight.clamp(WEIGHT_MIN, WEIGHT_MAX)
}

/// The weight one step (25) lighter (`up` false) or heavier, at the ends of
/// the range.
pub fn weight_stepped(weight: u16, up: bool) -> u16 {
    let weight = weight_clamped(weight);
    if up {
        weight.saturating_add(WEIGHT_STEP).min(WEIGHT_MAX)
    } else {
        weight.saturating_sub(WEIGHT_STEP).max(WEIGHT_MIN)
    }
}

/// Whether the quick button at `index` is the weight in use. A weight between
/// the presets (say 450) lights none of them.
pub fn weight_preset_active(weight: u16, index: usize) -> bool {
    WEIGHT_PRESETS.get(index).is_some_and(|&w| w == weight)
}

/// Whether punctuation trim is enabled (0: 开, 1: 关).
pub fn trim_is_on(index: usize) -> bool {
    index == 0
}

/// Native width below which the sheet gives every chip the width of its
/// label instead of an equal share of the row.
const COMPACT_WIDTH: i32 = 700;

/// Horizontal padding of the sheet, in reference pixels.
const SHEET_PADDING: i32 = 88;

/// Room kept around a row label, in reference pixels.
const LABEL_GUTTER: i32 = 24;

/// Room kept around a chip label, in reference pixels.
const CHIP_GUTTER: i32 = 24;

/// The least room a chip keeps between its label and its border, in
/// reference pixels.
const CHIP_MIN_PAD: i32 = 10;

/// The least room a row label keeps between its text and the edge of its
/// box, in reference pixels.
const LABEL_MIN_PAD: i32 = 4;

/// Room left for the rounding of the density scaling, in reference pixels.
const BUDGET_SLACK: i32 = 4;

/// The least width of a short chip (a stepper's button, an on/off switch),
/// and the width of the value readout between two stepper buttons, in
/// reference pixels.
const SMALL_CHIP_W: i32 = 64;
const STEP_VALUE_W: i32 = 132;

/// Gap inside a stepper, between two chips of the same group, and between
/// the weight presets group and its fine-tuning stepper, in reference pixels.
const STEP_GAP: i32 = 8;
const CHIP_GAP: i32 = 10;
const WEIGHT_GROUP_GAP: i32 = 24;

/// Gaps of the sheet's header: between its title and the switch, between
/// the switch and its buttons, and between the buttons themselves.
const FINE_LABEL_GAP: i32 = 12;
const HEADER_GAP: i32 = 16;

/// Room the header keeps between its title and the rest of itself to show
/// the title at all.
const HEADER_MIN_GAP: i32 = 24;

/// Measures text for the sheet.
///
/// The layout shapes text when it draws, but the sheet has to know how wide
/// a label is before it can share a row out. The widths come from the text
/// engine and are given in reference pixels, the unit the tree is built in.
struct Measure<'a> {
    text: &'a mut TextEngine,
    /// Native pixels per reference pixel.
    density: f32,
}

impl Measure<'_> {
    /// The width of `s` in reference pixels.
    ///
    /// A font that has no glyph for the text measures it as nothing, so the
    /// width falls back to a conservative estimate: wide characters take a
    /// full em and Latin letters five eighths of one.
    fn width(&mut self, s: &str, style: &TextStyle) -> i32 {
        let em = (style.size / self.density).ceil() as i32;
        let estimate = estimated_text_width(s, em);
        if !self.text.covers(s, style) {
            return estimate;
        }
        let native = self.text.measure(s, style);
        ((native / self.density).ceil() as i32).max(estimate)
    }
}

/// Whether `c` takes a full em in the UI font: the CJK scripts, Hangul and
/// the fullwidth forms.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 | 0x20000..=0x3FFFD)
}

/// A conservative width for `text` at `em` reference pixels.
fn estimated_text_width(text: &str, em: i32) -> i32 {
    text.chars()
        .map(|c| {
            if is_wide(c) {
                em
            } else if c == ' ' {
                em * 3 / 10
            } else {
                em * 5 / 8
            }
        })
        .sum()
}

/// The widths a group of chips needs for its labels.
///
/// `hard` is the width every chip needs for its label and `pref` the width
/// it would like to have, both in reference pixels.
fn chip_group_widths<S: AsRef<str>>(
    m: &mut Measure,
    st: &Styles,
    names: &[S],
    selected: usize,
) -> (Vec<i32>, Vec<i32>) {
    let mut hard = Vec::new();
    let mut pref = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let mut style = st.font_xs();
        if i == selected {
            style = style.bold();
        }
        let w = m.width(name.as_ref(), &style);
        hard.push(w);
        pref.push(w + CHIP_GUTTER);
    }
    (hard, pref)
}

/// The flex weights for a group of chips.
///
/// `hard` is the width every chip needs for its label and `pref` the width
/// it would like to have, both in reference pixels; `free` is the width the
/// group has. The chips share the group equally when an equal share is wide
/// enough for every label, and in the width of their labels when it is not,
/// so that no label runs over its neighbour. `None` means the group needs a
/// line of its own.
fn chip_weights(
    hard: &[i32],
    pref: &[i32],
    free: i32,
    slots: i32,
    allow_equal: bool,
) -> Option<Vec<u16>> {
    let n = hard.len();
    if n == 0 {
        return Some(Vec::new());
    }
    let equal = free / slots.max(1);
    if allow_equal && hard.iter().all(|&w| w <= equal) {
        return Some(vec![1; n]);
    }
    if pref.iter().sum::<i32>() + BUDGET_SLACK > free {
        return None;
    }
    Some(
        pref.iter()
            .map(|&w| w.clamp(1, u16::MAX as i32) as u16)
            .collect(),
    )
}

/// Room kept inside a two-line font chip, in reference pixels.
const FONT_CHIP_PADDING: i32 = 30;

/// Gap between the two stacked lines of a compact row, in reference pixels.
const STACK_GAP: i32 = 12;

/// Chip styling properties: (border_width, border_color, bg_color, fg_color, is_bold).
pub fn chip_style(active: bool) -> (i32, u8, u8, u8, bool) {
    if active {
        (0, INK, INK, PAPER, true)
    } else {
        (2, INK, PAPER, INK, false)
    }
}

// -----------------------------------------------------------------------------
// UI Construction
// -----------------------------------------------------------------------------

/// Builds a styled chip button.
#[allow(clippy::too_many_arguments)]
fn chip_button(
    t: &mut Tree,
    st: &Styles,
    label: &str,
    action: ActionId,
    active: bool,
    width: Option<i32>,
    height: i32,
    flex: u16,
) -> NodeId {
    let (border_w, border_col, bg_col, fg_col, is_bold) = chip_style(active);
    let mut style = if is_bold {
        st.font_xs().bold()
    } else {
        st.font_xs()
    };
    style = style.gray(fg_col);

    let text = t.label(label, style);
    let center = t.align(0.5, 0.5, text);
    let mut node = Node::new(Kind::Fixed {
        w: width,
        h: Some(height),
    })
    .bg(bg_col)
    .border(border_w, border_col, 16)
    .action(action)
    .feedback(Feedback::Invert);

    if width.is_none() {
        node = node.flex(flex.max(1));
    }
    t.add(node, [center])
}

/// Builds a 2-line font selection button (font name + sample text).
#[allow(clippy::too_many_arguments)]
fn font_chip_button(
    t: &mut Tree,
    st: &Styles,
    name: &str,
    sample: &str,
    sample_family: FamilyId,
    action: ActionId,
    active: bool,
    height: i32,
    flex: u16,
) -> NodeId {
    let (border_w, border_col, bg_col, fg_col, is_bold) = chip_style(active);

    let name_style = if is_bold {
        st.font_xs().bold().gray(fg_col)
    } else {
        st.font_xs().gray(fg_col)
    };
    let name_node = t.label(name, name_style);

    let mut sample_style = st.font_xs().gray(fg_col);
    sample_style.family = sample_family;
    let sample_node = t.label(sample, sample_style);

    let col = t.add(
        Node::column(6).cross(Cross::Center),
        [name_node, sample_node],
    );
    let center = t.align(0.5, 0.5, col);
    let pad = t.pad(Insets::vh(8, 12), center);

    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(height),
        })
        .flex(flex.max(1))
        .bg(bg_col)
        .border(border_w, border_col, 16)
        .action(action)
        .feedback(Feedback::Invert),
        [pad],
    )
}

/// Builds a non-interactive read-only badge for settings locked by publisher fine typesetting.
fn fine_locked_badge(t: &mut Tree, st: &Styles, height: i32) -> NodeId {
    let text = t.label(tr(Msg::TypesetFineLocked), st.font_xs().gray(INK_2));
    let center = t.align(0.5, 0.5, text);
    let pad = t.pad(Insets::vh(0, 14), center);
    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(height),
        })
        .flex(1)
        .bg(PAPER)
        .border(2, INK_2, 16),
        [pad],
    )
}

/// Builds a numeric stepper (`−  <value>  +`).
fn numeric_stepper(
    t: &mut Tree,
    st: &Styles,
    value_text: &str,
    down_action: ActionId,
    up_action: ActionId,
    value_w: i32,
    height: i32,
) -> NodeId {
    let step = |t: &mut Tree, label: &str, action: ActionId| {
        let (border_w, border_col, bg_col, fg_col, _) = chip_style(false);
        let text = t.label(label, st.font_xs().bold().gray(fg_col));
        let center = t.align(0.5, 0.5, text);
        t.add(
            Node::new(Kind::Fixed {
                w: Some(SMALL_CHIP_W),
                h: Some(height),
            })
            .bg(bg_col)
            .border(border_w, border_col, 16)
            .action(action)
            .feedback(Feedback::Invert),
            [center],
        )
    };
    let number = t.label(value_text, st.font_xs().bold().gray(INK));
    let number = t.align(0.5, 0.5, number);
    let value = t.add(
        Node::new(Kind::Fixed {
            w: Some(value_w),
            h: Some(height),
        })
        .bg(PAPER)
        .border(2, INK, 16),
        [number],
    );
    let down = step(t, "−", down_action);
    let up = step(t, "+", up_action);
    let row = t.add(Node::row(STEP_GAP).cross(Cross::Center), [down, value, up]);
    let total_w = 2 * SMALL_CHIP_W + value_w + 2 * STEP_GAP;
    t.fixed(Some(total_w), Some(height), Some(row))
}

/// Builds the typeset screen tree according to `ReaderTypeset.dc.html`.
///
/// This is the reference-panel variant; callers that know the panel's native
/// size use [`build_typeset_screen_for_size`].
pub fn build_typeset_screen(
    t: &mut Tree,
    text: &mut TextEngine,
    st: &Styles,
    v: &TypesetView,
) -> (NodeId, TypesetIds) {
    build_typeset_screen_for_size(t, text, st, v, Size::new(SCREEN_WIDTH, SCREEN_HEIGHT))
}

/// Builds the typeset screen for a panel of `size` native pixels.
///
/// The rows of the sheet share the panel width, so every label and every
/// chip has to fit the share it is given. The widths of the labels are
/// measured with the text engine and budgeted in reference pixels; a chip
/// group that does not fit beside its label takes a line of its own.
pub fn build_typeset_screen_for_size(
    t: &mut Tree,
    text: &mut TextEngine,
    st: &Styles,
    v: &TypesetView,
    size: Size,
) -> (NodeId, TypesetIds) {
    if v.fixed_layout {
        return build_fixed_typeset_screen(t, text, st, v, size);
    }
    let dpi = st.density.dpi() as f32;
    // The tree will scale reference pixels by the density after the screen is
    // built (`Ui::set_tree_with`); the budget works on the reference width
    // the panel will have at that density.
    // Below the reference width every chip takes the width of its label;
    // from the reference width up the chips share the row equally, as long
    // as an equal share holds their labels.
    let compact = size.w < COMPACT_WIDTH;
    let reference_width = (size.w as f32 * 300.0 / dpi).round() as i32;
    let content_width = (reference_width - 2 * SHEET_PADDING).max(1);
    let mut m = Measure {
        text,
        density: dpi / 300.0,
    };
    // The width of a row label. A narrow panel gives the label the room its
    // text needs; a reference panel keeps the width it has always had, as
    // long as its text fits in it.
    let label_width = |m: &mut Measure, label: &str, regular: i32| -> i32 {
        let needed = m.width(label, &st.font_xs());
        if compact {
            (needed + LABEL_GUTTER).min(content_width / 2).max(1)
        } else {
            regular.max(needed + LABEL_MIN_PAD).min(content_width)
        }
    };
    // The width a chip needs for its label, and the width it would like to
    // have: the text, and the text with room around it.
    let chip_widths = |m: &mut Measure, label: &str, active: bool| -> (i32, i32) {
        let mut style = st.font_xs();
        if active {
            style = style.bold();
        }
        let w = m.width(label, &style);
        (w + CHIP_MIN_PAD, w + CHIP_GUTTER)
    };

    // 1. Top header (height 96, padding 0 88)
    let header_text = format!("{} · {}", v.author, v.title);
    let header_label = t.label(&header_text, st.font_xs().gray(INK_2));
    let header_align = t.align(0.0, 0.5, header_label);
    let header_pad = t.pad(Insets::vh(0, SHEET_PADDING), header_align);
    let top_bar = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(96),
        })
        .action(action::COLLAPSE)
        .feedback(Feedback::None),
        [header_pad],
    );

    // 2. Middle preview area
    let preview_pad_px = i32::from(margin_ref_px_clamped(v.margin_ref_px));
    let preview_title = t.label(tr(Msg::TypesetPreviewTitle), st.font_xs().gray(INK_2));
    let gap_14 = t.gap_pixels(1, 14);

    let is_trad = script_is_traditional(v.script_index);
    let p1_text = if is_trad {
        "說話周瑞家的送了劉姥姥去後，便上來回王夫人話，誰知王夫人不在上房。問丫鬟們，方知往薛姨媽那邊說話兒去了。"
    } else {
        "话说周瑞家的送了刘姥姥去后，便上来回王夫人话，谁知王夫人不在上房。问丫鬟们，方知往薛姨妈那边说话儿去了。"
    };

    let preview_font = v
        .cjk_fonts
        .get(v.selected_cjk_font)
        .map(|f| f.sample_font)
        .unwrap_or(FamilyId(0));
    let font_px = font_size_preview_px(v.font_size_half_pt);
    let leading_ratio = spacing_ratio_from_pct(v.spacing_pct);

    let mut body_style = st.font_px(font_px).line_height(leading_ratio);
    body_style.family = preview_font;

    let indent_px = (font_px as f32) * indent_em_from_tenths(v.indent_tenths);

    let mut p1_para = Paragraph::plain(p1_text, body_style, Align::Justify);
    p1_para.style.indent = indent_px;
    p1_para.style.max_lines = Some(2);
    let p1_node = t.text(p1_para);

    // A narrow panel (`size.w < COMPACT_WIDTH`) has no room for both the live
    // preview and every control: the sheet takes the screen, and the preview
    // area is kept as the empty part above it that closes the sheet. Larger
    // panels show a two-line sample paragraph so six-inch panels never clip or
    // collapse preview text behind the bottom sheet.
    let preview_area = if compact || size.w > size.h {
        t.add(
            Node::column(0)
                .flex(1)
                .action(action::COLLAPSE)
                .feedback(Feedback::None),
            [],
        )
    } else {
        let preview_content = t.add(
            Node::column(0).cross(Cross::Stretch),
            [preview_title, gap_14, p1_node],
        );
        let preview_pad = t.pad(
            Insets::new(8, preview_pad_px, 0, preview_pad_px),
            preview_content,
        );
        t.add(
            Node::column(0)
                .flex(1)
                .action(action::COLLAPSE)
                .feedback(Feedback::None),
            [preview_pad],
        )
    };

    // 3. Bottom sheet (排版设置)
    // Sheet Header: "排版" (40px) + "收起" button (72px).

    let collapse_text = tr(Msg::TypesetCollapse);
    let collapse_btn = chip_button(t, st, collapse_text, action::COLLAPSE, false, None, 72, 1);
    let collapse_pad = t.pad(Insets::vh(0, 0), collapse_btn);
    let collapse_w = 140.max((chip_widths(&mut m, collapse_text, false).1 * 6 + 4) / 5);
    let collapse_fixed = t.fixed(Some(collapse_w), Some(72), Some(collapse_pad));

    let header_spacer = t.spacer(1);
    let add_font_text = tr(Msg::ReaderFontAdd);
    let add_font = chip_button(t, st, add_font_text, action::ADD_FONT, false, None, 72, 1);
    let add_font_w = 240.max((chip_widths(&mut m, add_font_text, false).1 * 6 + 4) / 5);
    let add_font = t.fixed(Some(add_font_w), Some(72), Some(add_font));
    // Fine typesetting shapes the whole book rather than one kind of text,
    // so its switch sits in the header with the sheet's own buttons.
    let fine_text = tr(Msg::TypesetFine);
    let fine_label = t.label(fine_text, st.font_xs().gray(INK_2));
    let fine_label_gap = t.gap(12, 1);
    let fine_on_text = tr(Msg::TypesetOn);
    let fine_off_text = tr(Msg::TypesetOff);
    let fine_on_w = SMALL_CHIP_W.max(chip_widths(&mut m, fine_on_text, true).1);
    let fine_off_w = SMALL_CHIP_W.max(chip_widths(&mut m, fine_off_text, false).1);
    let fine_on = chip_button(
        t,
        st,
        fine_on_text,
        action::FINE_BASE,
        v.fine_typesetting,
        None,
        72,
        1,
    );
    let fine_on = t.fixed(Some(fine_on_w), Some(72), Some(fine_on));
    let fine_chip_gap = t.gap(CHIP_GAP, 1);
    let fine_off = chip_button(
        t,
        st,
        fine_off_text,
        action::FINE_BASE + 1,
        !v.fine_typesetting,
        None,
        72,
        1,
    );
    let fine_off = t.fixed(Some(fine_off_w), Some(72), Some(fine_off));
    let header_gap = t.gap(16, 1);
    let header_gap_2 = t.gap(16, 1);
    // The header holds the sheet's title, the fine-typesetting switch and
    // the two buttons. Its labels are measured before the tree is built: the
    // title is the part that goes when they no longer share the line, so the
    // controls keep their width on a narrow panel.
    let title_style = st.font_base().bold();
    let title_w = m.width(tr(Msg::TypesetTitle), &title_style);
    let header_needs = title_w
        + m.width(fine_text, &st.font_xs())
        + fine_on_w
        + fine_off_w
        + add_font_w
        + collapse_w
        + FINE_LABEL_GAP
        + CHIP_GAP
        + 2 * HEADER_GAP;
    let sheet_title = (header_needs + HEADER_MIN_GAP <= content_width)
        .then(|| t.label(tr(Msg::TypesetTitle), title_style));
    let mut header_children = Vec::new();
    header_children.extend(sheet_title);
    header_children.extend([
        header_spacer,
        fine_label,
        fine_label_gap,
        fine_on,
        fine_chip_gap,
        fine_off,
        header_gap,
        add_font,
        header_gap_2,
        collapse_fixed,
    ]);
    let sheet_header_row = t.add(Node::row(0).cross(Cross::Center), header_children);
    // The space between two rows is measured in reference pixels like every
    // other dimension of the sheet. A native-pixel gap would take a larger
    // share of the height on a narrow panel, and the six-inch one has none to
    // spare (VERT-101).
    let compact_vert = compact || size.w > size.h;
    let sheet_row_gap = if compact_vert { 8 } else { 12 };
    let row_h = if compact_vert { 64 } else { 72 };
    let font_card_h = if compact_vert { 82 } else { 92 };
    let font_header_h = if compact_vert { 34 } else { 38 };
    let row_gap = |t: &mut Tree| t.gap(1, sheet_row_gap);

    // Helper: make row label (width 160px)
    let make_label = |t: &mut Tree, text: &str, width: i32| {
        let l = t.label(text, st.font_xs());
        let a = t.align(0.0, 0.5, l);
        t.fixed(Some(width), Some(row_h), Some(a))
    };

    // A compact row label above its chips: one text line tall instead of
    // the 72-pixel box a label beside the chips uses.
    let make_stacked_label = |t: &mut Tree, text: &str| {
        let l = t.label(text, st.font_xs());
        let a = t.align(0.0, 0.5, l);
        t.fixed(None, Some(font_header_h), Some(a))
    };

    let make_page_btn =
        |t: &mut Tree, st: &Styles, label: &str, action: Option<ActionId>, height: i32| {
            let fg = if action.is_some() { INK } else { INK_2 };
            let border_col = if action.is_some() { INK } else { INK_2 };
            let style = st.font_sm().bold().gray(fg);
            let text = t.label(label, style);
            let center = t.align(0.5, 0.5, text);
            let mut node = Node::new(Kind::Fixed {
                w: Some(56),
                h: Some(height),
            })
            .bg(PAPER)
            .border(2, border_col, 16);
            if let Some(act) = action {
                node = node.action(act).feedback(Feedback::Invert);
            }
            t.add(node, [center])
        };

    // Row 0: 方向 (横排 / 竖排) + 书内字体 (开 / 关). They share the top row
    // when both fit, and stack on narrow panels so no label clips.
    let writing_text = tr(Msg::TypesetWritingMode);
    let writing_label_w = label_width(&mut m, writing_text, 0);
    let writing_names = [tr(Msg::TypesetHorizontal), tr(Msg::TypesetVertical)];
    let writing = writing_mode_index(v.writing_mode);
    let (writing_hard, writing_pref) = chip_group_widths(&mut m, st, &writing_names, writing);

    let embed_text = tr(Msg::TypesetEmbeddedFonts);
    let embed_label_w = label_width(&mut m, embed_text, 0);
    let embed_names = [tr(Msg::TypesetOn), tr(Msg::TypesetOff)];
    let embed_idx = if v.embedded_fonts { 0 } else { 1 };
    let (embed_hard, embed_pref) = chip_group_widths(&mut m, st, &embed_names, embed_idx);

    let row0_gap = if compact { 8 } else { 28 };
    let row0_label_gap = if compact { 6 } else { 10 };
    let row0_chip_gap = if compact { 8 } else { CHIP_GAP };
    let row0_avail = content_width - row0_gap;
    let writing_min =
        writing_label_w + row0_label_gap + writing_hard.iter().sum::<i32>() + row0_chip_gap;
    let embed_min = embed_label_w + row0_label_gap + embed_hard.iter().sum::<i32>() + row0_chip_gap;
    let (writing_col_w, embed_col_w, w_flex, e_flex) = if !compact {
        let w = (row0_avail * 13) / 20;
        (w, row0_avail - w, 13u16, 7u16)
    } else if writing_min + embed_min <= row0_avail {
        let extra = row0_avail - writing_min - embed_min;
        let w = writing_min + (extra * 4) / 5;
        let e = row0_avail - w;
        (
            w,
            e,
            w.clamp(1, u16::MAX as i32) as u16,
            e.clamp(1, u16::MAX as i32) as u16,
        )
    } else {
        let total = (writing_min + embed_min).max(1);
        let w = ((row0_avail * writing_min) / total + 8)
            .min(row0_avail - embed_min.min(row0_avail / 2));
        let e = row0_avail - w;
        (
            w,
            e,
            w.clamp(1, u16::MAX as i32) as u16,
            e.clamp(1, u16::MAX as i32) as u16,
        )
    };
    let writing_share_free = writing_col_w - writing_label_w - row0_label_gap - row0_chip_gap;
    let embed_share_free = embed_col_w - embed_label_w - row0_label_gap - row0_chip_gap;
    let writing_fit = chip_weights(
        &writing_hard,
        &writing_pref,
        writing_share_free,
        2,
        !compact,
    )
    .or_else(|| {
        let hard_sum: i32 = writing_hard.iter().sum();
        (hard_sum <= writing_share_free).then(|| {
            writing_hard
                .iter()
                .map(|&w| w.clamp(1, u16::MAX as i32) as u16)
                .collect()
        })
    });
    let embed_fit =
        chip_weights(&embed_hard, &embed_pref, embed_share_free, 2, !compact).or_else(|| {
            let hard_sum: i32 = embed_hard.iter().sum();
            (hard_sum <= embed_share_free).then(|| {
                embed_hard
                    .iter()
                    .map(|&w| w.clamp(1, u16::MAX as i32) as u16)
                    .collect()
            })
        });
    let row0_one_line = writing_fit.is_some() && embed_fit.is_some();

    let (writing_weights, embed_weights) = match (writing_fit, embed_fit) {
        (Some(w), Some(e)) => (w, e),
        _ => {
            let w_free = content_width - writing_label_w - 20 - CHIP_GAP;
            let e_free = content_width - embed_label_w - 20 - CHIP_GAP;
            (
                chip_weights(&writing_hard, &writing_pref, w_free, 2, !compact)
                    .unwrap_or_else(|| vec![1; 2]),
                chip_weights(&embed_hard, &embed_pref, e_free, 2, !compact)
                    .unwrap_or_else(|| vec![1; 2]),
            )
        }
    };
    let writing_buttons: Vec<NodeId> = writing_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::WRITING_MODE_BASE + i as ActionId,
                i == writing,
                None,
                row_h,
                writing_weights[i],
            )
        })
        .collect();
    let writing_chips = t.add(Node::row(row0_chip_gap).flex(1), writing_buttons);

    let embed_buttons: Vec<NodeId> = embed_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::EMBEDDED_FONTS_BASE + i as ActionId,
                i == embed_idx,
                None,
                row_h,
                embed_weights[i],
            )
        })
        .collect();
    let embed_chips = t.add(Node::row(row0_chip_gap).flex(1), embed_buttons);

    let writing_row = if row0_one_line {
        let w_label = make_label(t, writing_text, writing_label_w);
        let e_label = make_label(t, embed_text, embed_label_w);
        let col_w = t.add(
            Node::row(row0_label_gap).cross(Cross::Center).flex(w_flex),
            [w_label, writing_chips],
        );
        let col_e = t.add(
            Node::row(row0_label_gap).cross(Cross::Center).flex(e_flex),
            [e_label, embed_chips],
        );
        t.add(Node::row(row0_gap).cross(Cross::Center), [col_w, col_e])
    } else {
        let writing_stacked = chip_weights(
            &writing_hard,
            &writing_pref,
            content_width - writing_label_w - 20 - CHIP_GAP,
            2,
            true,
        )
        .is_none();
        let w_line = if writing_stacked {
            let label = make_stacked_label(t, writing_text);
            t.add(
                Node::column(8).cross(Cross::Stretch),
                [label, writing_chips],
            )
        } else {
            let label = make_label(t, writing_text, writing_label_w);
            t.add(Node::row(20).cross(Cross::Center), [label, writing_chips])
        };
        let embed_stacked = chip_weights(
            &embed_hard,
            &embed_pref,
            content_width - embed_label_w - 20 - CHIP_GAP,
            2,
            true,
        )
        .is_none();
        let e_line = if embed_stacked {
            let label = make_stacked_label(t, embed_text);
            t.add(Node::column(8).cross(Cross::Stretch), [label, embed_chips])
        } else {
            let label = make_label(t, embed_text, embed_label_w);
            t.add(Node::row(20).cross(Cross::Center), [label, embed_chips])
        };
        t.add(
            Node::column(STACK_GAP).cross(Cross::Stretch),
            [w_line, e_line],
        )
    };

    // 恢复 0.3.5 行为：即使开启精排和书内嵌字体，字体选择与行距等排版控件也始终开放给用户自由修改。
    let _ = (v.fine_locks_fonts, v.fine_locks_spacing);
    let fonts_locked_by_fine = false;
    let spacing_locked_by_fine = false;

    // The width the font chips need for their name and their sample.
    let font_chip_widths = |m: &mut Measure,
                            fonts: &[FontOption],
                            sample: &str,
                            selected: usize,
                            start: usize,
                            localize: bool|
     -> (Vec<i32>, Vec<i32>) {
        let mut hard = Vec::new();
        let mut pref = Vec::new();
        for (i, font) in fonts.iter().enumerate() {
            let mut name_style = st.font_xs();
            if start + i == selected {
                name_style = name_style.bold();
            }
            let mut sample_style = st.font_xs();
            sample_style.family = font.sample_font;
            let shown_name = if localize {
                display_font_name(&font.name)
            } else {
                &font.name
            };
            let w = m
                .width(shown_name, &name_style)
                .max(m.width(sample, &sample_style));
            hard.push(w);
            pref.push(w + FONT_CHIP_PADDING);
        }
        (hard, pref)
    };

    // Row 1: 中文字体 (full-width scrollable strip with left/right arrows when > 1 page,
    // or "精排内置，不可修改" when locked by publisher embedded fonts).
    let cjk_label_text = tr(Msg::MenuFontCjk);
    let cjk_row = if fonts_locked_by_fine {
        let label_w = label_width(&mut m, cjk_label_text, 160);
        let label = make_label(t, cjk_label_text, label_w);
        let badge = fine_locked_badge(t, st, 72);
        t.add(Node::row(20).cross(Cross::Center), [label, badge])
    } else {
        let cjk_sample = tr(Msg::TypesetSampleCjk);
        let total_pages = v.cjk_fonts.len().max(1).div_ceil(FONTS_PER_PAGE);
        let cjk_page = v.cjk_page.min(total_pages - 1);
        let cjk_start = cjk_page * FONTS_PER_PAGE;
        let cjk_end = (cjk_start + FONTS_PER_PAGE).min(v.cjk_fonts.len());
        let shown_cjk = cjk_end.saturating_sub(cjk_start);
        let cjk_multi = v.cjk_fonts.len() > FONTS_PER_PAGE;
        let cjk_has_prev = cjk_multi && cjk_page > 0;
        let cjk_has_next = cjk_multi && cjk_end < v.cjk_fonts.len();
        let (cjk_hard, cjk_pref) = font_chip_widths(
            &mut m,
            &v.cjk_fonts[cjk_start.min(v.cjk_fonts.len())..cjk_end.min(v.cjk_fonts.len())],
            cjk_sample,
            v.selected_cjk_font,
            cjk_start,
            true,
        );
        let arrow_w = if cjk_multi { 2 * 56 } else { 0 };
        let gaps = 14 * (FONTS_PER_PAGE as i32 + if cjk_multi { 2 } else { 0 } - 1);
        let free = (content_width - arrow_w - gaps).max(1);
        let cjk_weights = chip_weights(&cjk_hard, &cjk_pref, free, FONTS_PER_PAGE as i32, true)
            .unwrap_or_else(|| vec![1; shown_cjk]);
        let mut cjk_row_children = Vec::new();
        if cjk_multi {
            cjk_row_children.push(make_page_btn(
                t,
                st,
                "‹",
                cjk_has_prev.then_some(action::PREV_CJK_PAGE),
                font_card_h,
            ));
        }
        for (slot, (i, font)) in v
            .cjk_fonts
            .iter()
            .enumerate()
            .skip(cjk_start)
            .take(shown_cjk)
            .enumerate()
        {
            let active = i == v.selected_cjk_font;
            cjk_row_children.push(font_chip_button(
                t,
                st,
                display_font_name(&font.name),
                cjk_sample,
                font.sample_font,
                action::CJK_FONT_BASE + i as ActionId,
                active,
                font_card_h,
                cjk_weights[slot],
            ));
        }
        for _ in shown_cjk..FONTS_PER_PAGE {
            cjk_row_children.push(t.spacer(1));
        }
        if cjk_multi {
            cjk_row_children.push(make_page_btn(
                t,
                st,
                "›",
                cjk_has_next.then_some(action::NEXT_CJK_PAGE),
                font_card_h,
            ));
        }
        let header = if cjk_multi {
            let lbl = t.label(cjk_label_text, st.font_xs());
            let sp = t.spacer(1);
            let pager = t.label(
                &format!("{} / {}", cjk_page + 1, total_pages),
                st.font_xs().gray(INK_2),
            );
            let row = t.add(Node::row(8).cross(Cross::Center), [lbl, sp, pager]);
            t.fixed(None, Some(font_header_h), Some(row))
        } else {
            make_stacked_label(t, cjk_label_text)
        };
        let strip = t.add(Node::row(14).cross(Cross::Center), cjk_row_children);
        t.add(Node::column(6).cross(Cross::Stretch), [header, strip])
    };

    // Row 2: 西文字体 (same full-width scrollable strip with left/right arrows).
    let latin_label_text = tr(Msg::MenuFontLatin);
    let latin_row = if fonts_locked_by_fine {
        let label_w = label_width(&mut m, latin_label_text, 160);
        let label = make_label(t, latin_label_text, label_w);
        let badge = fine_locked_badge(t, st, 72);
        t.add(Node::row(20).cross(Cross::Center), [label, badge])
    } else {
        let latin_sample = tr(Msg::TypesetSampleLatin);
        let total_pages = v.latin_fonts.len().max(1).div_ceil(FONTS_PER_PAGE);
        let latin_page = v.latin_page.min(total_pages - 1);
        let latin_start = latin_page * FONTS_PER_PAGE;
        let latin_end = (latin_start + FONTS_PER_PAGE).min(v.latin_fonts.len());
        let shown_latin = latin_end.saturating_sub(latin_start);
        let latin_multi = v.latin_fonts.len() > FONTS_PER_PAGE;
        let latin_has_prev = latin_multi && latin_page > 0;
        let latin_has_next = latin_multi && latin_end < v.latin_fonts.len();
        let (latin_hard, latin_pref) = font_chip_widths(
            &mut m,
            &v.latin_fonts
                [latin_start.min(v.latin_fonts.len())..latin_end.min(v.latin_fonts.len())],
            latin_sample,
            v.selected_latin_font,
            latin_start,
            false,
        );
        let arrow_w = if latin_multi { 2 * 56 } else { 0 };
        let gaps = 14 * (FONTS_PER_PAGE as i32 + if latin_multi { 2 } else { 0 } - 1);
        let free = (content_width - arrow_w - gaps).max(1);
        let latin_weights =
            chip_weights(&latin_hard, &latin_pref, free, FONTS_PER_PAGE as i32, true)
                .unwrap_or_else(|| vec![1; shown_latin]);
        let mut latin_row_children = Vec::new();
        if latin_multi {
            latin_row_children.push(make_page_btn(
                t,
                st,
                "‹",
                latin_has_prev.then_some(action::PREV_LATIN_PAGE),
                font_card_h,
            ));
        }
        for (slot, (i, font)) in v
            .latin_fonts
            .iter()
            .enumerate()
            .skip(latin_start)
            .take(shown_latin)
            .enumerate()
        {
            let active = i == v.selected_latin_font;
            latin_row_children.push(font_chip_button(
                t,
                st,
                &font.name,
                latin_sample,
                font.sample_font,
                action::LATIN_FONT_BASE + i as ActionId,
                active,
                font_card_h,
                latin_weights[slot],
            ));
        }
        for _ in shown_latin..FONTS_PER_PAGE {
            latin_row_children.push(t.spacer(1));
        }
        if latin_multi {
            latin_row_children.push(make_page_btn(
                t,
                st,
                "›",
                latin_has_next.then_some(action::NEXT_LATIN_PAGE),
                font_card_h,
            ));
        }
        let header = if latin_multi {
            let lbl = t.label(latin_label_text, st.font_xs());
            let sp = t.spacer(1);
            let pager = t.label(
                &format!("{} / {}", latin_page + 1, total_pages),
                st.font_xs().gray(INK_2),
            );
            let row = t.add(Node::row(8).cross(Cross::Center), [lbl, sp, pager]);
            t.fixed(None, Some(font_header_h), Some(row))
        } else {
            make_stacked_label(t, latin_label_text)
        };
        let strip = t.add(Node::row(14).cross(Cross::Center), latin_row_children);
        t.add(Node::column(6).cross(Cross::Stretch), [header, strip])
    };

    // Shared two-column grid metrics: short controls share two equal columns
    // separated by `col_gap`; long controls span both columns.
    let col_gap = if compact { 16 } else { 28 };
    let half_w = (content_width - col_gap) / 2;
    let stepper_w = 2 * SMALL_CHIP_W + STEP_VALUE_W + 2 * STEP_GAP;

    // Helper: pair two numeric stepper controls into a 1:1 two-column row when
    // both fit, stacking cleanly into two full-width rows on narrow/verbose locales.
    let pair_two_cols = |t: &mut Tree,
                         l_label: NodeId,
                         l_ctrl: NodeId,
                         l_min_w: i32,
                         r_label: NodeId,
                         r_ctrl: NodeId,
                         r_min_w: i32|
     -> NodeId {
        let one_line = l_min_w + col_gap + r_min_w + BUDGET_SLACK <= content_width;
        if one_line {
            let (l_flex, r_flex) =
                if l_min_w + BUDGET_SLACK <= half_w && r_min_w + BUDGET_SLACK <= half_w {
                    (1u16, 1u16)
                } else {
                    (
                        l_min_w.clamp(1, u16::MAX as i32) as u16,
                        r_min_w.clamp(1, u16::MAX as i32) as u16,
                    )
                };
            let sp_l = t.spacer(1);
            let sp_r = t.spacer(1);
            let left_col = t.add(
                Node::row(0).cross(Cross::Center).flex(l_flex),
                [l_label, sp_l, l_ctrl],
            );
            let right_col = t.add(
                Node::row(0).cross(Cross::Center).flex(r_flex),
                [r_label, sp_r, r_ctrl],
            );
            t.add(
                Node::row(col_gap).cross(Cross::Center),
                [left_col, right_col],
            )
        } else {
            let sp_l = t.spacer(1);
            let sp_r = t.spacer(1);
            let left_line = t.add(Node::row(0).cross(Cross::Center), [l_label, sp_l, l_ctrl]);
            let right_line = t.add(Node::row(0).cross(Cross::Center), [r_label, sp_r, r_ctrl]);
            t.add(
                Node::column(STACK_GAP).cross(Cross::Stretch),
                [left_line, right_line],
            )
        }
    };

    // Row 3 (full-width, right under fonts): 字重 (细 / 正常 / 粗 + ±25 stepper).
    let weight_text = tr(Msg::TypesetWeight);
    let weight_label_w = label_width(&mut m, weight_text, 120);
    let weight_names = [
        tr(Msg::TypesetWeightLight),
        tr(Msg::TypesetWeightNormal),
        tr(Msg::TypesetWeightBold),
    ];
    let weight = weight_clamped(v.font_weight);
    let weight_active = (0..weight_names.len())
        .find(|&i| weight_preset_active(weight, i))
        .unwrap_or(usize::MAX);
    let (weight_hard, weight_pref) = chip_group_widths(&mut m, st, &weight_names, weight_active);
    let weight_group_gap = if compact { 16 } else { WEIGHT_GROUP_GAP };
    let weight_value_w = if compact { 88 } else { STEP_VALUE_W };
    let weight_stepper_w = 2 * SMALL_CHIP_W + weight_value_w + 2 * STEP_GAP;
    let weight_beside =
        content_width - weight_label_w - 16 - weight_stepper_w - weight_group_gap - 2 * CHIP_GAP;
    let weight_stacked = chip_weights(&weight_hard, &weight_pref, weight_beside, 3, true).is_none();
    let weight_free = if weight_stacked {
        content_width - weight_stepper_w - weight_group_gap - 2 * CHIP_GAP
    } else {
        weight_beside
    };
    let weight_weights = chip_weights(&weight_hard, &weight_pref, weight_free, 3, true)
        .unwrap_or_else(|| vec![1; 3]);
    let weight_buttons: Vec<NodeId> = weight_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::WEIGHT_BASE + i as ActionId,
                weight_preset_active(weight, i),
                None,
                row_h,
                weight_weights[i],
            )
        })
        .collect();
    let weight_chips = t.add(Node::row(CHIP_GAP).flex(1), weight_buttons);
    let weight_stepper = numeric_stepper(
        t,
        st,
        &weight.to_string(),
        action::WEIGHT_DOWN,
        action::WEIGHT_UP,
        weight_value_w,
        row_h,
    );
    let weight_row = if weight_stacked {
        let weight_controls = t.add(
            Node::row(weight_group_gap).cross(Cross::Center),
            [weight_chips, weight_stepper],
        );
        let label = make_stacked_label(t, weight_text);
        t.add(
            Node::column(8).cross(Cross::Stretch),
            [label, weight_controls],
        )
    } else {
        let weight_controls = t.add(
            Node::row(weight_group_gap).cross(Cross::Center).flex(1),
            [weight_chips, weight_stepper],
        );
        let label = make_label(t, weight_text, weight_label_w);
        t.add(Node::row(16).cross(Cross::Center), [label, weight_controls])
    };

    // Row 4 (2 columns): 字号 (0.5pt stepper) | 行距 (5% stepper).
    let size_text = tr(Msg::MenuSize);
    let size_label_w = label_width(&mut m, size_text, 120);
    let size_label = make_label(t, size_text, size_label_w);
    let size_value_str = format_font_size_half_pt(v.font_size_half_pt);
    let size_stepper = numeric_stepper(
        t,
        st,
        &size_value_str,
        action::SIZE_DOWN,
        action::SIZE_UP,
        STEP_VALUE_W,
        row_h,
    );

    let spacing_text = tr(Msg::MenuSpacing);
    let spacing_label_w = label_width(&mut m, spacing_text, 120);
    let spacing_label = make_label(t, spacing_text, spacing_label_w);
    let spacing_ctrl_min_w = if spacing_locked_by_fine {
        m.width(tr(Msg::TypesetFineLocked), &st.font_xs()) + 32
    } else {
        stepper_w
    };
    let spacing_ctrl = if spacing_locked_by_fine {
        fine_locked_badge(t, st, row_h)
    } else {
        let spacing_str = format_spacing_pct(v.spacing_pct);
        numeric_stepper(
            t,
            st,
            &spacing_str,
            action::SPACING_DOWN,
            action::SPACING_UP,
            STEP_VALUE_W,
            row_h,
        )
    };
    let size_row = pair_two_cols(
        t,
        size_label,
        size_stepper,
        size_label_w + stepper_w,
        spacing_label,
        spacing_ctrl,
        spacing_label_w + spacing_ctrl_min_w,
    );

    // Row 5 (2 columns): 页边距 (5px stepper) | 段间距 (0.1em stepper).
    let margin_text = tr(Msg::MenuMargins);
    let margin_label_w = label_width(&mut m, margin_text, 120);
    let margin_label = make_label(t, margin_text, margin_label_w);
    let margin_str = format_margin_ref_px(v.margin_ref_px);
    let margin_stepper = numeric_stepper(
        t,
        st,
        &margin_str,
        action::MARGIN_DOWN,
        action::MARGIN_UP,
        STEP_VALUE_W,
        row_h,
    );

    let gap_text = tr(Msg::TypesetGap);
    let gap_label_w = label_width(&mut m, gap_text, 120);
    let gap_label = make_label(t, gap_text, gap_label_w);
    let gap_str = format_gap_tenths(v.gap_tenths);
    let gap_stepper = numeric_stepper(
        t,
        st,
        &gap_str,
        action::GAP_DOWN,
        action::GAP_UP,
        STEP_VALUE_W,
        row_h,
    );

    let margin_gap_row = pair_two_cols(
        t,
        margin_label,
        margin_stepper,
        margin_label_w + stepper_w,
        gap_label,
        gap_stepper,
        gap_label_w + stepper_w,
    );

    // Row 6 (2 columns when both fit): 首行缩进 (0.5em stepper) | 下划线 (无 / 实线 / 虚线).
    // C.4: 繁简转换、标点挤压、翻页动画都不做（Human 2026-11-09），
    // 界面不显示；旧 reader.json 里的字段仍照常读取。
    let indent_text = tr(Msg::TypesetIndent);
    let indent_label_w = label_width(&mut m, indent_text, 140);
    let indent_label = make_label(t, indent_text, indent_label_w);
    let indent_str = format_indent_tenths(v.indent_tenths);
    let indent_stepper = numeric_stepper(
        t,
        st,
        &indent_str,
        action::INDENT_DOWN,
        action::INDENT_UP,
        STEP_VALUE_W,
        row_h,
    );

    let underline_text = tr(Msg::TypesetUnderline);
    let underline_names = [
        tr(Msg::TypesetUnderlineOff),
        tr(Msg::TypesetUnderlineSolid),
        tr(Msg::TypesetUnderlineDotted),
    ];
    let (underline_hard, underline_pref) =
        chip_group_widths(&mut m, st, &underline_names, v.underline_index);
    let underline_compact_label_w = label_width(&mut m, underline_text, 108);
    let underline_chip_gap = if compact { 8 } else { CHIP_GAP };
    let underline_col_free = half_w - underline_compact_label_w - 10 - 2 * underline_chip_gap;
    let underline_half_weights = chip_weights(
        &underline_hard,
        &underline_pref,
        underline_col_free,
        3,
        !compact,
    );
    let indent_fits_half = indent_label_w + stepper_w + BUDGET_SLACK <= half_w;
    let indent_underline_one_line = indent_fits_half && underline_half_weights.is_some();

    let indent_underline_row = if let Some(u_weights) = indent_underline_one_line
        .then_some(underline_half_weights)
        .flatten()
    {
        let underline_buttons: Vec<NodeId> = underline_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                chip_button(
                    t,
                    st,
                    name,
                    action::UNDERLINE_BASE + i as ActionId,
                    i == v.underline_index,
                    None,
                    row_h,
                    u_weights[i],
                )
            })
            .collect();
        let underline_chips = t.add(Node::row(underline_chip_gap).flex(1), underline_buttons);
        let underline_label = make_label(t, underline_text, underline_compact_label_w);
        let sp_l = t.spacer(1);
        let col_indent = t.add(
            Node::row(0).cross(Cross::Center).flex(1),
            [indent_label, sp_l, indent_stepper],
        );
        let col_underline = t.add(
            Node::row(10).cross(Cross::Center).flex(1),
            [underline_label, underline_chips],
        );
        t.add(
            Node::row(col_gap).cross(Cross::Center),
            [col_indent, col_underline],
        )
    } else {
        let underline_label_w = label_width(&mut m, underline_text, 224);
        let underline_label = make_label(t, underline_text, underline_label_w);
        let underline_free = content_width - underline_label_w - 14 - 20;
        let u_weights = chip_weights(
            &underline_hard,
            &underline_pref,
            underline_free,
            3,
            !compact,
        )
        .unwrap_or_else(|| {
            let free = content_width - 2 * 20;
            chip_weights(&underline_hard, &underline_pref, free, 3, !compact)
                .unwrap_or_else(|| vec![1; 3])
        });
        let underline_buttons: Vec<NodeId> = underline_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                chip_button(
                    t,
                    st,
                    name,
                    action::UNDERLINE_BASE + i as ActionId,
                    i == v.underline_index,
                    None,
                    row_h,
                    u_weights[i],
                )
            })
            .collect();
        let underline_chips = t.add(Node::row(10).flex(1), underline_buttons);
        let sp_l = t.spacer(1);
        let indent_line = t.add(
            Node::row(0).cross(Cross::Center),
            [indent_label, sp_l, indent_stepper],
        );
        let underline_line = t.add(
            Node::row(14).cross(Cross::Center),
            [underline_label, underline_chips],
        );
        t.add(
            Node::column(STACK_GAP).cross(Cross::Stretch),
            [indent_line, underline_line],
        )
    };

    // 划线范围 (文字划线 / 整行划线): how wide the guide is, right beside the
    // guide's own three styles. A render option like the guide itself.
    let scope_text = tr(Msg::TypesetUnderlineScope);
    let scope_label_w = label_width(&mut m, scope_text, 224);
    let scope_names = [tr(Msg::TypesetScopeText), tr(Msg::TypesetScopeFull)];
    let scope = underline_scope_index(v.underline_scope);
    let (scope_hard, scope_pref) = chip_group_widths(&mut m, st, &scope_names, scope);
    let scope_beside = content_width - scope_label_w - 20 - CHIP_GAP;
    let scope_stacked = chip_weights(&scope_hard, &scope_pref, scope_beside, 2, true).is_none();
    let scope_weights = chip_weights(
        &scope_hard,
        &scope_pref,
        if scope_stacked {
            content_width - CHIP_GAP
        } else {
            scope_beside
        },
        2,
        true,
    )
    .unwrap_or_else(|| vec![1; 2]);
    let scope_buttons: Vec<NodeId> = scope_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::UNDERLINE_SCOPE_BASE + i as ActionId,
                i == scope,
                None,
                row_h,
                scope_weights[i],
            )
        })
        .collect();
    let scope_chips = t.add(Node::row(CHIP_GAP).flex(1), scope_buttons);
    let scope_row = if scope_stacked {
        let label = make_stacked_label(t, scope_text);
        t.add(Node::column(8).cross(Cross::Stretch), [label, scope_chips])
    } else {
        let label = make_label(t, scope_text, scope_label_w);
        t.add(Node::row(20).cross(Cross::Center), [label, scope_chips])
    };

    // Screen orientation row: Portrait / Inverted / LandscapeLeft / LandscapeRight.
    let orient_text = tr(Msg::DeviceSettingsHwOrientation);
    let orient_label_w = label_width(&mut m, orient_text, 160);
    let orient_names = if aristotle_i18n::language().starts_with("zh") {
        ["竖 0°", "倒 180°", "横 90°", "横 270°"]
    } else {
        ["0°", "180°", "90°", "270°"]
    };
    let orient_idx = v.orientation.min(orient_names.len() - 1);
    let (orient_hard, orient_pref) = chip_group_widths(&mut m, st, &orient_names, orient_idx);
    let orient_free = content_width - orient_label_w - 20 - 3 * CHIP_GAP;
    let orient_weights = chip_weights(&orient_hard, &orient_pref, orient_free, 4, !compact)
        .unwrap_or_else(|| vec![1; 4]);
    let orient_buttons: Vec<NodeId> = orient_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::ORIENTATION_BASE + i as ActionId,
                i == orient_idx,
                None,
                row_h,
                orient_weights[i],
            )
        })
        .collect();
    let orient_chips = t.add(Node::row(CHIP_GAP).flex(1), orient_buttons);
    let orient_label = make_label(t, orient_text, orient_label_w);
    let orientation_row = t.add(
        Node::row(20).cross(Cross::Center),
        [orient_label, orient_chips],
    );

    // Assemble the bottom sheet:
    // Full-width rows for long controls (header, fonts, weight, scope, orientation)
    // and balanced 2-column rows for short controls (writing/embedded, size/spacing,
    // margin/gap, indent/underline).
    let g_sheet_0 = row_gap(t);
    let g_sheet_1 = row_gap(t);
    let g_sheet_2 = row_gap(t);
    let g_sheet_3 = row_gap(t);
    let g_sheet_4 = row_gap(t);
    let g_sheet_5 = row_gap(t);
    let g_sheet_6 = row_gap(t);
    let g_sheet_7 = row_gap(t);
    let g_sheet_8 = row_gap(t);
    let sheet_content = t.add(
        Node::column(0).cross(Cross::Stretch),
        [
            sheet_header_row,
            g_sheet_0,
            writing_row,
            g_sheet_1,
            cjk_row,
            g_sheet_2,
            latin_row,
            g_sheet_3,
            weight_row,
            g_sheet_4,
            size_row,
            g_sheet_5,
            margin_gap_row,
            g_sheet_6,
            indent_underline_row,
            g_sheet_7,
            scope_row,
            g_sheet_8,
            orientation_row,
        ],
    );
    let sheet_pad = t.pad(
        Insets::new(16, SHEET_PADDING, 20, SHEET_PADDING),
        sheet_content,
    );
    let sheet_node = t.add(
        Node::column(0).bg(CARD).top_border(0, CARD, 32),
        [sheet_pad],
    );

    // Full screen layout: column of [top_bar, preview_area, sheet_node]
    let root = t.add(
        Node::column(0).cross(Cross::Stretch).bg(PAPER),
        [top_bar, preview_area, sheet_node],
    );

    let ids = TypesetIds {
        root,
        collapse: collapse_btn,
        preview: preview_area,
    };

    (root, ids)
}

/// Builds the fixed-layout / comic (CBZ, PDF) settings bottom sheet.
///
/// Replaces reflowable typography controls with fit mode, page-turn direction
/// (LTR / RTL manga), fullscreen, auto-crop, landscape spread splitting,
/// night-mode artwork preservation, and per-book screen orientation.
fn build_fixed_typeset_screen(
    t: &mut Tree,
    text: &mut TextEngine,
    st: &Styles,
    v: &TypesetView,
    size: Size,
) -> (NodeId, TypesetIds) {
    let dpi = st.density.dpi() as f32;
    let compact = size.w < COMPACT_WIDTH;
    let compact_vert = compact || size.w > size.h;
    let reference_width = (size.w as f32 * 300.0 / dpi).round() as i32;
    let content_width = (reference_width - 2 * SHEET_PADDING).max(1);
    let mut m = Measure {
        text,
        density: dpi / 300.0,
    };

    let label_width = |m: &mut Measure, label: &str, regular: i32| -> i32 {
        let needed = m.width(label, &st.font_xs());
        if compact {
            (needed + LABEL_GUTTER).min(content_width / 2).max(1)
        } else {
            regular.max(needed + LABEL_MIN_PAD).min(content_width)
        }
    };
    let chip_widths = |m: &mut Measure, label: &str, active: bool| -> (i32, i32) {
        let mut style = st.font_xs();
        if active {
            style = style.bold();
        }
        let w = m.width(label, &style);
        (w + CHIP_MIN_PAD, w + CHIP_GUTTER)
    };

    // 1. Top bar (book title, tap to collapse).
    let header_text = if v.author.is_empty() {
        v.title.clone()
    } else {
        format!("{} · {}", v.author, v.title)
    };
    let header_label = t.label(&header_text, st.font_xs().gray(INK_2));
    let header_align = t.align(0.0, 0.5, header_label);
    let header_pad = t.pad(Insets::vh(0, SHEET_PADDING), header_align);
    let top_bar = t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(96),
        })
        .action(action::COLLAPSE)
        .feedback(Feedback::None),
        [header_pad],
    );

    // 2. Empty tap-to-close area above the bottom sheet.
    let preview_area = t.add(
        Node::column(0)
            .flex(1)
            .action(action::COLLAPSE)
            .feedback(Feedback::None),
        [],
    );

    // 3. Sheet header: "漫画与版式" + "收起".
    let collapse_text = tr(Msg::TypesetCollapse);
    let collapse_btn = chip_button(t, st, collapse_text, action::COLLAPSE, false, None, 72, 1);
    let collapse_w = 140.max((chip_widths(&mut m, collapse_text, false).1 * 6 + 4) / 5);
    let collapse_fixed = t.fixed(Some(collapse_w), Some(72), Some(collapse_btn));

    let sheet_title = t.label(tr(Msg::TypesetFixedTitle), st.font_base().bold());
    let header_spacer = t.spacer(1);
    let sheet_header_row = t.add(
        Node::row(0).cross(Cross::Center),
        [sheet_title, header_spacer, collapse_fixed],
    );

    let sheet_row_gap = if compact_vert { 10 } else { 14 };
    let chip_h = if compact_vert { 64 } else { 72 };
    let header_h = if compact_vert { 34 } else { 38 };

    let make_label = |t: &mut Tree, text: &str, width: i32| {
        let l = t.label(text, st.font_xs());
        let a = t.align(0.0, 0.5, l);
        t.fixed(Some(width), Some(chip_h), Some(a))
    };
    let make_stacked_label = |t: &mut Tree, text: &str| {
        let l = t.label(text, st.font_xs());
        let a = t.align(0.0, 0.5, l);
        t.fixed(None, Some(header_h), Some(a))
    };

    // Helper: single full-width label + chip group row (stacks on narrow panels).
    let build_choice_row = |t: &mut Tree,
                            m: &mut Measure,
                            label_text: &str,
                            regular_w: i32,
                            names: &[&str],
                            active: usize,
                            base_action: ActionId|
     -> NodeId {
        let lw = label_width(m, label_text, regular_w);
        let (hard, pref) = chip_group_widths(m, st, names, active);
        let gaps = CHIP_GAP * (names.len().saturating_sub(1) as i32);
        let beside_free = content_width - lw - 20 - gaps;
        let stacked =
            chip_weights(&hard, &pref, beside_free, names.len() as i32, !compact).is_none();
        let free = if stacked {
            content_width - gaps
        } else {
            beside_free
        };
        let weights = chip_weights(&hard, &pref, free, names.len() as i32, !compact)
            .unwrap_or_else(|| vec![1; names.len()]);
        let buttons: Vec<NodeId> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                chip_button(
                    t,
                    st,
                    name,
                    base_action + i as ActionId,
                    i == active,
                    None,
                    chip_h,
                    weights[i],
                )
            })
            .collect();
        let chips = t.add(Node::row(CHIP_GAP).flex(1), buttons);
        if stacked {
            let lbl = make_stacked_label(t, label_text);
            t.add(Node::column(8).cross(Cross::Stretch), [lbl, chips])
        } else {
            let lbl = make_label(t, label_text, lw);
            t.add(Node::row(20).cross(Cross::Center), [lbl, chips])
        }
    };

    // Helper: pair of boolean (On/Off) toggles sharing one row when both fit,
    // stacking vertically on narrow panels.
    let build_toggle_pair = |t: &mut Tree,
                             m: &mut Measure,
                             left_label: &str,
                             left_on: bool,
                             left_base: ActionId,
                             right_label: &str,
                             right_on: bool,
                             right_base: ActionId|
     -> NodeId {
        let on_off = [tr(Msg::TypesetOn), tr(Msg::TypesetOff)];
        let left_idx = if left_on { 0 } else { 1 };
        let right_idx = if right_on { 0 } else { 1 };
        let lw = label_width(m, left_label, 0);
        let rw = label_width(m, right_label, 0);
        let (l_hard, l_pref) = chip_group_widths(m, st, &on_off, left_idx);
        let (r_hard, r_pref) = chip_group_widths(m, st, &on_off, right_idx);

        let pair_gap = if compact { 10 } else { 16 };
        let lbl_gap = if compact { 8 } else { 12 };
        let cg = if compact { 8 } else { CHIP_GAP };
        let col_w = (content_width - pair_gap) / 2;
        let l_free = col_w - lw - lbl_gap - cg;
        let r_free = col_w - rw - lbl_gap - cg;
        let l_fit = chip_weights(&l_hard, &l_pref, l_free, 2, !compact);
        let r_fit = chip_weights(&r_hard, &r_pref, r_free, 2, !compact);

        if let (Some(lwts), Some(rwts)) = (l_fit, r_fit) {
            let mk_chips = |t: &mut Tree, base: ActionId, active: usize, wts: &[u16]| {
                let btns: Vec<NodeId> = on_off
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        chip_button(
                            t,
                            st,
                            name,
                            base + i as ActionId,
                            i == active,
                            None,
                            chip_h,
                            wts[i],
                        )
                    })
                    .collect();
                t.add(Node::row(cg).flex(1), btns)
            };
            let l_lbl = make_label(t, left_label, lw);
            let l_chips = mk_chips(t, left_base, left_idx, &lwts);
            let col_l = t.add(
                Node::row(lbl_gap).cross(Cross::Center).flex(1),
                [l_lbl, l_chips],
            );
            let r_lbl = make_label(t, right_label, rw);
            let r_chips = mk_chips(t, right_base, right_idx, &rwts);
            let col_r = t.add(
                Node::row(lbl_gap).cross(Cross::Center).flex(1),
                [r_lbl, r_chips],
            );
            t.add(Node::row(pair_gap).cross(Cross::Center), [col_l, col_r])
        } else {
            let mk_row =
                |t: &mut Tree, m: &mut Measure, lbl_text: &str, active: usize, base: ActionId| {
                    let lw = label_width(m, lbl_text, 160);
                    let (hard, pref) = chip_group_widths(m, st, &on_off, active);
                    let free = content_width - lw - 20 - CHIP_GAP;
                    let wts =
                        chip_weights(&hard, &pref, free, 2, !compact).unwrap_or_else(|| vec![1; 2]);
                    let btns: Vec<NodeId> = on_off
                        .iter()
                        .enumerate()
                        .map(|(i, name)| {
                            chip_button(
                                t,
                                st,
                                name,
                                base + i as ActionId,
                                i == active,
                                None,
                                chip_h,
                                wts[i],
                            )
                        })
                        .collect();
                    let chips = t.add(Node::row(CHIP_GAP).flex(1), btns);
                    let lbl = make_label(t, lbl_text, lw);
                    t.add(Node::row(20).cross(Cross::Center), [lbl, chips])
                };
            let row_l = mk_row(t, m, left_label, left_idx, left_base);
            let row_r = mk_row(t, m, right_label, right_idx, right_base);
            t.add(
                Node::column(STACK_GAP).cross(Cross::Stretch),
                [row_l, row_r],
            )
        }
    };

    // Row 1: 缩放 (适屏 / 适宽 / 适高).
    let fit_names = [
        tr(Msg::TypesetFitPage),
        tr(Msg::TypesetFitWidth),
        tr(Msg::TypesetFitHeight),
    ];
    let fit_row = build_choice_row(
        t,
        &mut m,
        tr(Msg::TypesetFitMode),
        140,
        &fit_names,
        v.fit_mode.min(2),
        action::FIT_MODE_BASE,
    );

    // Row 2: 翻页方向 (左 → 右 / 右 → 左 (日漫)).
    let dir_names = [tr(Msg::TypesetDirLtr), tr(Msg::TypesetDirRtl)];
    let dir_row = build_choice_row(
        t,
        &mut m,
        tr(Msg::TypesetPageDirection),
        140,
        &dir_names,
        v.page_direction.min(1),
        action::PAGE_DIR_BASE,
    );

    // Row 3: 全屏 + 自动裁边.
    let toggle_row_1 = build_toggle_pair(
        t,
        &mut m,
        tr(Msg::TypesetFullscreen),
        v.fullscreen,
        action::FULLSCREEN_BASE,
        tr(Msg::TypesetAutoCrop),
        v.auto_crop,
        action::AUTO_CROP_BASE,
    );

    // Row 4: 双页拆分 + 夜间原色.
    let toggle_row_2 = build_toggle_pair(
        t,
        &mut m,
        tr(Msg::TypesetSplitSpread),
        v.split_spread,
        action::SPLIT_SPREAD_BASE,
        tr(Msg::TypesetNightKeepImage),
        v.keep_images_in_night,
        action::NIGHT_KEEP_IMG_BASE,
    );

    // Row 5: 屏幕方向 (按书记忆).
    let orient_names = if aristotle_i18n::language().starts_with("zh") {
        ["竖 0°", "倒 180°", "横 90°", "横 270°"]
    } else {
        ["0°", "180°", "90°", "270°"]
    };
    let orient_row = build_choice_row(
        t,
        &mut m,
        tr(Msg::DeviceSettingsHwOrientation),
        140,
        &orient_names,
        v.orientation.min(3),
        action::ORIENTATION_BASE,
    );

    let g1 = t.gap(1, sheet_row_gap);
    let g2 = t.gap(1, sheet_row_gap);
    let g3 = t.gap(1, sheet_row_gap);
    let g4 = t.gap(1, sheet_row_gap);
    let g5 = t.gap(1, sheet_row_gap);
    let sheet_content = t.add(
        Node::column(0).cross(Cross::Stretch),
        [
            sheet_header_row,
            g1,
            fit_row,
            g2,
            dir_row,
            g3,
            toggle_row_1,
            g4,
            toggle_row_2,
            g5,
            orient_row,
        ],
    );
    let sheet_pad = t.pad(
        Insets::new(16, SHEET_PADDING, 24, SHEET_PADDING),
        sheet_content,
    );
    let sheet_node = t.add(
        Node::column(0).bg(CARD).top_border(0, CARD, 32),
        [sheet_pad],
    );

    let root = t.add(
        Node::column(0).cross(Cross::Stretch).bg(PAPER),
        [top_bar, preview_area, sheet_node],
    );

    let ids = TypesetIds {
        root,
        collapse: collapse_btn,
        preview: preview_area,
    };

    (root, ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_font_is_a_visible_touch_target() {
        for lang in ["en", "zh"] {
            aristotle_i18n::with_language(lang, || {
                let mut tree = Tree::new();
                let mut text = crate::text::TextEngine::new(crate::text::Fonts::embedded());
                let (root, _) = build_typeset_screen(
                    &mut tree,
                    &mut text,
                    &Styles::new(FamilyId(0)),
                    &TypesetView::default(),
                );
                tree.set_root(root);
                let text = crate::text::TextEngine::new(crate::text::Fonts::embedded());
                let mut ui = crate::ui::Ui::new(tree, text, 1264, 1680);
                ui.layout();
                let node = (0..ui.tree.len())
                    .map(|i| NodeId(i as u32))
                    .find(|&id| ui.tree.node(id).action == Some(action::ADD_FONT))
                    .unwrap();
                let rect = ui.tree.rect(node);
                assert!(rect.w >= 64 && rect.h >= 64 && ui.bounds().contains_rect(&rect));
            });
        }
    }

    #[test]
    fn test_size_mappings() {
        assert_eq!(SIZES_PX.len(), 9);
        assert_eq!(size_px(0), 32);
        assert_eq!(size_px(3), 44);
        assert_eq!(size_px(8), 64);
        assert_eq!(size_px(99), 64); // Clamped

        assert_eq!(size_index_from_px(32), 0);
        assert_eq!(size_index_from_px(44), 3);
        assert_eq!(size_index_from_px(64), 8);
        assert_eq!(size_index_from_px(42), 2); // 40 is closer than 44
    }

    #[test]
    fn test_spacing_mappings() {
        assert_eq!(SPACING_RATIO.len(), 3);
        assert_eq!(spacing_ratio(0), 1.4);
        assert_eq!(spacing_ratio(1), 1.7);
        assert_eq!(spacing_ratio(2), 2.0);
        assert_eq!(spacing_ratio(10), 2.0);
    }

    #[test]
    fn test_margin_mappings() {
        assert_eq!(MARGINS_PX.len(), 3);
        assert_eq!(margin_px(0), 120);
        assert_eq!(margin_px(1), 88);
        assert_eq!(margin_px(2), 56);
        assert_eq!(margin_px(5), 56);

        assert_eq!(margin_index_from_px(120), 0);
        assert_eq!(margin_index_from_px(88), 1);
        assert_eq!(margin_index_from_px(56), 2);
    }

    #[test]
    fn test_chinese_typeset_mappings() {
        assert_eq!(indent_chars(0), 2);
        assert_eq!(indent_chars(1), 0);

        assert!(!script_is_traditional(0));
        assert!(script_is_traditional(1));

        assert_eq!(gap_ratio(0), 0.0);
        assert_eq!(gap_ratio(1), 0.3);
        assert_eq!(gap_ratio(2), 0.6);

        assert!(trim_is_on(0));
        assert!(!trim_is_on(1));

        // Test preview indent calculations
        let font_px = 44;
        let indent_2 = (font_px as f32) * (indent_chars(0) as f32);
        assert_eq!(indent_2, 88.0);
        let indent_0 = (font_px as f32) * (indent_chars(1) as f32);
        assert_eq!(indent_0, 0.0);
    }

    #[test]
    fn test_weight_and_scope_mappings() {
        assert_eq!(WEIGHT_PRESETS, [300, 400, 600]);
        assert_eq!(weights(), [100, 400, 900]);
        assert_eq!(WEIGHT_STEP, 25);
        assert_eq!(weight_clamped(50), 100);
        assert_eq!(weight_clamped(1000), 900);
        // The stepper moves by 25 and stops at the ends of the range.
        assert_eq!(weight_stepped(400, true), 425);
        assert_eq!(weight_stepped(400, false), 375);
        assert_eq!(weight_stepped(890, true), 900);
        assert_eq!(weight_stepped(110, false), 100);
        assert_eq!(weight_stepped(900, true), 900);
        assert_eq!(weight_stepped(100, false), 100);
        // A quick button lights only for its own weight.
        assert!(weight_preset_active(300, 0));
        assert!(weight_preset_active(400, 1));
        assert!(weight_preset_active(600, 2));
        assert!(!weight_preset_active(425, 0));
        assert!(!weight_preset_active(425, 1));
        assert!(!weight_preset_active(400, 9));

        // The guide goes under the text unless the whole line is chosen.
        assert_eq!(underline_scope_index(0), 0);
        assert_eq!(underline_scope_index(1), 1);
        assert_eq!(underline_scope_index(9), 1);
        assert!(!underline_is_full_line(0));
        assert!(underline_is_full_line(1));
        assert!(underline_is_full_line(7));
        assert_eq!(UNDERLINE_SCOPE_RANGE, 2);
        assert_eq!(TypesetView::default().underline_scope, 0);
        assert!(TypesetView::default().fine_typesetting);
        assert_eq!(TypesetView::default().font_weight, WEIGHT_NORMAL);
    }

    #[test]
    fn test_koreader_stepper_precision_and_formatting() {
        // Font size: 0.5pt step, 6.0pt..=19.0pt
        assert_eq!(
            format_font_size_half_pt(FONT_SIZE_DEFAULT_HALF_PT),
            "10.0pt"
        );
        assert_eq!(font_size_half_pt_stepped(20, true), 21);
        assert_eq!(format_font_size_half_pt(21), "10.5pt");
        assert_eq!(font_size_half_pt_stepped(12, false), 12);
        assert_eq!(font_size_half_pt_stepped(38, true), 38);

        // Line spacing: 5% step, 0%..=300%
        assert_eq!(format_spacing_pct(SPACING_DEFAULT_PCT), "170%");
        assert_eq!(spacing_pct_stepped(170, true), 175);
        assert_eq!(spacing_pct_stepped(170, false), 165);
        assert_eq!(spacing_pct_stepped(0, false), 0);
        assert_eq!(spacing_pct_stepped(300, true), 300);

        // Page margins: 5px step, 0px..=200px
        assert_eq!(format_margin_ref_px(MARGIN_DEFAULT_PX), "60px");
        assert_eq!(margin_ref_px_stepped(60, true), 65);
        assert_eq!(margin_ref_px_stepped(60, false), 55);
        assert_eq!(margin_ref_px_stepped(0, false), 0);
        assert_eq!(margin_ref_px_stepped(200, true), 200);

        // First-line indent: 0.5em step, 0.0em..=4.0em
        assert_eq!(format_indent_tenths(INDENT_DEFAULT_TENTHS), "2.0em");
        assert_eq!(indent_tenths_stepped(20, true), 25);
        assert_eq!(format_indent_tenths(25), "2.5em");
        assert_eq!(indent_tenths_stepped(0, false), 0);
        assert_eq!(indent_tenths_stepped(40, true), 40);

        // Paragraph gap: 0.1em step, 0.0em..=2.0em
        assert_eq!(format_gap_tenths(GAP_DEFAULT_TENTHS), "0.6em");
        assert_eq!(gap_tenths_stepped(6, true), 7);
        assert_eq!(format_gap_tenths(7), "0.7em");
        assert_eq!(gap_tenths_stepped(0, false), 0);
        assert_eq!(gap_tenths_stepped(20, true), 20);
    }

    /// The ends of the weight range, for the clamping test.
    fn weights() -> [u16; 3] {
        [WEIGHT_MIN, WEIGHT_NORMAL, WEIGHT_MAX]
    }

    #[test]
    fn test_writing_mode_mapping() {
        // Two directions, whatever the stored setting holds.
        assert_eq!(writing_mode_index(0), 0);
        assert_eq!(writing_mode_index(1), 1);
        assert_eq!(writing_mode_index(7), 1);
        assert_eq!(action::WRITING_MODE_BASE, 790);
        assert_eq!(TypesetView::default().writing_mode, 0);
    }

    #[test]
    fn test_chip_style() {
        let (border_w, _border_col, bg_col, fg_col, is_bold) = chip_style(true);
        assert_eq!(border_w, 0);
        assert_eq!(bg_col, INK);
        assert_eq!(fg_col, PAPER);
        assert!(is_bold);

        let (border_w, border_col, bg_col, fg_col, is_bold) = chip_style(false);
        assert_eq!(border_w, 2);
        assert_eq!(border_col, INK);
        assert_eq!(bg_col, PAPER);
        assert_eq!(fg_col, INK);
        assert!(!is_bold);
    }

    #[test]
    fn test_build_typeset_screen_constructs_valid_tree() {
        let mut t = Tree::new();
        let st = Styles::new(FamilyId(0));
        let view = TypesetView::default();
        let mut text = crate::text::TextEngine::new(crate::text::Fonts::embedded());
        let (root, ids) = build_typeset_screen(&mut t, &mut text, &st, &view);
        assert_eq!(root, ids.root);
        assert!(!t.node(ids.collapse).children.is_empty());

        t.set_root(root);
        let mut text = crate::text::TextEngine::new(crate::text::Fonts::embedded());
        t.layout(crate::geom::Rect::new(0, 0, 1264, 1680), &mut text);

        let top_rect = t.rect(t.node(root).children[0]);
        let preview_rect = t.rect(ids.preview);
        let sheet_rect = t.rect(t.node(root).children[2]);

        assert_eq!(top_rect.h, 96);
        // The live preview keeps a fifth of the tallest panel even in the
        // language with the most stacked rows (VERT-101).
        assert!(preview_rect.h > 300);
        assert!(sheet_rect.h > 500);
        assert_eq!(top_rect.h + preview_rect.h + sheet_rect.h, 1680);
    }

    #[test]
    fn test_display_font_name() {
        aristotle_i18n::with_language("zh", || {
            assert_eq!(display_font_name("Noto Sans SC"), "思源黑体");
            assert_eq!(display_font_name("Noto Serif SC"), "思源宋体");
            assert_eq!(display_font_name("Literata"), "Literata");
        });
        aristotle_i18n::with_language("en", || {
            assert_eq!(display_font_name("Noto Sans SC"), "Noto Sans SC");
        });
    }
}
