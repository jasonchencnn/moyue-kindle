// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reader typeset panel according to Prototype v2 (F2, F3, X5).
//!
//! Renders the bottom sheet popup for reader typesetting settings with a live
//! text preview above it.

use crate::geom::{Insets, Size, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::text::{Align, FamilyId, Paragraph};
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
    pub trim_index: usize,
    pub anim_enabled: bool,
    pub anim_speed: usize,
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
            trim_index: 0,      // 开
            anim_enabled: false,
            anim_speed: 1, // 标准
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
    font_family: Option<FamilyId>,
) -> NodeId {
    let (border_w, border_col, bg_col, fg_col, is_bold) = chip_style(active);
    let mut style = if let Some(fam) = font_family {
        let mut s = st.font_xs();
        s.family = fam;
        s
    } else if is_bold {
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
        node = node.flex(1);
    }
    t.add(node, [center])
}

/// Builds a 2-line font selection button (font name + sample text).
fn font_chip_button(
    t: &mut Tree,
    st: &Styles,
    name: &str,
    sample: &str,
    sample_family: FamilyId,
    action: ActionId,
    active: bool,
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
        .flex(1)
        .bg(bg_col)
        .border(border_w, border_col, 16)
        .action(action)
        .feedback(Feedback::Invert),
        [pad],
    )
}

/// Builds the typeset screen tree according to `ReaderTypeset.dc.html`.
pub fn build_typeset_screen(t: &mut Tree, st: &Styles, v: &TypesetView) -> (NodeId, TypesetIds) {
    let size = Size::new(SCREEN_WIDTH, SCREEN_HEIGHT);

    // 1. Top header (height 96, padding 0 88)
    let header_text = format!("{} · {}", v.author, v.title);
    let header_label = t.label(&header_text, st.font_xs().gray(INK_2));
    let header_align = t.align(0.0, 0.5, header_label);
    let header_pad = t.pad(Insets::vh(0, 88), header_align);
    let top_bar = t.add(
        Node::new(Kind::Fixed {
            w: Some(size.w),
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
    let p2_text = if is_trad {
        "周瑞家的聽說，便出東角門，過東院，往梨香院來。剛至院門前，只見王夫人的丫鬟金釧兒站在臺階兒上玩呢。"
    } else {
        "周瑞家的听说，便出东角门，过东院，往梨香院来。刚至院门前，只见王夫人的丫鬟金钏儿站在台阶儿上玩呢。"
    };

    let preview_font = v
        .cjk_fonts
        .get(v.selected_cjk_font)
        .map(|f| f.sample_font)
        .unwrap_or(FamilyId(0));
    let font_px = size_px(v.size_index);
    let leading_ratio = spacing_ratio(v.spacing_index);
    let gap_px = ((font_px as f32) * gap_ratio(v.gap_index)).round() as i32;

    let mut body_style = st.font_px(font_px).line_height(leading_ratio);
    body_style.family = preview_font;

    let indent_px = (font_px as f32) * (indent_chars(v.indent_index) as f32);

    let mut p1_para = Paragraph::plain(p1_text, body_style.clone(), Align::Justify);
    p1_para.style.indent = indent_px;
    p1_para.style.max_lines = Some(4);
    let p1_node = t.text(p1_para);

    let mut p2_para = Paragraph::plain(p2_text, body_style, Align::Justify);
    p2_para.style.indent = indent_px;
    p2_para.style.max_lines = Some(3);
    let p2_node = t.text(p2_para);

    let p_gap = t.gap_pixels(1, gap_px.max(4));
    let preview_content = t.add(
        Node::column(0).cross(Cross::Stretch),
        [preview_title, gap_14, p1_node, p_gap, p2_node],
    );
    let preview_pad = t.pad(
        Insets::new(8, preview_pad_px, 0, preview_pad_px),
        preview_content,
    );
    let preview_area = t.add(
        Node::column(0)
            .flex(1)
            .action(action::COLLAPSE)
            .feedback(Feedback::None),
        [preview_pad],
    );

    // 3. Bottom sheet (排版设置)
    // Sheet Header: "排版" (40px) + "收起" button (72px)
    let sheet_title = t.label(tr(Msg::TypesetTitle), st.font_base().bold());

    let collapse_btn = chip_button(
        t,
        st,
        tr(Msg::TypesetCollapse),
        action::COLLAPSE,
        false,
        None,
        72,
        None,
    );
    let collapse_pad = t.pad(Insets::vh(0, 0), collapse_btn);
    let collapse_fixed = t.fixed(Some(140), Some(72), Some(collapse_pad));

    let header_spacer = t.spacer(1);
    let add_font = chip_button(
        t,
        st,
        tr(Msg::ReaderFontAdd),
        action::ADD_FONT,
        false,
        None,
        72,
        None,
    );
    let add_font = t.fixed(Some(240), Some(72), Some(add_font));
    let header_gap = t.gap(16, 1);
    let sheet_header_row = t.add(
        Node::row(0).cross(Cross::Center),
        [
            sheet_title,
            header_spacer,
            add_font,
            header_gap,
            collapse_fixed,
        ],
    );
    let gap_20 = t.gap_pixels(1, 20);

    // Helper: make row label (width 160px)
    let make_label = |t: &mut Tree, text: &str, width: i32| {
        let l = t.label(text, st.font_xs());
        let a = t.align(0.0, 0.5, l);
        t.fixed(Some(width), Some(72), Some(a))
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

    // Row 1: 中文字体
    let cjk_label = make_label(t, tr(Msg::MenuFontCjk), 160);
    let mut cjk_buttons = Vec::new();
    let cjk_sample = tr(Msg::TypesetSampleCjk);
    let cjk_start = v.cjk_page * FONTS_PER_PAGE;
    let cjk_end = (cjk_start + FONTS_PER_PAGE).min(v.cjk_fonts.len());
    if v.cjk_fonts.len() > FONTS_PER_PAGE && v.cjk_page > 0 {
        cjk_buttons.push(make_page_btn(t, st, "‹", action::PREV_CJK_PAGE));
    }
    for (i, font) in v
        .cjk_fonts
        .iter()
        .enumerate()
        .skip(cjk_start)
        .take(cjk_end.saturating_sub(cjk_start))
    {
        let active = i == v.selected_cjk_font;
        cjk_buttons.push(font_chip_button(
            t,
            st,
            display_font_name(&font.name),
            cjk_sample,
            font.sample_font,
            action::CJK_FONT_BASE + i as ActionId,
            active,
        ));
    }
    if v.cjk_fonts.len() > FONTS_PER_PAGE && (v.cjk_page + 1) * FONTS_PER_PAGE < v.cjk_fonts.len() {
        cjk_buttons.push(make_page_btn(t, st, "›", action::NEXT_CJK_PAGE));
    }
    let shown_cjk = cjk_end.saturating_sub(cjk_start);
    for _ in shown_cjk..FONTS_PER_PAGE {
        cjk_buttons.push(t.spacer(1));
    }
    let cjk_row_content = t.add(Node::row(16).flex(1), cjk_buttons);
    let cjk_row = t.add(
        Node::row(20).cross(Cross::Center),
        [cjk_label, cjk_row_content],
    );

    // Row 2: 西文字体
    let latin_label = make_label(t, tr(Msg::MenuFontLatin), 160);
    let mut latin_buttons = Vec::new();
    let latin_sample = tr(Msg::TypesetSampleLatin);
    let latin_start = v.latin_page * FONTS_PER_PAGE;
    let latin_end = (latin_start + FONTS_PER_PAGE).min(v.latin_fonts.len());
    if v.latin_fonts.len() > FONTS_PER_PAGE && v.latin_page > 0 {
        latin_buttons.push(make_page_btn(t, st, "‹", action::PREV_LATIN_PAGE));
    }
    for (i, font) in v
        .latin_fonts
        .iter()
        .enumerate()
        .skip(latin_start)
        .take(latin_end.saturating_sub(latin_start))
    {
        let active = i == v.selected_latin_font;
        latin_buttons.push(font_chip_button(
            t,
            st,
            &font.name,
            latin_sample,
            font.sample_font,
            action::LATIN_FONT_BASE + i as ActionId,
            active,
        ));
    }
    if v.latin_fonts.len() > FONTS_PER_PAGE
        && (v.latin_page + 1) * FONTS_PER_PAGE < v.latin_fonts.len()
    {
        latin_buttons.push(make_page_btn(t, st, "›", action::NEXT_LATIN_PAGE));
    }
    let shown_latin = latin_end.saturating_sub(latin_start);
    for _ in shown_latin..FONTS_PER_PAGE {
        latin_buttons.push(t.spacer(1));
    }
    let latin_row_content = t.add(Node::row(16).flex(1), latin_buttons);
    let latin_row = t.add(
        Node::row(20).cross(Cross::Center),
        [latin_label, latin_row_content],
    );

    // Row 3: 字号 (9 steps: 32..64)
    let size_label = make_label(t, tr(Msg::MenuSize), 160);
    let mut size_buttons = Vec::new();
    for (i, &px) in SIZES_PX.iter().enumerate() {
        let active = i == v.size_index;
        let s_str = format!("{px}");
        size_buttons.push(chip_button(
            t,
            st,
            &s_str,
            action::SIZE_BASE + i as ActionId,
            active,
            None,
            72,
            None,
        ));
    }
    let size_row_content = t.add(Node::row(10).flex(1), size_buttons);
    let size_row = t.add(
        Node::row(20).cross(Cross::Center),
        [size_label, size_row_content],
    );

    // Row 4: 行距 (3) & 页边距 (3)
    let spacing_label = make_label(t, tr(Msg::MenuSpacing), 160);
    let spacing_names = [
        tr(Msg::TypesetSpacingTight),
        tr(Msg::TypesetSpacingNormal),
        tr(Msg::TypesetSpacingLoose),
    ];
    let mut spacing_buttons = Vec::new();
    for (i, name) in spacing_names.iter().enumerate() {
        let active = i == v.spacing_index;
        spacing_buttons.push(chip_button(
            t,
            st,
            name,
            action::SPACING_BASE + i as ActionId,
            active,
            None,
            72,
            None,
        ));
    }
    let spacing_row_content = t.add(Node::row(12).flex(1), spacing_buttons);

    let margin_label = make_label(t, tr(Msg::MenuMargins), 176);
    let margin_names = [
        tr(Msg::TypesetMarginWide),
        tr(Msg::TypesetMarginNormal),
        tr(Msg::TypesetMarginNarrow),
    ];
    let mut margin_buttons = Vec::new();
    for (i, name) in margin_names.iter().enumerate() {
        let active = i == v.margin_index;
        margin_buttons.push(chip_button(
            t,
            st,
            name,
            action::MARGIN_BASE + i as ActionId,
            active,
            None,
            72,
            None,
        ));
    }
    let margin_row_content = t.add(Node::row(12).flex(1), margin_buttons);

    let row4 = t.add(
        Node::row(20).cross(Cross::Center),
        [
            spacing_label,
            spacing_row_content,
            margin_label,
            margin_row_content,
        ],
    );

    // Row 5: 中文排版 (首行缩进 + 段间距)
    // C.4: 繁简转换、标点挤压、翻页动画都不做（Human 2026-11-09），
    // 界面不显示；旧 reader.json 里的字段仍照常读取。
    let cjk_section_title = t.label(tr(Msg::TypesetCjkSection), st.font_xs().gray(INK_2));
    let gap_14_sub = t.gap_pixels(1, 14);

    // 首行缩进 (2 字 / 无)
    let indent_label = make_label(t, tr(Msg::TypesetIndent), 136);
    let indent_names = [tr(Msg::TypesetIndent2chars), tr(Msg::TypesetIndentNone)];
    let mut indent_buttons = Vec::new();
    for (i, name) in indent_names.iter().enumerate() {
        let active = i == v.indent_index;
        indent_buttons.push(chip_button(
            t,
            st,
            name,
            action::INDENT_BASE + i as ActionId,
            active,
            None,
            72,
            None,
        ));
    }
    let indent_chips = t.add(Node::row(10).flex(1), indent_buttons);
    let col_indent = t.add(
        Node::row(14).cross(Cross::Center).flex(1),
        [indent_label, indent_chips],
    );

    // 段间距 (无 / 小 / 大)
    let gap_label = make_label(t, tr(Msg::TypesetGap), 136);
    let gap_names = [
        tr(Msg::TypesetGapNone),
        tr(Msg::TypesetGapSmall),
        tr(Msg::TypesetGapLarge),
    ];
    let mut gap_buttons = Vec::new();
    for (i, name) in gap_names.iter().enumerate() {
        let active = i == v.gap_index;
        gap_buttons.push(chip_button(
            t,
            st,
            name,
            action::GAP_BASE + i as ActionId,
            active,
            None,
            72,
            None,
        ));
    }
    let gap_chips = t.add(Node::row(10).flex(1), gap_buttons);
    let col_gap = t.add(
        Node::row(14).cross(Cross::Center).flex(1),
        [gap_label, gap_chips],
    );

    let cjk_row_settings = t.add(Node::row(32).cross(Cross::Center), [col_indent, col_gap]);

    // 下划线 (无 / 实线 / 虚线)：正文每一行下方的引导线。
    let underline_label = make_label(t, tr(Msg::TypesetUnderline), 224);
    let underline_names = [
        tr(Msg::TypesetUnderlineOff),
        tr(Msg::TypesetUnderlineSolid),
        tr(Msg::TypesetUnderlineDotted),
    ];
    let mut underline_buttons = Vec::new();
    for (i, name) in underline_names.iter().enumerate() {
        let active = i == v.underline_index;
        underline_buttons.push(chip_button(
            t,
            st,
            name,
            action::UNDERLINE_BASE + i as ActionId,
            active,
            None,
            72,
            None,
        ));
    }
    let underline_chips = t.add(Node::row(10).flex(1), underline_buttons);
    let col_underline = t.add(
        Node::row(14).cross(Cross::Center).flex(1),
        [underline_label, underline_chips],
    );
    let underline_row = t.add(Node::row(32).cross(Cross::Center), [col_underline]);

    // Assemble the bottom sheet
    let gap_24 = t.gap_pixels(1, 24);
    let sheet_content = t.add(
        Node::column(0).cross(Cross::Stretch),
        [
            sheet_header_row,
            gap_20,
            cjk_row,
            gap_20,
            latin_row,
            gap_20,
            size_row,
            gap_20,
            row4,
            gap_24,
            cjk_section_title,
            gap_14_sub,
            cjk_row_settings,
            gap_20,
            underline_row,
        ],
    );
    let sheet_pad = t.pad(Insets::new(24, 88, 32, 88), sheet_content);
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
                let (root, _) = build_typeset_screen(
                    &mut tree,
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
        let (root, ids) = build_typeset_screen(&mut t, &st, &view);
        assert_eq!(root, ids.root);
        assert!(!t.node(ids.collapse).children.is_empty());

        t.set_root(root);
        let mut text = crate::text::TextEngine::new(crate::text::Fonts::embedded());
        t.layout(crate::geom::Rect::new(0, 0, 1264, 1680), &mut text);

        let top_rect = t.rect(t.node(root).children[0]);
        let preview_rect = t.rect(ids.preview);
        let sheet_rect = t.rect(t.node(root).children[2]);

        assert_eq!(top_rect.h, 96);
        assert!(preview_rect.h > 400);
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
