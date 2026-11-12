// SPDX-License-Identifier: AGPL-3.0-or-later
//! Custom widgets the generic node kinds do not cover.
//!
//! All of them draw in solid black and white wherever possible, so their
//! state changes qualify for the fast DU waveform, and keep grays for text
//! anti-aliasing and the odd quiet track.

use crate::canvas::{Painter, BLACK, WHITE};
use crate::geom::{Point, Rect, Size};
use crate::text::{
    layout_paragraph, Align, FamilyId, NoHyphenation, Paragraph, ParagraphLayout, ParagraphStyle,
    Span, TextEngine, TextStyle,
};
use crate::theme::{INK, RULE};
use crate::tree::{Constraints, PaintCx, Widget};
use std::rc::Rc;
use zeno::{PathBuilder, Transform};

/// An 8-bit gray picture (0 black, 255 white), shared cheaply between the
/// screen model and the tree. Cover thumbnails arrive this way from the
/// library's cover cache, already dithered to the panel's 16 levels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrayImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Rc<Vec<u8>>,
}

impl GrayImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        assert_eq!(pixels.len(), width as usize * height as usize);
        Self {
            width,
            height,
            pixels: Rc::new(pixels),
        }
    }

    /// The largest size with this aspect ratio that fits in `w` × `h`.
    pub fn fit(&self, w: i32, h: i32) -> Size {
        if self.width == 0 || self.height == 0 || w <= 0 || h <= 0 {
            return Size::default();
        }
        let sx = w as f32 / self.width as f32;
        let sy = h as f32 / self.height as f32;
        let s = sx.min(sy);
        Size::new(
            ((self.width as f32 * s).round() as i32).clamp(1, w),
            ((self.height as f32 * s).round() as i32).clamp(1, h),
        )
    }

    /// Scaled to `w` × `h` by area averaging: every source pixel counts in
    /// proportion to its overlap, so thin type on a cover does not alias
    /// away the way nearest-neighbour sampling would make it.
    ///
    /// Two separable integer passes with weights worked out once per column
    /// and row: the same filter as `aristotle_library::cover::resize`, which
    /// cannot be shared because the UI crate does not depend on the
    /// library. A page of covers is rescaled on the way to the screen, and
    /// the device has one 1 GHz core.
    pub fn resized(&self, w: u32, h: u32) -> GrayImage {
        if (w, h) == (self.width, self.height) || self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let rows = resample_axis(&self.pixels, self.width, self.height, w, true);
        let pixels = resample_axis(&rows, w, self.height, h, false);
        GrayImage::new(w, h, pixels)
    }

    /// The picture with its tones turned the other way round.
    ///
    /// The night mode inverts the whole screen, so a picture that is to
    /// stay as it is has to be inverted before that happens.
    pub fn inverted(&self) -> GrayImage {
        let pixels = self.pixels.iter().map(|p| 255 - p).collect();
        GrayImage::new(self.width, self.height, pixels)
    }
}

/// One axis resampled: a box filter when shrinking, linear interpolation
/// when enlarging, in 8-bit fixed point.
fn resample_axis(px: &[u8], sw: u32, sh: u32, d: u32, horizontal: bool) -> Vec<u8> {
    let (s_len, lines) = if horizontal { (sw, sh) } else { (sh, sw) };
    let (out_w, out_h) = if horizontal { (d, sh) } else { (sw, d) };
    let mut out = vec![0u8; (out_w * out_h) as usize];
    if d == 0 || s_len == 0 {
        return out;
    }
    let (step, line_step) = if horizontal {
        (1usize, sw as usize)
    } else {
        (sw as usize, 1usize)
    };
    let out_step = if horizontal { 1 } else { out_w as usize };
    let last = s_len as usize - 1;
    let scale = s_len as f64 / d as f64;
    if scale > 1.0 {
        // Per output index: the first source index and its weights (256ths).
        let mut start = Vec::with_capacity(d as usize);
        let mut offset = vec![0u32];
        let mut weights: Vec<u16> = Vec::new();
        for i in 0..d {
            let a = i as f64 * scale;
            let b = (i + 1) as f64 * scale;
            let j0 = a.floor() as u32;
            let j1 = (b.ceil() as u32).min(s_len);
            let mut w: Vec<u16> = (j0..j1)
                .map(|j| {
                    let lo = a.max(j as f64);
                    let hi = b.min(j as f64 + 1.0);
                    ((hi - lo).max(0.0) / scale * 256.0).round() as u16
                })
                .collect();
            let sum: i32 = w.iter().map(|&x| x as i32).sum();
            if let Some(l) = w.last_mut() {
                *l = (*l as i32 + 256 - sum).max(0) as u16;
            }
            start.push(j0);
            weights.extend_from_slice(&w);
            offset.push(weights.len() as u32);
        }
        for line in 0..lines as usize {
            let src_base = line * line_step;
            let out_base = if horizontal { line * d as usize } else { line };
            for i in 0..d as usize {
                let w = &weights[offset[i] as usize..offset[i + 1] as usize];
                let s = src_base + start[i] as usize * step;
                let mut acc = 0u32;
                for (k, &wt) in w.iter().enumerate() {
                    acc += px[s + k * step] as u32 * wt as u32;
                }
                out[out_base + i * out_step] = ((acc + 128) >> 8) as u8;
            }
        }
    } else {
        for line in 0..lines as usize {
            let src_base = line * line_step;
            let out_base = if horizontal { line * d as usize } else { line };
            for i in 0..d as usize {
                let x = ((i as f64 + 0.5) * scale - 0.5).max(0.0);
                let j = (x.floor() as u32 as usize).min(last);
                let t = ((x - j as f64) * 256.0).round().clamp(0.0, 256.0) as u32;
                let a = px[src_base + j * step] as u32;
                let b = px[src_base + (j + 1).min(last) * step] as u32;
                out[out_base + i * out_step] = ((a * (256 - t) + b * t + 128) >> 8) as u8;
            }
        }
    }
    out
}

/// The size a thumbnail is drawn at in a `box`. A thumbnail is never
/// enlarged into a box that is only a little bigger: interpolating pixels
/// that are already dithered for the panel only blurs them, so the picture
/// keeps its own size (up to an eighth smaller than the box) and stays
/// crisp; the resample is left for boxes that really are larger, such as
/// the cover grid.
fn drawn_size(img: &GrayImage, b: Size) -> Size {
    let fit = img.fit(b.w, b.h);
    let up = fit.w as u32 >= img.width && fit.h as u32 >= img.height;
    let near = (fit.w - img.width as i32).abs() * 8 <= img.width as i32
        && (fit.h - img.height as i32).abs() * 8 <= img.height as i32;
    if up && near {
        Size::new(img.width as i32, img.height as i32)
    } else {
        fit
    }
}

/// What a typographic cover shows besides title and author.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoverMark {
    /// The shelf the book belongs to (its series or folder). Books of one
    /// shelf share a binding, so an author's folder reads as a set.
    pub group: Option<String>,
    /// The book's number in its series, printed small on the card.
    pub series_index: Option<f32>,
}

/// The four bindings of typographic cards: a background and an ink.
/// Two light (#FFF with 2px stroke, #F2F2F2) and two dark (#000, #333),
/// avoiding mid-grays (0x55) to reduce e-ink ghosting.
pub const BINDINGS: [(u8, u8); 4] = [(WHITE, BLACK), (0x33, WHITE), (0xf2, BLACK), (BLACK, WHITE)];

/// FNV-1a, a stable hash for picking a binding.
fn fnv(s: &str) -> u32 {
    s.bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619))
}

/// True when a title contains Chinese/CJK characters.
pub fn is_cjk_title(title: &str) -> bool {
    title.chars().any(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}' |
            '\u{3400}'..='\u{4DBF}' |
            '\u{20000}'..='\u{2A6DF}' |
            '\u{2A700}'..='\u{2B73F}' |
            '\u{2B740}'..='\u{2B81F}' |
            '\u{2B820}'..='\u{2CEAF}' |
            '\u{F900}'..='\u{FAFF}' |
            '\u{2F800}'..='\u{2FA1F}'
        )
    })
}

/// Splits a Chinese title into columns for vertical typographic strip layout with a column capacity limit.
/// - Title <= max_per_col: one column
/// - Title <= max_per_col * 2: two columns evenly split
/// - Title > max_per_col * 2: truncated with ellipsis (first max_per_col, then next (max_per_col - 1) + '…')
/// - When max_per_col <= 1: only the first character is kept (one column)
pub fn split_cjk_title_with_limit(title: &str, max_per_col: usize) -> (Vec<char>, Vec<char>) {
    let chars: Vec<char> = title.chars().filter(|c| !c.is_whitespace()).collect();
    if max_per_col <= 1 {
        let first = chars.into_iter().take(1).collect();
        return (first, Vec::new());
    }
    if chars.len() <= max_per_col {
        (chars, Vec::new())
    } else if chars.len() <= max_per_col * 2 {
        let per = chars.len().div_ceil(2);
        (chars[..per].to_vec(), chars[per..].to_vec())
    } else {
        let mut c2 = chars[max_per_col..(max_per_col * 2 - 1)].to_vec();
        c2.push('…');
        (chars[..max_per_col].to_vec(), c2)
    }
}

/// Splits a Chinese title into columns for vertical typographic strip layout (default max 4 chars per column).
/// - Title <= 4 characters: one column
/// - Title 5–8 characters: two columns evenly split
/// - Title > 8 characters: truncated with ellipsis (first 4, then next 3 + '…')
pub fn split_cjk_title(title: &str) -> (Vec<char>, Vec<char>) {
    split_cjk_title_with_limit(title, 4)
}

/// "3", "2.5" ("2,5" in Polish): a series number without a pointless ".0".
pub fn series_number(i: f32) -> String {
    if i.fract() == 0.0 {
        format!("{i:.0}")
    } else {
        let s = format!("{i}");
        match aristotle_i18n::locale().decimal {
            "." => s,
            sep => s.replacen('.', sep, 1),
        }
    }
}

/// The type metrics of a cover's vertical CJK title strip.
///
/// The strip is one design scaled with the type that sits in it: the
/// em size, the padding and the space the strip leaves around the type
/// all shrink together when a long title has to be set smaller.
#[derive(Clone, Copy)]
struct StripType {
    /// Em size of one glyph.
    font: f32,
    /// The smallest em size the panel can still read.
    min_font: f32,
    /// Distance from the cover's edge to the strip.
    margin_x: f32,
    margin_y: f32,
    /// Padding between the strip's edge and the type.
    pad_x: f32,
    pad_y: f32,
    /// Space between two glyphs of a column, and between two columns.
    char_gap: f32,
    col_gap: f32,
    /// Strip width and height beyond the type block.
    slack_w: f32,
    slack_h: f32,
}

impl StripType {
    /// The card layout's own metrics for a large cover: the type grows with
    /// the card and the strip hugs the type.
    fn large(w: f32, h: f32) -> Self {
        if w >= 400.0 {
            return Self {
                font: 72.0,
                min_font: 36.0,
                margin_x: 42.0,
                margin_y: 42.0,
                pad_x: 28.0,
                pad_y: 32.0,
                char_gap: 10.0,
                col_gap: 14.0,
                slack_w: 56.0,
                slack_h: 64.0,
            };
        }
        let font = (w * 0.16).clamp(28.0, 36.0);
        let pad_x = (font * 0.22).clamp(6.0, 10.0);
        let pad_y = (font * 0.25).clamp(8.0, 12.0);
        Self {
            font,
            min_font: 28.0,
            margin_x: (w * 0.08).round().clamp(14.0, 24.0),
            margin_y: (h * 0.06).round().clamp(14.0, 24.0),
            pad_x,
            pad_y,
            char_gap: (font * 0.10).max(2.0),
            col_gap: (font * 0.12).max(4.0),
            slack_w: pad_x * 2.0,
            slack_h: pad_y * 2.0,
        }
    }
}

/// The prototype's title strip for a cover `w` pixels wide.
///
/// `docs/ui/prototype-v2/reference/home.png` sets 32 px type 12 px from the
/// top left corner of its 144x192 card, `library.png` 28 px type 10 px in on
/// its 108x144 thumbnail. Between the two the metrics interpolate; outside
/// them they scale with the cover, so a smaller card gets a smaller strip
/// and the type keeps its place in it.
fn strip_type(w: f32) -> StripType {
    const LIB: StripType = StripType {
        font: 28.0,
        min_font: 12.0,
        margin_x: 10.0,
        margin_y: 10.0,
        pad_x: 4.0,
        pad_y: 6.0,
        char_gap: 0.0,
        col_gap: 0.0,
        slack_w: 24.0,
        slack_h: 28.0,
    };
    const HOME: StripType = StripType {
        font: 32.0,
        min_font: 12.0,
        margin_x: 12.0,
        margin_y: 12.0,
        pad_x: 6.0,
        pad_y: 8.0,
        char_gap: 0.0,
        col_gap: 0.0,
        slack_w: 24.0,
        slack_h: 28.0,
    };
    const LIB_W: f32 = 108.0;
    const HOME_W: f32 = 144.0;
    let (a, b, t, scale) = if w <= LIB_W {
        (LIB, LIB, 0.0, w / LIB_W)
    } else if w >= HOME_W {
        (HOME, HOME, 1.0, w / HOME_W)
    } else {
        (LIB, HOME, (w - LIB_W) / (HOME_W - LIB_W), 1.0)
    };
    let mix = |x: f32, y: f32| (x + (y - x) * t) * scale;
    let min_font = a.min_font.min(b.min_font);
    StripType {
        font: mix(a.font, b.font).round().max(min_font),
        min_font,
        margin_x: mix(a.margin_x, b.margin_x).round(),
        margin_y: mix(a.margin_y, b.margin_y).round(),
        pad_x: mix(a.pad_x, b.pad_x).round(),
        pad_y: mix(a.pad_y, b.pad_y).round(),
        char_gap: mix(a.char_gap, b.char_gap),
        col_gap: mix(a.col_gap, b.col_gap),
        slack_w: mix(a.slack_w, b.slack_w).round(),
        slack_h: mix(a.slack_h, b.slack_h).round(),
    }
}

/// A book cover: a typographic card, or the real cover when one is given.
///
/// Most sideloaded books have poor or no covers, and e-ink renders photo
/// covers as muddy gray. A card set in type is crisp and legible in 16
/// grays and inverted, needs no image decoding, and its binding (one of
/// [`BINDINGS`], picked by shelf) groups an author's or a series' books at a
/// glance. Large cards (150 px wide and up) carry the title, a short rule
/// and the author in small capitals, with "Volume 3" above the title for a
/// series; small ones a monogram, since the title is printed next to them,
/// and the series number in a corner.
pub struct Cover {
    pub title: String,
    pub author: String,
    pub family: FamilyId,
    pub mark: CoverMark,
    pub binding: Option<(u8, u8)>,
    /// The real cover, scaled to the widget once per size.
    pub image: Option<GrayImage>,
    size: Size,
    scaled: Option<GrayImage>,
    title_layout: Option<ParagraphLayout>,
    author_layout: Option<ParagraphLayout>,
    mark_layout: Option<ParagraphLayout>,
    cjk_chars: Vec<(ParagraphLayout, Point)>,
    strip_rect: Option<Rect>,
    author_pos: Option<Point>,
    mark_pos: Option<Point>,
}

impl Cover {
    pub fn new(title: &str, author: &str, family: FamilyId) -> Self {
        Self {
            title: title.to_string(),
            author: author.to_string(),
            family,
            mark: CoverMark::default(),
            binding: None,
            image: None,
            size: Size::default(),
            scaled: None,
            title_layout: None,
            author_layout: None,
            mark_layout: None,
            cjk_chars: Vec::new(),
            strip_rect: None,
            author_pos: None,
            mark_pos: None,
        }
    }

    pub fn with_image(mut self, image: Option<GrayImage>) -> Self {
        self.image = image;
        self
    }

    /// Replaces a cover photo and releases the scaled copy when it changes.
    pub fn set_image(&mut self, image: Option<GrayImage>) -> bool {
        let same = match (&self.image, &image) {
            (Some(a), Some(b)) => {
                a.width == b.width && a.height == b.height && Rc::ptr_eq(&a.pixels, &b.pixels)
            }
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        self.image = image;
        self.scaled = None;
        self.size = Size::default();
        true
    }

    pub fn with_mark(mut self, mark: CoverMark) -> Self {
        self.mark = mark;
        self
    }

    pub fn with_binding(mut self, bg: u8, ink: u8) -> Self {
        self.binding = Some((bg, ink));
        self
    }

    /// Background and ink of the card's binding.
    pub fn binding(&self) -> (u8, u8) {
        if let Some(b) = self.binding {
            return b;
        }
        let key = self.mark.group.as_deref().unwrap_or(&self.title);
        BINDINGS[(fnv(key) % BINDINGS.len() as u32) as usize]
    }

    fn large(&self, density: crate::geom::Density) -> bool {
        self.size.w >= density.px(150)
    }

    /// The bounding rectangle of the vertical CJK title strip, relative to the cover.
    pub fn strip_rect(&self) -> Option<Rect> {
        self.strip_rect
    }

    /// The laid out CJK characters and their relative positions.
    pub fn cjk_chars(&self) -> &[(ParagraphLayout, Point)] {
        &self.cjk_chars
    }
}

impl Widget for Cover {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let s = c.constrain(c.max);
        if s != self.size {
            self.size = s;
            if let Some(img) = &self.image {
                let fit = drawn_size(img, s);
                self.scaled = Some(if (fit.w as u32, fit.h as u32) == (img.width, img.height) {
                    img.clone()
                } else {
                    img.resized(fit.w as u32, fit.h as u32)
                });
                return s;
            }
            let (bg, ink) = self.binding();
            let w = s.w as f32;
            let h = s.h as f32;
            let density = text.density();
            let index = self.mark.series_index.map(series_number);
            let is_large = self.large(density);

            if is_cjk_title(&self.title) {
                self.title_layout = None;
                self.cjk_chars.clear();
                self.strip_rect = None;

                // A light card carries a 2 px ink frame, and its strip
                // carries one too: the type sits inside the frame, the way
                // the prototype puts it inside the card's.
                let frame = if bg == WHITE || bg == 0xf2 {
                    density.scale(2.0)
                } else {
                    0.0
                };

                let ty = if is_large {
                    StripType::large(w, h)
                } else {
                    strip_type(w)
                };

                let (margin_x, margin_y, max_avail_w, max_avail_h) = if is_large {
                    let mx = ty.margin_x;
                    let my = ty.margin_y;
                    let author_space = if !self.author.is_empty() {
                        let author_font_size = if w >= 400.0 {
                            36.0
                        } else {
                            (w * 0.12).clamp(24.0, 32.0)
                        };
                        let author_style = TextStyle::new(self.family, author_font_size).gray(ink);
                        let mut ap = Paragraph::plain(&self.author, author_style, Align::Left);
                        ap.style.max_lines = Some(1);
                        let al = layout_paragraph(text, &ap, w * 0.8, &NoHyphenation);
                        let ax = (frame + mx).round() as i32;
                        let ay = (h - my - al.height).round() as i32;
                        self.author_layout = Some(al);
                        self.author_pos = Some(Point { x: ax, y: ay });
                        (h - ay as f32) + 12.0
                    } else {
                        self.author_layout = None;
                        self.author_pos = None;
                        my
                    };
                    (
                        mx,
                        my,
                        (w - (frame + mx) * 2.0).max(10.0),
                        (h - my - author_space).max(10.0),
                    )
                } else {
                    self.author_layout = None;
                    self.author_pos = None;
                    (
                        ty.margin_x,
                        ty.margin_y,
                        (w - (frame + ty.margin_x) * 2.0).max(10.0),
                        (h - (frame + ty.margin_y) * 2.0).max(10.0),
                    )
                };

                struct Candidate {
                    col1: Vec<char>,
                    col2: Vec<char>,
                    font_size: f32,
                    col_w: f32,
                    step: f32,
                    col_gap: f32,
                    pad_x: f32,
                    pad_y: f32,
                    strip_w: f32,
                    strip_h: f32,
                }

                let try_candidate = |font_size: f32, max_per_col: usize| -> Option<Candidate> {
                    let (col1, col2) = split_cjk_title_with_limit(&self.title, max_per_col);
                    let cols = if col2.is_empty() { 1 } else { 2 };
                    let max_chars = col1.len().max(col2.len());
                    if max_chars == 0 {
                        return None;
                    }
                    let char_height = font_size.round();
                    let col_w = font_size.round();
                    // The strip is one design, scaled with the type in it.
                    let k = font_size / ty.font;
                    let pad_x = (ty.pad_x * k).round();
                    let pad_y = (ty.pad_y * k).round();
                    let col_gap = (ty.col_gap * k).round();
                    let char_gap = (ty.char_gap * k).round();
                    let slack_w = (ty.slack_w * k).round();
                    let slack_h = (ty.slack_h * k).round();
                    let step = char_height + char_gap;
                    let mut strip_w =
                        (cols as f32 * col_w + (cols - 1) as f32 * col_gap + slack_w).round();
                    let mut strip_h =
                        ((max_chars - 1) as f32 * step + char_height + slack_h).round();
                    if w >= 400.0 {
                        strip_w = 128.0;
                        strip_h = 520.0;
                    }
                    if strip_w <= max_avail_w && strip_h <= max_avail_h {
                        Some(Candidate {
                            col1,
                            col2,
                            font_size,
                            col_w,
                            step,
                            col_gap,
                            pad_x,
                            pad_y,
                            strip_w,
                            strip_h,
                        })
                    } else {
                        None
                    }
                };

                let mut chosen = None;

                // 1. Try target font size, decreasing max_per_col (4 -> 3 -> 2)
                for max_per_col in (2..=4).rev() {
                    if let Some(cand) = try_candidate(ty.font, max_per_col) {
                        chosen = Some(cand);
                        break;
                    }
                }

                // 2. Reduce font size down to min_font_size, trying max_per_col (4 -> 3 -> 2)
                if chosen.is_none() {
                    let mut fs = ty.font - 1.0;
                    while fs >= ty.min_font - 0.01 {
                        for max_per_col in (2..=4).rev() {
                            if let Some(cand) = try_candidate(fs, max_per_col) {
                                chosen = Some(cand);
                                break;
                            }
                        }
                        if chosen.is_some() {
                            break;
                        }
                        fs -= 1.0;
                    }
                }

                // 3. Fallback: single initial character ("只显示书名首字")
                if chosen.is_none() {
                    let mut fs = ty.min_font;
                    while fs >= 6.0 {
                        if let Some(cand) = try_candidate(fs, 1) {
                            chosen = Some(cand);
                            break;
                        }
                        fs -= 1.0;
                    }
                }

                let cand = chosen.unwrap_or_else(|| {
                    let (col1, col2) = split_cjk_title_with_limit(&self.title, 1);
                    Candidate {
                        col1,
                        col2,
                        font_size: ty.min_font,
                        col_w: ty.min_font.round(),
                        step: ty.min_font.round() + 1.0,
                        col_gap: 0.0,
                        pad_x: 2.0,
                        pad_y: 2.0,
                        strip_w: max_avail_w.min(ty.min_font + 4.0),
                        strip_h: max_avail_h.min(ty.min_font + 4.0),
                    }
                });

                let strip_x = frame + margin_x;
                let strip_y = frame + margin_y;
                let strip_w = cand.strip_w;
                let strip_h = cand.strip_h;

                self.strip_rect = Some(Rect::new(
                    strip_x.round() as i32,
                    strip_y.round() as i32,
                    strip_w.round() as i32,
                    strip_h.round() as i32,
                ));

                let cols = if cand.col2.is_empty() { 1 } else { 2 };
                // Vertical-rl: the first column is the right one, and the
                // type block hugs the strip's left edge, as in the
                // prototype.
                let origin_x = strip_x + frame + cand.pad_x;
                let origin_y = strip_y + frame + cand.pad_y;
                let col1_x = origin_x + (cols - 1) as f32 * (cand.col_w + cand.col_gap);
                let col2_x = origin_x;

                let char_style = TextStyle::new(self.family, cand.font_size)
                    .bold()
                    .line_height(1.0)
                    .gray(INK);

                for (col_idx, (col_chars, cx)) in [(&cand.col1, col1_x), (&cand.col2, col2_x)]
                    .into_iter()
                    .enumerate()
                {
                    if col_idx == 1 && cols == 1 {
                        continue;
                    }
                    for (j, &ch) in col_chars.iter().enumerate() {
                        let p = Paragraph::plain(ch.to_string(), char_style.clone(), Align::Center);
                        let layout = layout_paragraph(text, &p, cand.col_w, &NoHyphenation);
                        let cy = origin_y + j as f32 * cand.step;
                        self.cjk_chars.push((
                            layout,
                            Point {
                                x: cx.round() as i32,
                                y: cy.round() as i32,
                            },
                        ));
                    }
                }

                if let Some(i) = index {
                    if is_large {
                        let label = aristotle_i18n::tf!(aristotle_i18n::Msg::LibraryVolume, n = i);
                        let small_style =
                            TextStyle::new(self.family, (w * 0.068).clamp(16.0, 22.0)).gray(ink);
                        let p = Paragraph::plain(label, small_style, Align::Right);
                        let ml = layout_paragraph(text, &p, w * 0.4, &NoHyphenation);
                        let mx = (w - margin_x - ml.width).round() as i32;
                        let my = margin_y.round() as i32;
                        self.mark_layout = Some(ml);
                        self.mark_pos = Some(Point { x: mx, y: my });
                    } else {
                        let small_style = TextStyle::new(self.family, w * 0.2).bold().gray(ink);
                        let p = Paragraph::plain(i, small_style, Align::Right);
                        let ml = layout_paragraph(text, &p, w * 0.84, &NoHyphenation);
                        let mx = (w * 0.08).round() as i32;
                        let my = (h - ml.height - h * 0.03).round() as i32;
                        self.mark_layout = Some(ml);
                        self.mark_pos = Some(Point { x: mx, y: my });
                    }
                } else {
                    self.mark_layout = None;
                    self.mark_pos = None;
                }
            } else {
                self.cjk_chars.clear();
                self.strip_rect = None;
                self.author_pos = None;
                self.mark_pos = None;
                if is_large {
                    let inner = w * 0.72;
                    let mut title = TextStyle::new(self.family, w * 0.13)
                        .bold()
                        .line_height(1.12)
                        .gray(ink);
                    // Cards are not hyphenated: shrink the type until the
                    // longest word fits ("przeznaczenia" on a narrow card).
                    let longest = self
                        .title
                        .split_whitespace()
                        .map(|word| text.measure(word, &title))
                        .fold(0.0f32, f32::max);
                    if longest > inner {
                        title.size *= inner / longest;
                    }
                    let inner = w * 0.76;
                    let mut set = |title: TextStyle| {
                        let mut p = Paragraph::plain(&self.title, title, Align::Center);
                        p.style.max_lines = Some(4);
                        p.style.balance = true;
                        layout_paragraph(text, &p, inner, &NoHyphenation)
                    };
                    let mut layout = set(title.clone());
                    // The word-by-word measure above misses lines the breaker
                    // cannot split further (a short word glued to a long one,
                    // "i uprzedzenie" on a grid card): shrink until the set
                    // lines fit as well.
                    for _ in 0..3 {
                        if layout.width <= inner {
                            break;
                        }
                        title.size *= inner / layout.width * 0.98;
                        layout = set(title.clone());
                    }
                    self.title_layout = Some(layout);
                    let small = TextStyle::new(self.family, w * 0.068)
                        .small_caps()
                        .tracking(w * 0.006)
                        .gray(ink);
                    let mut p = Paragraph::plain(&self.author, small.clone(), Align::Center);
                    p.style.max_lines = Some(2);
                    self.author_layout = Some(layout_paragraph(text, &p, inner, &NoHyphenation));
                    self.mark_layout = index.map(|i| {
                        let label = aristotle_i18n::tf!(aristotle_i18n::Msg::LibraryVolume, n = i);
                        let p = Paragraph::plain(label, small, Align::Center);
                        layout_paragraph(text, &p, inner, &NoHyphenation)
                    });
                } else {
                    let initial: String = self.title.chars().take(1).collect();
                    let style = TextStyle::new(self.family, w * 0.58)
                        .italic()
                        .line_height(1.0)
                        .gray(ink);
                    let p = Paragraph::new(
                        vec![Span::new(initial, style)],
                        ParagraphStyle {
                            align: Align::Center,
                            ..Default::default()
                        },
                    );
                    self.title_layout = Some(layout_paragraph(text, &p, w, &NoHyphenation));
                    self.author_layout = None;
                    self.mark_layout = index.map(|i| {
                        let style = TextStyle::new(self.family, w * 0.2).bold().gray(ink);
                        let p = Paragraph::plain(i, style, Align::Right);
                        layout_paragraph(text, &p, w * 0.84, &NoHyphenation)
                    });
                }
            }
        }
        s
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let density = cx.text.density();
        if let Some(img) = &self.scaled {
            // Bottom-aligned and centred, like a book standing on a shelf;
            // a hairline keeps white covers from dissolving into the page.
            let x = r.x + (r.w - img.width as i32) / 2;
            let y = r.bottom() - img.height as i32;
            let p = &mut *cx.painter;
            p.blit_gray(x, y, img.width, img.height, img.width as usize, &img.pixels);
            p.stroke_rect(
                Rect::new(x, y, img.width as i32, img.height as i32),
                1,
                RULE,
            );
            return;
        }
        let (bg, ink) = self.binding();
        let w = r.w as f32;
        let h = r.h as f32;
        let radius = if w >= 400.0 {
            density.scale(24.0)
        } else {
            (w * 0.05).clamp(density.scale(4.0), density.scale(16.0))
        };
        let p = &mut *cx.painter;
        p.fill_round_rect(r, radius, bg);
        if bg == WHITE || bg == 0xf2 {
            // A light card needs an edge on white paper.
            p.stroke_round_rect(r, radius, density.scale(2.0), INK);
        }
        if is_cjk_title(&self.title) {
            if let Some(strip) = self.strip_rect {
                let strip_r = Rect::new(r.x + strip.x, r.y + strip.y, strip.w, strip.h);
                // The prototype rounds the strip's corners by 6 px (12 px for hero).
                let strip_radius = if w >= 400.0 {
                    density.scale(12.0)
                } else {
                    density.scale(6.0).min(strip.w as f32 * 0.3)
                };
                p.fill_round_rect(strip_r, strip_radius, WHITE);
                if bg == WHITE || bg == 0xf2 {
                    p.stroke_round_rect(strip_r, strip_radius, density.scale(2.0), INK);
                }
                for (layout, pt) in &self.cjk_chars {
                    layout.draw(
                        cx.text,
                        cx.painter,
                        (r.x + pt.x) as f32,
                        (r.y + pt.y) as f32,
                    );
                }
                if let (Some(a), Some(pt)) = (&self.author_layout, self.author_pos) {
                    a.draw(
                        cx.text,
                        cx.painter,
                        (r.x + pt.x) as f32,
                        (r.y + pt.y) as f32,
                    );
                }
                if let (Some(m), Some(pt)) = (&self.mark_layout, self.mark_pos) {
                    m.draw(
                        cx.text,
                        cx.painter,
                        (r.x + pt.x) as f32,
                        (r.y + pt.y) as f32,
                    );
                }
            }
            return;
        }
        let x = r.x as f32 + w * 0.12;
        if self.large(density) {
            // An inner frame, like a border blocked on a cloth binding.
            let inset = (w * 0.06).round() as i32;
            p.stroke_rect(r.outset(-inset), density.px(2), ink);
            if let Some(m) = &self.mark_layout {
                m.draw(cx.text, cx.painter, x, r.y as f32 + h * 0.11);
            }
            if let Some(t) = &self.title_layout {
                // Short titles sit a little lower than long ones.
                let y = r.y as f32 + h * 0.2 + (h * 0.3 - t.height).max(0.0) * 0.5;
                t.draw(cx.text, cx.painter, x, y);
                let rule_y = (y + t.height + h * 0.05) as i32;
                let rw = (w * 0.16) as i32;
                cx.painter.fill_rect(
                    Rect::new(r.center().x - rw / 2, rule_y, rw, density.px(3)),
                    ink,
                );
            }
            if let Some(a) = &self.author_layout {
                let ay = r.bottom() as f32 - h * 0.13 - a.height;
                a.draw(cx.text, cx.painter, x, ay);
            }
        } else {
            if let Some(t) = &self.title_layout {
                let y = r.y as f32 + (h - t.height) / 2.0;
                t.draw(cx.text, cx.painter, r.x as f32, y);
            }
            if let Some(m) = &self.mark_layout {
                let y = r.bottom() as f32 - m.height - h * 0.03;
                m.draw(cx.text, cx.painter, r.x as f32 + w * 0.08, y);
            }
        }
    }
}

/// Battery: the icon family's outline with a proportional fill, or a bolt
/// while charging (the percentage next to it still says how full).
pub struct Battery {
    pub level: f32,
    pub charging: bool,
}

impl Widget for Battery {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        c.constrain(Size::new(text.density().px(46), text.density().px(26)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let d = cx.text.density();
        let p = &mut *cx.painter;
        let body = Rect::new(r.x, r.y, r.w - d.px(6), r.h);
        p.stroke_round_rect(body, d.scale(4.0), d.scale(3.0), INK);
        p.fill_rect(
            Rect::new(body.right() + d.px(2), r.y + r.h / 3, d.px(4), r.h / 3),
            INK,
        );
        let inner = body.outset(-d.px(6));
        if self.charging {
            let s = inner.h as f32 / 7.0;
            let (x, y) = (inner.center().x as f32, inner.y as f32);
            let mut bolt: Vec<zeno::Command> = Vec::new();
            bolt.move_to([x + 0.8 * s, y])
                .line_to([x - 2.4 * s, y + 4.0 * s])
                .line_to([x - 0.2 * s, y + 4.0 * s])
                .line_to([x - 0.8 * s, y + 7.0 * s])
                .line_to([x + 2.4 * s, y + 3.0 * s])
                .line_to([x + 0.2 * s, y + 3.0 * s])
                .close();
            p.fill_path(&bolt, None, INK);
            return;
        }
        let w = (inner.w as f32 * self.level.clamp(0.0, 1.0)).round() as i32;
        p.fill_rect(Rect::new(inner.x, inner.y, w, inner.h), INK);
    }
}

/// The busy mark: a black tab with three white dots, square-cornered and
/// pixel-aligned so it is pure black and white and goes out with DU. Shown
/// in the status line when an action will take longer than about 300 ms.
pub struct BusyMark;

impl Widget for BusyMark {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        c.constrain(Size::new(text.density().px(72), text.density().px(28)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        p.fill_rect(r, BLACK);
        let d = cx.text.density().px(8);
        let gap = (r.w - 3 * d) / 4;
        for i in 0..3 {
            let x = r.x + gap + i * (d + gap);
            p.fill_rect(Rect::new(x, r.y + (r.h - d) / 2, d, d), WHITE);
        }
    }
}

/// The bar under "today": the part of the daily goal read, on a white
/// track. The track is white so it reads as empty on the gray card, the
/// fill is black, and both ends are round.
pub struct GoalBar {
    /// The share of the goal read, from 0 to 1.
    pub value: f32,
}

impl Widget for GoalBar {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let d = text.density();
        c.constrain(Size::new(d.px(0), d.px(10)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        let radius = r.h as f32 / 2.0;
        p.fill_round_rect(r, radius, WHITE);
        let w = (r.w as f32 * self.value.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            p.fill_round_rect(Rect::new(r.x, r.y, w, r.h), radius, INK);
        }
    }
}

/// An on/off switch. Pure black and white in both states, so flipping it
/// is a single DU update.
pub struct Toggle {
    pub on: bool,
}

impl Widget for Toggle {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        c.constrain(Size::new(text.density().px(104), text.density().px(58)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let d = cx.text.density();
        let p = &mut *cx.painter;
        let radius = r.h as f32 / 2.0;
        let knob_r = r.h as f32 / 2.0 - d.scale(9.0);
        let cy = r.y + r.h / 2;
        if self.on {
            p.fill_round_rect(r, radius, INK);
            let cx = r.right() - r.h / 2;
            p.fill_circle(Point { x: cx, y: cy }, knob_r, WHITE);
        } else {
            p.stroke_round_rect(r, radius, d.scale(4.0), INK);
            let cx = r.x + r.h / 2;
            p.fill_circle(Point { x: cx, y: cy }, knob_r, INK);
        }
    }
}

/// A horizontal slider, e.g. for the frontlight. The app maps a tap's x
/// position to a value with [`Slider::value_at`] or [`Slider::step_at`];
/// there is no dragging, because a drag on e-ink would need a stream of A2
/// updates for little gain over tapping where you want it.
pub struct Slider {
    pub value: f32,
}

impl Slider {
    /// A slider at `value`, between 0 and 1.
    pub fn new(value: f32) -> Self {
        Slider {
            value: value.clamp(0.0, 1.0),
        }
    }

    pub fn value_at(rect: Rect, x: i32) -> f32 {
        let pad = rect.h / 2;
        ((x - rect.x - pad) as f32 / (rect.w - 2 * pad).max(1) as f32).clamp(0.0, 1.0)
    }

    /// The nearest of the positions 0 to `steps` to a tap at `x`.
    pub fn step_at(rect: Rect, x: i32, steps: u32) -> u32 {
        (Self::value_at(rect, x) * steps as f32).round() as u32
    }
}

impl Widget for Slider {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        // 64 px, so the slider is a touch target of its own.
        c.constrain(Size::new(c.max.w, text.density().px(64)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let d = cx.text.density();
        let p = &mut *cx.painter;
        let pad = r.h / 2;
        let cy = r.y + r.h / 2;
        let x0 = r.x + pad;
        let w = r.w - 2 * pad;
        let kx = x0 + (w as f32 * self.value.clamp(0.0, 1.0)).round() as i32;
        p.fill_rect(Rect::new(x0, cy - d.px(1), w, d.px(3)), RULE);
        p.fill_rect(Rect::new(x0, cy - d.px(3), kx - x0, d.px(7)), INK);
        p.fill_circle(Point { x: kx, y: cy }, d.scale(20.0), INK);
        p.fill_circle(Point { x: kx, y: cy }, d.scale(11.0), WHITE);
    }
}

/// A row of dots growing in size, filled up to the current step. Used for
/// the font size: it shows both where you are and how many steps exist.
pub struct Steps {
    pub count: usize,
    pub value: usize,
}

impl Widget for Steps {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        c.constrain(Size::new(c.max.w, text.density().px(56)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let density = cx.text.density();
        let p = &mut *cx.painter;
        let n = self.count.max(1) as i32;
        let step = r.w / n;
        for i in 0..n {
            let d = density.scale(10.0 + 14.0 * i as f32 / (n - 1).max(1) as f32);
            let c = Point {
                x: r.x + step * i + step / 2,
                y: r.y + r.h / 2,
            };
            if i as usize <= self.value {
                p.fill_circle(c, d / 2.0, INK);
            } else {
                p.fill_circle(c, d / 2.0, INK);
                p.fill_circle(c, d / 2.0 - density.scale(2.5), WHITE);
            }
        }
    }
}

/// Draws a dashed outline, for debug overlays (damage, tap zones).
pub fn dashed_rect(p: &mut Painter, r: Rect, width: i32, dash: i32, gray: u8) {
    let mut x = r.x;
    while x < r.right() {
        let l = dash.min(r.right() - x);
        p.fill_rect(Rect::new(x, r.y, l, width), gray);
        p.fill_rect(Rect::new(x, r.bottom() - width, l, width), gray);
        x += 2 * dash;
    }
    let mut y = r.y;
    while y < r.bottom() {
        let l = dash.min(r.bottom() - y);
        p.fill_rect(Rect::new(r.x, y, width, l), gray);
        p.fill_rect(Rect::new(r.right() - width, y, width, l), gray);
        y += 2 * dash;
    }
}

/// The width of the thin bar of the status bar, in reference pixels.
pub const THIN_BAR_W: i32 = 360;

/// The height of that bar: thin enough to sit under a line of text.
pub const THIN_BAR_H: i32 = 12;

/// A progress bar for document formatting: a horizontal track with a
/// proportional black fill, and the percentage text beside it. Solid black
/// and white so updates qualify for the fast DU/A2 waveforms.
pub struct ProgressBar {
    pub percent: u8,
    /// How the bar draws itself.
    pub kind: BarKind,
}

/// How a [`ProgressBar`] draws itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    /// The reader's footer: a white pill with a bordered track and the
    /// percentage at its right.
    Pill,
    /// The status bar: a thin track with no pill and no text of its own,
    /// the line above it already says the share.
    Thin,
}

impl ProgressBar {
    pub fn new(percent: u8) -> Self {
        Self {
            percent,
            kind: BarKind::Pill,
        }
    }

    /// The thin bar of the status bar.
    pub fn thin(percent: u8) -> Self {
        Self {
            percent,
            kind: BarKind::Thin,
        }
    }
}

impl Widget for ProgressBar {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let d = text.density();
        match self.kind {
            // A compact bar: 240 px wide, 32 px tall at 300 DPI.
            BarKind::Pill => c.constrain(Size::new(d.px(240), d.px(32))),
            // The bar of the status bar: 360 px wide, thin.
            BarKind::Thin => c.constrain(Size::new(d.px(THIN_BAR_W), d.px(THIN_BAR_H))),
        }
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let d = cx.text.density();
        let p = &mut *cx.painter;

        if self.kind == BarKind::Thin {
            // A bare track: a hairline border and a black fill. No pill and
            // no text: the line above the bar says the share.
            let border_w = d.scale(1.5).max(1.0).round() as i32;
            p.stroke_rect(r, border_w, INK);
            let fill_max = r.w - 2 * border_w;
            let fill_w =
                ((fill_max as f32 * (self.percent.min(100) as f32 / 100.0)).round() as i32).max(0);
            if fill_w > 0 {
                p.fill_rect(
                    Rect::new(r.x + border_w, r.y + border_w, fill_w, r.h - 2 * border_w),
                    INK,
                );
            }
            return;
        }

        // Background pill: solid white to cleanly cover the footer underneath,
        // with a crisp border.
        let radius = d.scale(4.0);
        p.fill_round_rect(r, radius, WHITE);
        p.stroke_round_rect(r, radius, d.scale(2.0), INK);

        // Track dimensions: left side of the pill.
        let pad_x = d.px(14);
        let track_w = d.px(140);
        let track_h = d.px(10);
        let track_y = r.y + (r.h - track_h) / 2;
        let track_rect = Rect::new(r.x + pad_x, track_y, track_w, track_h);

        // Border around track
        let border_w = d.scale(1.5).max(1.0).round() as i32;
        p.stroke_rect(track_rect, border_w, INK);

        // Fill within track
        let fill_max = track_w - 2 * border_w;
        let fill_w =
            ((fill_max as f32 * (self.percent.min(100) as f32 / 100.0)).round() as i32).max(0);
        if fill_w > 0 {
            let fill_rect = Rect::new(
                track_rect.x + border_w,
                track_rect.y + border_w,
                fill_w,
                track_h - 2 * border_w,
            );
            p.fill_rect(fill_rect, INK);
        }

        // Percentage text on the right
        let percent_str = format!("{}%", self.percent.min(100));
        let style = TextStyle::new(FamilyId(0), d.scale(14.0)).gray(INK);
        let para = Paragraph::plain(&percent_str, style, Align::Left);
        let layout = layout_paragraph(cx.text, &para, d.scale(60.0), &NoHyphenation);
        let text_x = (track_rect.right() + d.px(10)) as f32;
        let text_y = (r.y as f32 + (r.h as f32 - layout.height) / 2.0).round();
        layout.draw(cx.text, cx.painter, text_x, text_y);
    }
}

/// A 6px fine progress bar with a customizable background track color
/// and an inner black progress fill with rounded ends according to Prototype v2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FineProgressBar {
    pub value: f32,
    pub bg: u8,
}

impl FineProgressBar {
    pub fn new(value: f32, bg: u8) -> Self {
        Self { value, bg }
    }
}

impl Widget for FineProgressBar {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let d = text.density();
        c.constrain(Size::new(c.max.w, d.px(6)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let d = cx.text.density();
        let p = &mut *cx.painter;
        let radius = d.scale(3.0);
        let h = d.px(6);
        let y = r.y + (r.h - h) / 2;
        let bar_r = Rect::new(r.x, y, r.w, h);
        p.fill_round_rect(bar_r, radius, self.bg);
        let w = ((bar_r.w as f32 * self.value.clamp(0.0, 1.0)).round() as i32).max(0);
        if w > 0 {
            let fill_r = Rect::new(bar_r.x, bar_r.y, w, h);
            p.fill_round_rect(fill_r, radius, INK);
        }
    }
}

/// A vector bookmark corner ribbon according to Prototype v2 (48x64 or 36x48).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookmarkRibbon {
    pub bookmarked: bool,
    pub width: i32,
    pub height: i32,
}

impl BookmarkRibbon {
    pub fn new(bookmarked: bool, width: i32, height: i32) -> Self {
        Self {
            bookmarked,
            width,
            height,
        }
    }
}

impl Widget for BookmarkRibbon {
    fn measure(&mut self, c: Constraints, text: &mut TextEngine) -> Size {
        let d = text.density();
        c.constrain(Size::new(d.px(self.width), d.px(self.height)))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let d = cx.text.density();
        let p = &mut *cx.painter;
        let sx = r.w as f32 / 48.0;
        let sy = r.h as f32 / 64.0;
        let transform = Transform::scale(sx, sy).then_translate(r.x as f32, r.y as f32);
        let path = "M6 3H42V57L24 41L6 57Z";
        if self.bookmarked {
            p.fill_path(path, Some(transform), INK);
        } else {
            p.fill_path(path, Some(transform), WHITE);
            p.stroke_path(path, d.scale(2.0) / sx, Some(transform), INK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_are_panel_levels_with_contrast() {
        for (bg, ink) in BINDINGS {
            assert!(bg % 17 == 0 || bg == 0xf2);
            assert!(
                (bg as i32 - ink as i32).abs() >= 0x99,
                "{bg:#x} on {ink:#x}"
            );
        }
    }

    #[test]
    fn cjk_title_detection() {
        assert!(is_cjk_title("红楼梦"));
        assert!(is_cjk_title("紅樓夢"));
        assert!(is_cjk_title("三国演义"));
        assert!(is_cjk_title("阿Q正传"));
        assert!(!is_cjk_title("Pan Tadeusz"));
        assert!(!is_cjk_title("Lalka"));
        assert!(!is_cjk_title("Moby Dick"));
    }

    #[test]
    fn cjk_title_splitting_rules() {
        let (c1, c2) = split_cjk_title("红楼梦");
        assert_eq!(c1, vec!['红', '楼', '梦']);
        assert!(c2.is_empty());

        let (c1, c2) = split_cjk_title("三国演义");
        assert_eq!(c1, vec!['三', '国', '演', '义']);
        assert!(c2.is_empty());

        let (c1, c2) = split_cjk_title("傲慢与偏见");
        assert_eq!(c1, vec!['傲', '慢', '与']);
        assert_eq!(c2, vec!['偏', '见']);

        let (c1, c2) = split_cjk_title("中国哲学简史");
        assert_eq!(c1, vec!['中', '国', '哲']);
        assert_eq!(c2, vec!['学', '简', '史']);

        let (c1, c2) = split_cjk_title("一二三四五六七八");
        assert_eq!(c1, vec!['一', '二', '三', '四']);
        assert_eq!(c2, vec!['五', '六', '七', '八']);

        let (c1, c2) = split_cjk_title("一二三四五六七八九十");
        assert_eq!(c1, vec!['一', '二', '三', '四']);
        assert_eq!(c2, vec!['五', '六', '七', '…']);
    }

    #[test]
    fn cjk_cover_measures_without_crashing() {
        let fam = FamilyId(0);
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let mut cover = Cover::new("红楼梦", "曹雪芹", fam);
        // Measure large
        let s_large = cover.measure(Constraints::tight(Size::new(200, 300)), &mut text);
        assert_eq!(s_large, Size::new(200, 300));
        assert!(cover.strip_rect.is_some());
        assert!(!cover.cjk_chars.is_empty());

        // Measure small
        let s_small = cover.measure(Constraints::tight(Size::new(72, 106)), &mut text);
        assert_eq!(s_small, Size::new(72, 106));
        assert!(cover.strip_rect.is_some());
    }

    #[test]
    fn cjk_cover_bounds_home_card() {
        let fam = FamilyId(0);
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let size = Size::new(200, 300); // CARD_COVER in home.rs
        let cover_rect = Rect::new(0, 0, size.w, size.h);
        let titles = [
            ("红楼梦", "曹雪芹"),
            ("三国演义", "罗贯中"),
            ("傲慢与偏见", "简·奥斯汀"),
            ("超长中文书名超出两列截断", "佚名"),
            ("书", "作者"),
        ];
        for (title, author) in titles {
            let mut cover = Cover::new(title, author, fam);
            cover.measure(Constraints::tight(size), &mut text);
            let strip = cover
                .strip_rect()
                .expect("strip_rect must exist for CJK cover");
            assert!(
                cover_rect.contains_rect(&strip),
                "Strip {strip:?} must be inside cover {cover_rect:?} for '{title}' at {size:?}"
            );
            let chars = cover.cjk_chars();
            assert!(!chars.is_empty(), "chars must not be empty for '{title}'");
            for (layout, pt) in chars {
                let char_rect = Rect::new(
                    pt.x,
                    pt.y,
                    layout.width.ceil() as i32,
                    layout.height.ceil() as i32,
                );
                assert!(
                    strip.contains_rect(&char_rect),
                    "Char {char_rect:?} must be inside strip {strip:?} for '{title}' at {size:?}"
                );
            }
        }
    }

    #[test]
    fn cjk_cover_bounds_home_recent() {
        let fam = FamilyId(0);
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let size = Size::new(144, 192); // RECENT_COVER in home.rs
        let cover_rect = Rect::new(0, 0, size.w, size.h);
        let titles = [
            ("紅樓夢", "曹雪芹"),
            ("红楼梦", "曹雪芹"),
            ("三国演义", "罗贯中"),
            ("傲慢与偏见", "简·奥斯汀"),
            ("超长中文书名超出两列截断", "佚名"),
            ("书", "作者"),
        ];
        for (title, author) in titles {
            let mut cover = Cover::new(title, author, fam);
            cover.measure(Constraints::tight(size), &mut text);
            let strip = cover
                .strip_rect()
                .expect("strip_rect must exist for CJK cover");
            assert!(
                cover_rect.contains_rect(&strip),
                "Strip {strip:?} must be inside cover {cover_rect:?} for '{title}' at {size:?}"
            );
            let chars = cover.cjk_chars();
            assert!(!chars.is_empty(), "chars must not be empty for '{title}'");
            for (layout, pt) in chars {
                let char_rect = Rect::new(
                    pt.x,
                    pt.y,
                    layout.width.ceil() as i32,
                    layout.height.ceil() as i32,
                );
                assert!(
                    strip.contains_rect(&char_rect),
                    "Char {char_rect:?} must be inside strip {strip:?} for '{title}' at {size:?}"
                );
            }
        }
    }

    #[test]
    fn cjk_cover_bounds_library_thumbnail() {
        let fam = FamilyId(0);
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let sizes = [
            Size::new(108, 144), // ROW_COVER in library.rs
            Size::new(72, 106),  // compact / legacy ROW_COVER
        ];
        let titles = [
            ("紅樓夢", "曹雪芹"),
            ("红楼梦", "曹雪芹"),
            ("三国演义", "罗贯中"),
            ("傲慢与偏见", "简·奥斯汀"),
            ("超长中文书名超出两列截断", "佚名"),
            ("书", "作者"),
        ];
        for size in sizes {
            let cover_rect = Rect::new(0, 0, size.w, size.h);
            for (title, author) in titles {
                let mut cover = Cover::new(title, author, fam);
                cover.measure(Constraints::tight(size), &mut text);
                let strip = cover
                    .strip_rect()
                    .expect("strip_rect must exist for CJK cover");
                assert!(
                    cover_rect.contains_rect(&strip),
                    "Strip {strip:?} must be inside cover {cover_rect:?} for '{title}' at {size:?}"
                );
                let chars = cover.cjk_chars();
                assert!(!chars.is_empty(), "chars must not be empty for '{title}'");
                for (layout, pt) in chars {
                    let char_rect = Rect::new(
                        pt.x,
                        pt.y,
                        layout.width.ceil() as i32,
                        layout.height.ceil() as i32,
                    );
                    assert!(
                        strip.contains_rect(&char_rect),
                        "Char {char_rect:?} must be inside strip {strip:?} for '{title}' at {size:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn cjk_cover_bounds_library_grid() {
        let fam = FamilyId(0);
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let grid_sizes = [
            Size::new(180, 270), // docs/design/README.md
            Size::new(272, 363), // docs/ui/prototype-v2/mcode/DESIGN.md
            Size::new(156, 234), // HERO_COVER
        ];
        let titles = [
            ("红楼梦", "曹雪芹"),
            ("三国演义", "罗贯中"),
            ("傲慢与偏见", "简·奥斯汀"),
            ("超长中文书名超出两列截断", "佚名"),
            ("书", "作者"),
        ];
        for size in grid_sizes {
            let cover_rect = Rect::new(0, 0, size.w, size.h);
            for (title, author) in titles {
                let mut cover = Cover::new(title, author, fam);
                cover.measure(Constraints::tight(size), &mut text);
                let strip = cover
                    .strip_rect()
                    .expect("strip_rect must exist for CJK cover");
                assert!(
                    cover_rect.contains_rect(&strip),
                    "Strip {strip:?} must be inside cover {cover_rect:?} for '{title}' at {size:?}"
                );
                let chars = cover.cjk_chars();
                assert!(!chars.is_empty(), "chars must not be empty for '{title}'");
                for (layout, pt) in chars {
                    let char_rect = Rect::new(
                        pt.x,
                        pt.y,
                        layout.width.ceil() as i32,
                        layout.height.ceil() as i32,
                    );
                    assert!(
                        strip.contains_rect(&char_rect),
                        "Char {char_rect:?} must be inside strip {strip:?} for '{title}' at {size:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn cjk_cover_bounds_covers_sheet_small() {
        let fam = FamilyId(0);
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let size = Size::new(72, 106); // covers_sheet small thumbnail
        let cover_rect = Rect::new(0, 0, size.w, size.h);
        let titles = [
            ("红楼梦", "曹雪芹"),
            ("三国演义", "罗贯中"),
            ("傲慢与偏见", "简·奥斯汀"),
            ("超长中文书名超出两列截断", "佚名"),
        ];
        for (title, author) in titles {
            let mut cover = Cover::new(title, author, fam);
            cover.measure(Constraints::tight(size), &mut text);
            let strip = cover
                .strip_rect()
                .expect("strip_rect must exist for CJK cover");
            assert!(
                cover_rect.contains_rect(&strip),
                "Strip {strip:?} must be inside cover {cover_rect:?} for '{title}' at {size:?}"
            );
            let chars = cover.cjk_chars();
            assert!(!chars.is_empty(), "chars must not be empty for '{title}'");
            for (layout, pt) in chars {
                let char_rect = Rect::new(
                    pt.x,
                    pt.y,
                    layout.width.ceil() as i32,
                    layout.height.ceil() as i32,
                );
                assert!(
                    strip.contains_rect(&char_rect),
                    "Char {char_rect:?} must be inside strip {strip:?} for '{title}' at {size:?}"
                );
            }
        }
    }

    #[test]
    fn cjk_cover_bounds_extreme_small_fallback() {
        let fam = FamilyId(0);
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        // Small sizes that trigger character reduction or single initial character
        let extreme_sizes = [Size::new(60, 80), Size::new(48, 64), Size::new(36, 48)];
        for size in extreme_sizes {
            let cover_rect = Rect::new(0, 0, size.w, size.h);
            let mut cover = Cover::new("超长中文书名超出两列截断", "佚名", fam);
            cover.measure(Constraints::tight(size), &mut text);
            let strip = cover
                .strip_rect()
                .expect("strip_rect must exist for CJK cover");
            assert!(
                cover_rect.contains_rect(&strip),
                "Strip {strip:?} must be inside cover {cover_rect:?} at {size:?}"
            );
            let chars = cover.cjk_chars();
            assert!(!chars.is_empty());
            for (layout, pt) in chars {
                let char_rect = Rect::new(
                    pt.x,
                    pt.y,
                    layout.width.ceil() as i32,
                    layout.height.ceil() as i32,
                );
                assert!(
                    strip.contains_rect(&char_rect),
                    "Char {char_rect:?} must be inside strip {strip:?} at {size:?}"
                );
            }
        }
    }

    #[test]
    fn strip_type_matches_the_prototype() {
        // docs/ui/prototype-v2/reference/home.png: 32 px type, the strip
        // 12 px from the top left corner of a 144x192 cover.
        let home = strip_type(144.0);
        assert_eq!(home.font, 32.0);
        assert_eq!(home.margin_x, 12.0);
        assert_eq!(home.margin_y, 12.0);
        assert_eq!(home.pad_x, 6.0);
        assert_eq!(home.pad_y, 8.0);
        assert_eq!(home.slack_w, 24.0);
        assert_eq!(home.slack_h, 28.0);

        // library.png: 28 px type, the strip 10 px in on a 108x144 cover.
        let lib = strip_type(108.0);
        assert_eq!(lib.font, 28.0);
        assert_eq!(lib.margin_x, 10.0);
        assert_eq!(lib.margin_y, 10.0);
        assert_eq!(lib.pad_x, 4.0);
        assert_eq!(lib.pad_y, 6.0);
        assert_eq!(lib.slack_w, 24.0);
        assert_eq!(lib.slack_h, 28.0);

        // A smaller cover takes the thumbnail's design, scaled down.
        let small = strip_type(72.0);
        assert_eq!(small.font, 19.0);
        assert_eq!(small.margin_x, 7.0);
        assert_eq!(small.margin_y, 7.0);
        assert_eq!(small.pad_x, 3.0);
        assert_eq!(small.pad_y, 4.0);
        assert_eq!(small.slack_w, 16.0);
        assert_eq!(small.slack_h, 19.0);
    }

    /// The strip of a cover, measured the way the prototype draws it.
    fn strip_of(title: &str, size: Size, text: &mut TextEngine) -> (Rect, f32) {
        let mut cover = Cover::new(title, "佚名", FamilyId(0));
        cover.measure(Constraints::tight(size), text);
        let strip = cover.strip_rect().expect("strip_rect must exist");
        // The frame a light card and its strip share.
        let (bg, _) = cover.binding();
        let frame = if bg == WHITE || bg == 0xf2 { 2.0 } else { 0.0 };
        (strip, frame)
    }

    #[test]
    fn cjk_strip_matches_the_prototype_geometry() {
        let mut text = TextEngine::new(crate::text::Fonts::embedded());

        // 144x192: one column of 32 px type in a 56x124 strip, 12 px in.
        let (strip, frame) = strip_of("红楼梦", Size::new(144, 192), &mut text);
        assert_eq!(strip.w, 56);
        assert_eq!(strip.h, 124);
        assert_eq!(strip.x, frame as i32 + 12);
        assert_eq!(strip.y, frame as i32 + 12);

        // A five character title splits into two columns of three and two.
        let (strip, _) = strip_of("傲慢与偏见", Size::new(144, 192), &mut text);
        assert_eq!(strip.w, 88);
        assert_eq!(strip.h, 124);

        // 108x144: one column of 28 px type in a 52x112 strip, 10 px in.
        let (strip, frame) = strip_of("红楼梦", Size::new(108, 144), &mut text);
        assert_eq!(strip.w, 52);
        assert_eq!(strip.h, 112);
        assert_eq!(strip.x, frame as i32 + 10);
        assert_eq!(strip.y, frame as i32 + 10);

        // The library list thumbnail is smaller than the prototype's, and
        // takes the same design scaled down.
        let (strip, frame) = strip_of("红楼梦", Size::new(72, 106), &mut text);
        assert_eq!(strip.w, 35);
        assert_eq!(strip.h, 76);
        assert_eq!(strip.x, frame as i32 + 7);
        assert_eq!(strip.y, frame as i32 + 7);
    }

    #[test]
    fn cjk_strip_truncates_before_it_shrinks() {
        let mut text = TextEngine::new(crate::text::Fonts::embedded());
        let mut cover = Cover::new("超长中文书名超出两列截断", "佚名", FamilyId(0));
        cover.measure(Constraints::tight(Size::new(144, 192)), &mut text);
        let strip = cover.strip_rect().expect("strip_rect must exist");
        // The title is cut to two columns of four, the eighth an ellipsis,
        // and the type keeps the prototype's 32 px: four rows of type and
        // the prototype's slack make a 156 px strip.
        assert_eq!(strip.w, 88);
        assert_eq!(strip.h, 156);
        assert_eq!(cover.cjk_chars().len(), 8);
    }

    #[test]
    fn a_shelf_shares_one_binding() {
        let fam = FamilyId(0);
        let mark = CoverMark {
            group: Some("Mickiewicz, Adam".into()),
            series_index: None,
        };
        let a = Cover::new("Pan Tadeusz", "Adam Mickiewicz", fam).with_mark(mark.clone());
        let b = Cover::new("Dziady", "Adam Mickiewicz", fam).with_mark(mark);
        assert_eq!(a.binding(), b.binding());
    }

    #[test]
    fn series_numbers() {
        assert_eq!(series_number(3.0), "3");
        assert_eq!(series_number(2.5), "2.5");
    }

    #[test]
    fn resampling_keeps_a_flat_cover_flat_and_the_mean_of_a_gradient() {
        let flat = GrayImage::new(40, 60, vec![200; 40 * 60]);
        for (w, h) in [(71, 93), (13, 19), (40, 30)] {
            let r = flat.resized(w, h);
            assert_eq!((r.width, r.height), (w, h));
            assert!(
                r.pixels.iter().all(|&p| (p as i32 - 200).abs() <= 1),
                "{w}x{h} is not flat"
            );
        }
        // A horizontal gradient keeps its ends and its middle.
        let mut px = Vec::new();
        for _ in 0..60 {
            for x in 0..40u8 {
                px.push(x * 6);
            }
        }
        let g = GrayImage::new(40, 60, px).resized(20, 60);
        let mean = |a: usize, b: usize| {
            let mut sum = 0u32;
            let mut n = 0u32;
            for y in 0..60 {
                for x in a..b {
                    sum += g.pixels[y * 20 + x] as u32;
                    n += 1;
                }
            }
            sum as f32 / n as f32
        };
        assert!((mean(0, 2) - 9.0).abs() < 6.0, "{}", mean(0, 2));
        assert!((mean(18, 20) - 225.0).abs() < 6.0, "{}", mean(18, 20));
    }

    #[test]
    fn a_thumbnail_is_not_enlarged_into_a_slightly_bigger_box() {
        let img = GrayImage::new(200, 300, vec![128; 200 * 300]);
        // The home card's 240 × 320 box: fit is 213 × 320, a sixth taller
        // than the thumbnail, so it is drawn as it is.
        assert_eq!(drawn_size(&img, Size::new(240, 320)), Size::new(200, 300));
        // The grid's 272 × 363 box is much bigger: the thumbnail is scaled
        // up, as the prototype enlarges covers there.
        assert_eq!(drawn_size(&img, Size::new(272, 363)), Size::new(242, 363));
        // A smaller box is always resampled: it must fit.
        assert_eq!(drawn_size(&img, Size::new(108, 144)), Size::new(96, 144));
        // An image that already fills the box exactly is not touched.
        assert_eq!(drawn_size(&img, Size::new(200, 300)), Size::new(200, 300));
    }

    #[test]
    fn progress_bar_measures_and_initializes() {
        let pb = ProgressBar::new(40);
        assert_eq!(pb.percent, 40);
        let fpb = FineProgressBar::new(0.43, 0xf2);
        assert_eq!(fpb.value, 0.43);
        let bmr = BookmarkRibbon::new(true, 48, 64);
        assert!(bmr.bookmarked);
    }
}
