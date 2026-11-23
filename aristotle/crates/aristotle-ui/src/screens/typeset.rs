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

/// Available body font sizes in pixels (9 steps from 32 to 64).
pub const SIZES_PX: [i32; 9] = [32, 36, 40, 44, 48, 52, 56, 60, 64];

/// Line spacing / pitch ratios (紧, 标准, 疏).
pub const SPACING_RATIO: [f32; 3] = [1.4, 1.7, 2.0];

/// Page margin options in pixels (宽, 标准, 窄).
pub const MARGINS_PX: [i32; 3] = [120, 88, 56];

/// Paragraph gap ratios (无: 0.0, 小: 0.3, 大: 0.6).
pub const GAP_RATIO: [f32; 3] = [0.0, 0.3, 0.6];

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
    /// Font size selection: `SIZE_BASE + i` (i in 0..9).
    pub const SIZE_BASE: ActionId = 650;
    /// Line spacing selection: `SPACING_BASE + i` (i in 0..3).
    pub const SPACING_BASE: ActionId = 660;
    /// Margin selection: `MARGIN_BASE + i` (i in 0..3).
    pub const MARGIN_BASE: ActionId = 670;
    /// First-line indent: `INDENT_BASE + i` (0: 2 字, 1: 无).
    pub const INDENT_BASE: ActionId = 680;
    /// Script conversion: `SCRIPT_BASE + i` (0: 简体, 1: 繁体).
    pub const SCRIPT_BASE: ActionId = 690;
    /// Paragraph gap: `GAP_BASE + i` (0: 无, 1: 小, 2: 大).
    pub const GAP_BASE: ActionId = 700;
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
    /// One step (50) lighter.
    pub const WEIGHT_DOWN: ActionId = 773;
    /// One step (50) heavier.
    pub const WEIGHT_UP: ActionId = 774;
    /// How wide the underline is: `UNDERLINE_SCOPE_BASE + i` (0 text only,
    /// 1 full line).
    pub const UNDERLINE_SCOPE_BASE: ActionId = 780;
    /// Writing direction: `WRITING_MODE_BASE + i` (0 横排 horizontal, 1 竖排
    /// vertical).
    pub const WRITING_MODE_BASE: ActionId = 790;
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
    pub spacing_index: usize,
    pub margin_index: usize,
    pub indent_index: usize,
    pub script_index: usize,
    pub gap_index: usize,
    /// 正文每行下方的引导线：0 无、1 实线、2 虚线。
    pub underline_index: usize,
    /// How wide that guide is: 0 under the text only, 1 across the whole
    /// text block.
    pub underline_scope: usize,
    /// Whether crengine's modern typesetting is on (TYPE-101, TYPE-102).
    pub fine_typesetting: bool,
    /// Body text weight as a CSS `font-weight` number (100..=900).
    pub font_weight: u16,
    pub trim_index: usize,
    pub anim_enabled: bool,
    pub anim_speed: usize,
    /// 书写方向（writing direction）：0 横排（horizontal）、1 竖排（vertical）。
    pub writing_mode: usize,
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
            size_index: 3,      // 44 px (prototype default)
            spacing_index: 1,   // 标准 (1.7)
            margin_index: 1,    // 标准 (88 px)
            indent_index: 0,    // 2 字
            script_index: 0,    // 简体
            gap_index: 2,       // 大 (0.6)
            underline_index: 0, // 无
            underline_scope: 0, // 文字划线
            fine_typesetting: true,
            font_weight: WEIGHT_NORMAL,
            trim_index: 0, // 开
            anim_enabled: false,
            anim_speed: 1,   // 标准
            writing_mode: 0, // 横排
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

/// Get line spacing multiplier for spacing index (0: 紧, 1: 标准, 2: 疏).
pub fn spacing_ratio(index: usize) -> f32 {
    SPACING_RATIO[index.min(SPACING_RATIO.len() - 1)]
}

/// Get side margin in pixels for margin index (0: 宽, 1: 标准, 2: 窄).
pub fn margin_px(index: usize) -> i32 {
    MARGINS_PX[index.min(MARGINS_PX.len() - 1)]
}

/// Find closest margin index for given pixel margin.
pub fn margin_index_from_px(px: i32) -> usize {
    MARGINS_PX
        .iter()
        .enumerate()
        .min_by_key(|(_, &m)| (m - px).abs())
        .map(|(idx, _)| idx)
        .unwrap_or(1)
}

/// Indent characters count (0: 2 字, 1: 无).
pub fn indent_chars(index: usize) -> usize {
    if index == 0 {
        2
    } else {
        0
    }
}

/// Whether script is traditional (0: 简体, 1: 繁体).
pub fn script_is_traditional(index: usize) -> bool {
    index == 1
}

/// Paragraph gap ratio (0: 无 0.0, 1: 小 0.3, 2: 大 0.6).
pub fn gap_ratio(index: usize) -> f32 {
    GAP_RATIO[index.min(GAP_RATIO.len() - 1)]
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
pub const WEIGHT_STEP: u16 = 50;

/// A weight inside the range the engine takes (100 to 900).
pub fn weight_clamped(weight: u16) -> u16 {
    weight.clamp(WEIGHT_MIN, WEIGHT_MAX)
}

/// The weight one step (50) lighter (`up` false) or heavier, at the ends of
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

/// Convert stored `page_anim` (0: 关, 1: 开·慢, 2: 开·标准, 3: 开·快) to
/// `(anim_enabled, speed_index)`.
pub fn page_anim_to_typeset(page_anim: u8) -> (bool, usize) {
    match page_anim {
        0 => (false, 1),
        1 => (true, 0),
        2 => (true, 1),
        3 => (true, 2),
        _ => (false, 1),
    }
}

/// Convert `(anim_enabled, speed_index)` to stored `page_anim`.
pub fn typeset_to_page_anim(enabled: bool, speed: usize) -> u8 {
    if !enabled {
        0
    } else {
        match speed {
            0 => 1,
            1 => 2,
            2 => 3,
            _ => 2,
        }
    }
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
/// and the width of the number between two stepper buttons, in reference
/// pixels.
const SMALL_CHIP_W: i32 = 64;
const STEP_VALUE_W: i32 = 80;

/// Gap inside a stepper, and between two chips of the same group, in
/// reference pixels.
const STEP_GAP: i32 = 8;
const CHIP_GAP: i32 = 10;

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

/// Gap between the two controls of row 4, in reference pixels.
const ROW4_GAP: i32 = 20;

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
        Node::column(8).cross(Cross::Center),
        [name_node, sample_node],
    );
    let center = t.align(0.5, 0.5, col);
    let pad = t.pad(Insets::vh(10, 14), center);

    t.add(
        Node::new(Kind::Fixed {
            w: None,
            h: Some(108),
        })
        .flex(flex.max(1))
        .bg(bg_col)
        .border(border_w, border_col, 16)
        .action(action)
        .feedback(Feedback::Invert),
        [pad],
    )
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
    let preview_pad_px = margin_px(v.margin_index);
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
    let font_px = size_px(v.size_index);
    let leading_ratio = spacing_ratio(v.spacing_index);

    let mut body_style = st.font_px(font_px).line_height(leading_ratio);
    body_style.family = preview_font;

    let indent_px = (font_px as f32) * (indent_chars(v.indent_index) as f32);

    let mut p1_para = Paragraph::plain(p1_text, body_style, Align::Justify);
    p1_para.style.indent = indent_px;
    p1_para.style.max_lines = Some(2);
    let p1_node = t.text(p1_para);

    // A narrow panel (`size.w < COMPACT_WIDTH`) has no room for both the live
    // preview and every control: the sheet takes the screen, and the preview
    // area is kept as the empty part above it that closes the sheet. Larger
    // panels show a two-line sample paragraph so six-inch panels never clip or
    // collapse preview text behind the bottom sheet.
    let preview_area = if compact {
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
    let sheet_row_gap = if compact { 16 } else { 20 };
    let row_gap = |t: &mut Tree| t.gap(1, sheet_row_gap);

    // Helper: make row label (width 160px)
    let make_label = |t: &mut Tree, text: &str, width: i32| {
        let l = t.label(text, st.font_xs());
        let a = t.align(0.0, 0.5, l);
        t.fixed(Some(width), Some(72), Some(a))
    };

    // A compact row label above its chips: one text line tall instead of
    // the 72-pixel box a label beside the chips uses.
    let make_stacked_label = |t: &mut Tree, text: &str| {
        let l = t.label(text, st.font_xs());
        let a = t.align(0.0, 0.5, l);
        t.fixed(None, Some(42), Some(a))
    };

    let make_page_btn = |t: &mut Tree, st: &Styles, label: &str, action: ActionId| {
        let (border_w, border_col, bg_col, fg_col, _) = chip_style(false);
        let style = st.font_xs().gray(fg_col);
        let text = t.label(label, style);
        let center = t.align(0.5, 0.5, text);
        t.add(
            Node::new(Kind::Fixed {
                w: Some(54),
                h: Some(108),
            })
            .bg(bg_col)
            .border(border_w, border_col, 16)
            .action(action)
            .feedback(Feedback::Invert),
            [center],
        )
    };

    // Row 0: 方向 (横排 / 竖排). It reads the whole page, so it sits at the
    // top of the sheet, right under its header.
    let writing_text = tr(Msg::TypesetWritingMode);
    let writing_label_w = label_width(&mut m, writing_text, 160);
    let writing_names = [tr(Msg::TypesetHorizontal), tr(Msg::TypesetVertical)];
    let writing = writing_mode_index(v.writing_mode);
    let (writing_hard, writing_pref) = chip_group_widths(&mut m, st, &writing_names, writing);
    let writing_beside = content_width - writing_label_w - 20 - CHIP_GAP;
    let writing_stacked =
        chip_weights(&writing_hard, &writing_pref, writing_beside, 2, true).is_none();
    let writing_weights = chip_weights(
        &writing_hard,
        &writing_pref,
        if writing_stacked {
            content_width - CHIP_GAP
        } else {
            writing_beside
        },
        2,
        true,
    )
    .unwrap_or_else(|| vec![1; 2]);
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
                72,
                writing_weights[i],
            )
        })
        .collect();
    let writing_chips = t.add(Node::row(CHIP_GAP).flex(1), writing_buttons);
    let writing_row = if writing_stacked {
        let label = make_stacked_label(t, writing_text);
        t.add(
            Node::column(8).cross(Cross::Stretch),
            [label, writing_chips],
        )
    } else {
        let label = make_label(t, writing_text, writing_label_w);
        t.add(Node::row(20).cross(Cross::Center), [label, writing_chips])
    };

    // Row 1: 中文字体
    let cjk_label_text = tr(Msg::MenuFontCjk);
    let cjk_label_w = label_width(&mut m, cjk_label_text, 160);
    let cjk_sample = tr(Msg::TypesetSampleCjk);
    let cjk_start = v.cjk_page * FONTS_PER_PAGE;
    let cjk_end = (cjk_start + FONTS_PER_PAGE).min(v.cjk_fonts.len());
    let shown_cjk = cjk_end.saturating_sub(cjk_start);
    let cjk_has_prev = v.cjk_fonts.len() > FONTS_PER_PAGE && v.cjk_page > 0;
    let cjk_has_next =
        v.cjk_fonts.len() > FONTS_PER_PAGE && (v.cjk_page + 1) * FONTS_PER_PAGE < v.cjk_fonts.len();
    // The width the font chips need for their name and their sample.
    let font_chip_widths = |m: &mut Measure,
                            fonts: &[FontOption],
                            sample: &str,
                            selected: usize,
                            start: usize|
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
            let w = m
                .width(display_font_name(&font.name), &name_style)
                .max(m.width(sample, &sample_style));
            hard.push(w);
            pref.push(w + FONT_CHIP_PADDING);
        }
        (hard, pref)
    };
    let (cjk_hard, cjk_pref) = font_chip_widths(
        &mut m,
        &v.cjk_fonts[cjk_start.min(v.cjk_fonts.len())..cjk_end.min(v.cjk_fonts.len())],
        cjk_sample,
        v.selected_cjk_font,
        cjk_start,
    );
    // The chips share the row the label leaves them. When they need more
    // than that, the label moves above them and they take the whole width.
    let cjk_page_btns = cjk_has_prev as i32 + cjk_has_next as i32;
    let cjk_gaps = 16 * (FONTS_PER_PAGE as i32 + cjk_page_btns - 1);
    let cjk_beside = content_width - cjk_label_w - 20;
    let cjk_free = cjk_beside - cjk_page_btns * 54 - cjk_gaps;
    let cjk_stacked = chip_weights(
        &cjk_hard,
        &cjk_pref,
        cjk_free,
        FONTS_PER_PAGE as i32,
        compact,
    )
    .is_none();
    let cjk_weights = if cjk_stacked {
        let free = content_width - cjk_page_btns * 54 - 16 * (shown_cjk as i32 + cjk_page_btns - 1);
        chip_weights(&cjk_hard, &cjk_pref, free, shown_cjk as i32, true)
            .unwrap_or_else(|| vec![1; shown_cjk])
    } else {
        chip_weights(
            &cjk_hard,
            &cjk_pref,
            cjk_free,
            FONTS_PER_PAGE as i32,
            compact,
        )
        .unwrap_or_else(|| vec![1; shown_cjk])
    };
    // The chips are built after their weights are known.
    let mut cjk_chips = Vec::new();
    for (slot, (i, font)) in v
        .cjk_fonts
        .iter()
        .enumerate()
        .skip(cjk_start)
        .take(shown_cjk)
        .enumerate()
    {
        let active = i == v.selected_cjk_font;
        cjk_chips.push(font_chip_button(
            t,
            st,
            display_font_name(&font.name),
            cjk_sample,
            font.sample_font,
            action::CJK_FONT_BASE + i as ActionId,
            active,
            cjk_weights[slot],
        ));
    }
    let mut cjk_row_children = Vec::new();
    if cjk_has_prev {
        cjk_row_children.push(make_page_btn(t, st, "‹", action::PREV_CJK_PAGE));
    }
    cjk_row_children.extend(cjk_chips);
    if cjk_has_next {
        cjk_row_children.push(make_page_btn(t, st, "›", action::NEXT_CJK_PAGE));
    }
    if !cjk_stacked {
        // The empty slots keep the chips in their place: they share the row
        // with the chips, so that a single chip is not stretched over it.
        for _ in shown_cjk..FONTS_PER_PAGE {
            cjk_row_children.push(t.spacer(1));
        }
    }
    let cjk_row_content = t.add(
        if cjk_stacked {
            Node::row(16)
        } else {
            Node::row(16).flex(1)
        },
        cjk_row_children,
    );
    let cjk_row = if cjk_stacked {
        let label = make_stacked_label(t, cjk_label_text);
        t.add(
            Node::column(8).cross(Cross::Stretch),
            [label, cjk_row_content],
        )
    } else {
        let label = make_label(t, cjk_label_text, cjk_label_w);
        t.add(Node::row(20).cross(Cross::Center), [label, cjk_row_content])
    };

    // Row 2: 西文字体
    let latin_label_text = tr(Msg::MenuFontLatin);
    let latin_label_w = label_width(&mut m, latin_label_text, 160);
    let latin_sample = tr(Msg::TypesetSampleLatin);
    let latin_start = v.latin_page * FONTS_PER_PAGE;
    let latin_end = (latin_start + FONTS_PER_PAGE).min(v.latin_fonts.len());
    let shown_latin = latin_end.saturating_sub(latin_start);
    let latin_has_prev = v.latin_fonts.len() > FONTS_PER_PAGE && v.latin_page > 0;
    let latin_has_next = v.latin_fonts.len() > FONTS_PER_PAGE
        && (v.latin_page + 1) * FONTS_PER_PAGE < v.latin_fonts.len();
    let (latin_hard, latin_pref) = font_chip_widths(
        &mut m,
        &v.latin_fonts[latin_start.min(v.latin_fonts.len())..latin_end.min(v.latin_fonts.len())],
        latin_sample,
        v.selected_latin_font,
        latin_start,
    );
    let latin_page_btns = latin_has_prev as i32 + latin_has_next as i32;
    let latin_gaps = 16 * (FONTS_PER_PAGE as i32 + latin_page_btns - 1);
    let latin_beside = content_width - latin_label_w - 20;
    let latin_free = latin_beside - latin_page_btns * 54 - latin_gaps;
    let latin_stacked = chip_weights(
        &latin_hard,
        &latin_pref,
        latin_free,
        FONTS_PER_PAGE as i32,
        compact,
    )
    .is_none();
    let latin_weights = if latin_stacked {
        let free =
            content_width - latin_page_btns * 54 - 16 * (shown_latin as i32 + latin_page_btns - 1);
        chip_weights(&latin_hard, &latin_pref, free, shown_latin as i32, true)
            .unwrap_or_else(|| vec![1; shown_latin])
    } else {
        chip_weights(
            &latin_hard,
            &latin_pref,
            latin_free,
            FONTS_PER_PAGE as i32,
            compact,
        )
        .unwrap_or_else(|| vec![1; shown_latin])
    };
    let mut latin_row_children = Vec::new();
    if latin_has_prev {
        latin_row_children.push(make_page_btn(t, st, "‹", action::PREV_LATIN_PAGE));
    }
    for (i, font) in v
        .latin_fonts
        .iter()
        .enumerate()
        .skip(latin_start)
        .take(shown_latin)
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
            latin_weights[i - latin_start],
        ));
    }
    if latin_has_next {
        latin_row_children.push(make_page_btn(t, st, "›", action::NEXT_LATIN_PAGE));
    }
    if !latin_stacked {
        // The empty slots keep the chips in their place: they share the row
        // with the chips, so that a single chip is not stretched over it.
        for _ in shown_latin..FONTS_PER_PAGE {
            latin_row_children.push(t.spacer(1));
        }
    }
    let latin_row_content = t.add(
        if latin_stacked {
            Node::row(16)
        } else {
            Node::row(16).flex(1)
        },
        latin_row_children,
    );
    let latin_row = if latin_stacked {
        let label = make_stacked_label(t, latin_label_text);
        t.add(
            Node::column(8).cross(Cross::Stretch),
            [label, latin_row_content],
        )
    } else {
        let label = make_label(t, latin_label_text, latin_label_w);
        t.add(
            Node::row(20).cross(Cross::Center),
            [label, latin_row_content],
        )
    };

    // Row 3: 字号 (9 steps: 32..64)
    let size_text = tr(Msg::MenuSize);
    let size_label_w = label_width(&mut m, size_text, 160);
    let size_label = make_label(t, size_text, size_label_w);
    let size_names: Vec<String> = SIZES_PX.iter().map(|px| format!("{px}")).collect();
    let (size_hard, size_pref) = chip_group_widths(&mut m, st, &size_names, v.size_index);
    let size_free = content_width - size_label_w - 20 - 8 * 10;
    let size_weights = chip_weights(
        &size_hard,
        &size_pref,
        size_free,
        size_names.len() as i32,
        compact,
    )
    .unwrap_or_else(|| vec![1; size_names.len()]);
    let mut size_buttons = Vec::new();
    for (i, name) in size_names.iter().enumerate() {
        let active = i == v.size_index;
        size_buttons.push(chip_button(
            t,
            st,
            name,
            action::SIZE_BASE + i as ActionId,
            active,
            None,
            72,
            size_weights[i],
        ));
    }
    let size_row_content = t.add(Node::row(10).flex(1), size_buttons);
    let size_row = t.add(
        Node::row(20).cross(Cross::Center),
        [size_label, size_row_content],
    );

    // 字重 (细 / 正常 / 粗 and a stepper): the three quick buttons set the
    // presets the reading panel offers, the stepper goes on in steps of 50.
    let weight_text = tr(Msg::TypesetWeight);
    let weight_label_w = label_width(&mut m, weight_text, 160);
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
    // The stepper keeps its own width; the three presets share what is left.
    let stepper_w = 2 * SMALL_CHIP_W + STEP_VALUE_W + 2 * STEP_GAP;
    let weight_beside = content_width - weight_label_w - 20 - stepper_w - 3 * CHIP_GAP;
    let weight_stacked = chip_weights(&weight_hard, &weight_pref, weight_beside, 3, true).is_none();
    let weight_free = if weight_stacked {
        content_width - stepper_w - 3 * CHIP_GAP
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
                72,
                weight_weights[i],
            )
        })
        .collect();
    let weight_chips = t.add(Node::row(CHIP_GAP).flex(1), weight_buttons);
    let weight_stepper = {
        let step = |t: &mut Tree, label: &str, action: ActionId| {
            let (border_w, border_col, bg_col, fg_col, _) = chip_style(false);
            let text = t.label(label, st.font_xs().gray(fg_col));
            let center = t.align(0.5, 0.5, text);
            t.add(
                Node::new(Kind::Fixed {
                    w: Some(SMALL_CHIP_W),
                    h: Some(72),
                })
                .bg(bg_col)
                .border(border_w, border_col, 16)
                .action(action)
                .feedback(Feedback::Invert),
                [center],
            )
        };
        // The number is a reading of the weight, not a control: only the
        // two buttons beside it are.
        let number = t.label(&weight.to_string(), st.font_xs().bold().gray(INK));
        let number = t.align(0.5, 0.5, number);
        let value = t.add(
            Node::new(Kind::Fixed {
                w: Some(STEP_VALUE_W),
                h: Some(72),
            })
            .bg(PAPER)
            .border(2, INK, 16),
            [number],
        );
        let down = step(t, "−", action::WEIGHT_DOWN);
        let up = step(t, "+", action::WEIGHT_UP);
        t.add(Node::row(STEP_GAP).cross(Cross::Center), [down, value, up])
    };
    let weight_controls = t.add(
        Node::row(CHIP_GAP).cross(Cross::Center),
        [weight_chips, weight_stepper],
    );
    let weight_row = if weight_stacked {
        let label = make_stacked_label(t, weight_text);
        t.add(
            Node::column(8).cross(Cross::Stretch),
            [label, weight_controls],
        )
    } else {
        let label = make_label(t, weight_text, weight_label_w);
        t.add(Node::row(20).cross(Cross::Center), [label, weight_controls])
    };

    // Row 4: 行距 (3) & 页边距 (3)
    let spacing_text = tr(Msg::MenuSpacing);
    let spacing_label_w = label_width(&mut m, spacing_text, 160);
    let spacing_label = make_label(t, spacing_text, spacing_label_w);
    let spacing_names = [
        tr(Msg::TypesetSpacingTight),
        tr(Msg::TypesetSpacingNormal),
        tr(Msg::TypesetSpacingLoose),
    ];
    let (spacing_hard, spacing_pref) =
        chip_group_widths(&mut m, st, &spacing_names, v.spacing_index);

    let margin_text = tr(Msg::MenuMargins);
    let margin_label_w = label_width(&mut m, margin_text, 176);
    let margin_label = make_label(t, margin_text, margin_label_w);
    let margin_names = [
        tr(Msg::TypesetMarginWide),
        tr(Msg::TypesetMarginNormal),
        tr(Msg::TypesetMarginNarrow),
    ];
    let (margin_hard, margin_pref) = chip_group_widths(&mut m, st, &margin_names, v.margin_index);

    // The two groups share one line as long as both of them fit their half
    // of the row; otherwise each takes a line of its own.
    let row4_gap = ROW4_GAP;
    let row4_free = content_width - spacing_label_w - margin_label_w - 3 * row4_gap;
    let row4_half = row4_free / 2;
    let row4_one_line = chip_weights(
        &spacing_hard,
        &spacing_pref,
        row4_half - 2 * 12,
        3,
        !compact,
    )
    .is_some()
        && chip_weights(&margin_hard, &margin_pref, row4_half - 2 * 12, 3, !compact).is_some();
    let (spacing_weights, margin_weights) = if row4_one_line {
        (
            chip_weights(
                &spacing_hard,
                &spacing_pref,
                row4_half - 2 * 12,
                3,
                !compact,
            )
            .unwrap_or_else(|| vec![1; 3]),
            chip_weights(&margin_hard, &margin_pref, row4_half - 2 * 12, 3, !compact)
                .unwrap_or_else(|| vec![1; 3]),
        )
    } else {
        let spacing_free = content_width - spacing_label_w - row4_gap - 2 * 12;
        let margin_free = content_width - margin_label_w - row4_gap - 2 * 12;
        (
            chip_weights(&spacing_hard, &spacing_pref, spacing_free, 3, !compact)
                .unwrap_or_else(|| vec![1; 3]),
            chip_weights(&margin_hard, &margin_pref, margin_free, 3, !compact)
                .unwrap_or_else(|| vec![1; 3]),
        )
    };
    let spacing_buttons: Vec<NodeId> = spacing_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::SPACING_BASE + i as ActionId,
                i == v.spacing_index,
                None,
                72,
                spacing_weights[i],
            )
        })
        .collect();
    let spacing_row_content = t.add(Node::row(12).flex(1), spacing_buttons);
    let margin_buttons: Vec<NodeId> = margin_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::MARGIN_BASE + i as ActionId,
                i == v.margin_index,
                None,
                72,
                margin_weights[i],
            )
        })
        .collect();
    let margin_row_content = t.add(Node::row(12).flex(1), margin_buttons);

    // On a narrow panel the two labels and two chip groups no longer share
    // one line with a readable gutter, so each group takes its own line.
    let row4 = if !row4_one_line {
        let spacing_line = t.add(
            Node::row(row4_gap).cross(Cross::Center),
            [spacing_label, spacing_row_content],
        );
        let margin_line = t.add(
            Node::row(row4_gap).cross(Cross::Center),
            [margin_label, margin_row_content],
        );
        t.add(
            Node::column(STACK_GAP).cross(Cross::Stretch),
            [spacing_line, margin_line],
        )
    } else {
        t.add(
            Node::row(row4_gap).cross(Cross::Center),
            [
                spacing_label,
                spacing_row_content,
                margin_label,
                margin_row_content,
            ],
        )
    };

    // Row 5: 中文排版 (首行缩进 + 段间距)
    // C.4: 繁简转换、标点挤压、翻页动画都不做（Human 2026-11-09），
    // 界面不显示；旧 reader.json 里的字段仍照常读取。
    let cjk_section_title = t.label(tr(Msg::TypesetCjkSection), st.font_xs().gray(INK_2));
    let gap_14_sub = t.gap_pixels(1, 14);

    // 首行缩进 (2 字 / 无)
    let indent_text = tr(Msg::TypesetIndent);
    let indent_label_w = label_width(&mut m, indent_text, 136);
    let indent_label = make_label(t, indent_text, indent_label_w);
    let indent_names = [tr(Msg::TypesetIndent2chars), tr(Msg::TypesetIndentNone)];

    // 段间距 (无 / 小 / 大)
    let gap_text = tr(Msg::TypesetGap);
    let gap_label_w = label_width(&mut m, gap_text, 136);
    let gap_label = make_label(t, gap_text, gap_label_w);
    let gap_names = [
        tr(Msg::TypesetGapNone),
        tr(Msg::TypesetGapSmall),
        tr(Msg::TypesetGapLarge),
    ];

    // Two controls share row 5. Each gets the share its labels and chips
    // need; when even that does not fit they stack.
    let (indent_hard, indent_pref) = chip_group_widths(&mut m, st, &indent_names, v.indent_index);
    let (gap_hard, gap_pref) = chip_group_widths(&mut m, st, &gap_names, v.gap_index);
    let row5_free = content_width - 32;
    let row5_half = row5_free / 2;
    let indent_free = row5_half - indent_label_w - 14 - 10;
    let gap_free = row5_half - gap_label_w - 14 - 20;
    let row5_one_line = chip_weights(&indent_hard, &indent_pref, indent_free, 2, !compact)
        .is_some()
        && chip_weights(&gap_hard, &gap_pref, gap_free, 3, !compact).is_some();
    let (indent_weights, gap_weights) = if row5_one_line {
        (
            chip_weights(&indent_hard, &indent_pref, indent_free, 2, !compact)
                .unwrap_or_else(|| vec![1; 2]),
            chip_weights(&gap_hard, &gap_pref, gap_free, 3, !compact).unwrap_or_else(|| vec![1; 3]),
        )
    } else {
        let indent_free = content_width - indent_label_w - 14 - 10;
        let gap_free = content_width - gap_label_w - 14 - 20;
        (
            chip_weights(&indent_hard, &indent_pref, indent_free, 2, !compact)
                .unwrap_or_else(|| vec![1; 2]),
            chip_weights(&gap_hard, &gap_pref, gap_free, 3, !compact).unwrap_or_else(|| vec![1; 3]),
        )
    };
    let indent_buttons: Vec<NodeId> = indent_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::INDENT_BASE + i as ActionId,
                i == v.indent_index,
                None,
                72,
                indent_weights[i],
            )
        })
        .collect();
    let indent_chips = t.add(Node::row(10).flex(1), indent_buttons);
    let gap_buttons: Vec<NodeId> = gap_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            chip_button(
                t,
                st,
                name,
                action::GAP_BASE + i as ActionId,
                i == v.gap_index,
                None,
                72,
                gap_weights[i],
            )
        })
        .collect();
    let gap_chips = t.add(Node::row(10).flex(1), gap_buttons);
    let cjk_row_settings = if !row5_one_line {
        let indent_line = t.add(
            Node::row(14).cross(Cross::Center),
            [indent_label, indent_chips],
        );
        let gap_line = t.add(Node::row(14).cross(Cross::Center), [gap_label, gap_chips]);
        t.add(
            Node::column(STACK_GAP).cross(Cross::Stretch),
            [indent_line, gap_line],
        )
    } else {
        let col_indent = t.add(
            Node::row(14).cross(Cross::Center).flex(1),
            [indent_label, indent_chips],
        );
        let col_gap = t.add(
            Node::row(14).cross(Cross::Center).flex(1),
            [gap_label, gap_chips],
        );
        t.add(Node::row(32).cross(Cross::Center), [col_indent, col_gap])
    };

    // 下划线 (无 / 实线 / 虚线)：正文每一行下方的引导线。
    let underline_text = tr(Msg::TypesetUnderline);
    let underline_label_w = label_width(&mut m, underline_text, 224);
    let underline_label = make_label(t, underline_text, underline_label_w);
    let underline_names = [
        tr(Msg::TypesetUnderlineOff),
        tr(Msg::TypesetUnderlineSolid),
        tr(Msg::TypesetUnderlineDotted),
    ];
    // Underline labels differ a lot between languages ("Ciągła" next to
    // "Kropkowana"), so the chips share the row in the width of their
    // labels when an equal third is too narrow for them.
    let (underline_hard, underline_pref) =
        chip_group_widths(&mut m, st, &underline_names, v.underline_index);
    let underline_free = content_width - underline_label_w - 14 - 20;
    let underline_weights = chip_weights(
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
                72,
                underline_weights[i],
            )
        })
        .collect();
    let underline_chips = t.add(Node::row(10).flex(1), underline_buttons);
    let col_underline = t.add(
        Node::row(14).cross(Cross::Center).flex(1),
        [underline_label, underline_chips],
    );
    let underline_row = t.add(Node::row(32).cross(Cross::Center), [col_underline]);

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
                72,
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

    // Assemble the bottom sheet
    let gap_24 = t.gap_pixels(1, 24);
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
            g_sheet_1,
            writing_row,
            g_sheet_2,
            cjk_row,
            g_sheet_3,
            latin_row,
            g_sheet_4,
            size_row,
            g_sheet_5,
            weight_row,
            g_sheet_6,
            row4,
            gap_24,
            cjk_section_title,
            gap_14_sub,
            cjk_row_settings,
            g_sheet_7,
            underline_row,
            g_sheet_8,
            scope_row,
        ],
    );
    let sheet_pad = t.pad(
        Insets::new(24, SHEET_PADDING, 32, SHEET_PADDING),
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
        assert_eq!(weight_clamped(50), 100);
        assert_eq!(weight_clamped(1000), 900);
        // The stepper moves by 50 and stops at the ends of the range.
        assert_eq!(weight_stepped(400, true), 450);
        assert_eq!(weight_stepped(400, false), 350);
        assert_eq!(weight_stepped(880, true), 900);
        assert_eq!(weight_stepped(120, false), 100);
        assert_eq!(weight_stepped(900, true), 900);
        assert_eq!(weight_stepped(100, false), 100);
        // A quick button lights only for its own weight.
        assert!(weight_preset_active(300, 0));
        assert!(weight_preset_active(400, 1));
        assert!(weight_preset_active(600, 2));
        assert!(!weight_preset_active(450, 0));
        assert!(!weight_preset_active(450, 1));
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
    fn test_page_anim_mappings() {
        assert_eq!(page_anim_to_typeset(0), (false, 1));
        assert_eq!(page_anim_to_typeset(1), (true, 0));
        assert_eq!(page_anim_to_typeset(2), (true, 1));
        assert_eq!(page_anim_to_typeset(3), (true, 2));

        assert_eq!(typeset_to_page_anim(false, 1), 0);
        assert_eq!(typeset_to_page_anim(true, 0), 1);
        assert_eq!(typeset_to_page_anim(true, 1), 2);
        assert_eq!(typeset_to_page_anim(true, 2), 3);
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
