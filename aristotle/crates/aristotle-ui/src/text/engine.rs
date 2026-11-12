// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shaping and glyph rasterization with `swash`.
//!
//! Shaping runs OpenType GSUB and GPOS, so kerning pairs, ligatures and
//! mark positioning come from the font itself rather than from the legacy
//! `kern` table only. Rasterization uses swash's scaler with hinting on.
//! swash 0.2 hints through skrifa in its "smooth" mode, which snaps vertical
//! features (baseline, x-height, stems of horizontals) to the pixel grid but
//! keeps horizontal metrics linear. That is the right trade on a 300 DPI
//! panel: crisper x-height and baseline in 16 gray levels, while justified
//! lines keep exact widths and glyphs can sit at quarter-pixel positions.

use super::embolden;
use super::font::{FaceId, FamilyId, Fonts, ResolvedFace};
use crate::canvas::Painter;
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use swash::scale::{Render, ScaleContext, Source};
use swash::shape::ShapeContext;
use swash::text::{Language, Script};
use swash::zeno::{Angle, Format, Mask, Origin, Scratch, Transform, Vector, Verb};

/// How a piece of text looks.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    density: crate::geom::Density,
    pub family: FamilyId,
    /// Font size in native pixels after applying the screen density.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    /// Ink level, 0 black to 255 white.
    pub gray: u8,
    /// Line pitch as a multiple of `size`.
    pub line_height: f32,
    /// Extra space after every character, in pixels. Small caps and
    /// all-caps labels need a little to stay legible.
    pub tracking: f32,
    /// OpenType small capitals (`smcp`).
    pub small_caps: bool,
    /// Old-style (lowercase) figures (`onum`), which sit better in running
    /// text; UI numbers keep the default lining figures.
    pub oldstyle: bool,
}

impl TextStyle {
    pub fn new(family: FamilyId, size: f32) -> Self {
        Self {
            density: crate::geom::Density::default(),
            family,
            size,
            bold: false,
            italic: false,
            gray: 0,
            line_height: 1.35,
            tracking: 0.0,
            small_caps: false,
            oldstyle: false,
        }
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub fn gray(mut self, gray: u8) -> Self {
        self.gray = gray;
        self
    }

    pub fn line_height(mut self, lh: f32) -> Self {
        self.line_height = lh;
        self
    }

    pub fn tracking(mut self, t: f32) -> Self {
        self.tracking = self.density.scale(t);
        self
    }

    pub fn small_caps(mut self) -> Self {
        self.small_caps = true;
        self
    }

    pub fn oldstyle(mut self) -> Self {
        self.oldstyle = true;
        self
    }

    /// Sets the size in reference pixels at 300 DPI.
    pub fn size(mut self, size: f32) -> Self {
        self.size = self.density.scale(size);
        self
    }

    /// Sets an already measured native pixel size.
    pub fn size_pixels(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Changes density without scaling native metrics twice.
    pub fn at_density(mut self, density: crate::geom::Density) -> Self {
        let ratio = density.dpi() as f32 / self.density.dpi() as f32;
        self.size *= ratio;
        self.tracking *= ratio;
        self.density = density;
        self
    }
}

/// Vertical metrics of a style, in pixels.
#[derive(Clone, Copy, Debug)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
    /// Distance between baselines.
    pub pitch: f32,
    pub x_height: f32,
    pub cap_height: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct RunGlyph {
    pub id: u16,
    /// The face the glyph comes from: the style's family, or a fallback
    /// family for the characters it has no glyph for.
    pub face: ResolvedFace,
    /// Pen position relative to the start of the run, plus GPOS offset.
    pub x: f32,
    pub y: f32,
}

/// One cluster: the smallest unit text can be split at. Ligatures are one
/// cluster, so a hyphenation point inside "ffi" is never taken.
#[derive(Clone, Debug)]
pub struct RunCluster {
    /// Byte range in the shaped string.
    pub source: Range<u32>,
    pub x: f32,
    pub advance: f32,
    pub glyphs: Range<u32>,
}

/// The result of shaping one string in one style.
#[derive(Clone, Debug)]
pub struct ShapedRun {
    pub face: ResolvedFace,
    pub size: f32,
    pub gray: u8,
    pub glyphs: Vec<RunGlyph>,
    pub clusters: Vec<RunCluster>,
    pub width: f32,
    /// The shaped string, kept by paragraph layout for ellipsis and
    /// debugging.
    pub text: Option<String>,
    /// Index of the span style this run came from, within its paragraph.
    pub style: usize,
}

impl ShapedRun {
    /// Index of the cluster starting at byte `offset`, if a cluster boundary
    /// is there.
    pub fn cluster_at(&self, offset: u32) -> Option<usize> {
        self.clusters
            .iter()
            .position(|c| c.source.start == offset)
            .or_else(|| {
                (self.clusters.last().map(|c| c.source.end) == Some(offset))
                    .then_some(self.clusters.len())
            })
    }

    /// Width of the clusters in `range`.
    pub fn width_of(&self, range: Range<usize>) -> f32 {
        if range.is_empty() {
            return 0.0;
        }
        let first = &self.clusters[range.start];
        let last = &self.clusters[range.end - 1];
        last.x + last.advance - first.x
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: FaceId,
    glyph: u16,
    /// Size in 1/16 px.
    size: u32,
    /// Horizontal subpixel phase in quarter pixels.
    phase: u8,
    embolden: bool,
    slant: bool,
}

struct GlyphImage {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

const GLYPH_CACHE_BYTES: usize = 4 * 1024 * 1024;
const GLYPH_CACHE_ENTRIES: usize = 4096;

/// Shapes, measures and draws text. Owns the fonts and the glyph cache.
pub struct TextEngine {
    pub fonts: Fonts,
    pub(crate) density: crate::geom::Density,
    shape: ShapeContext,
    scale: ScaleContext,
    cache: HashMap<GlyphKey, Option<GlyphImage>>,
    cache_order: VecDeque<GlyphKey>,
    cache_bytes: usize,
    uncached_glyph: Option<GlyphImage>,
    language: Option<Language>,
    /// Coverage curve applied when glyphs are rasterized; see
    /// [`TextEngine::set_darkening`].
    lut: [u8; 256],
}

impl TextEngine {
    pub fn new(fonts: Fonts) -> Self {
        let mut e = Self {
            fonts,
            density: crate::geom::Density::default(),
            shape: ShapeContext::new(),
            scale: ScaleContext::new(),
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            cache_bytes: 0,
            uncached_glyph: None,
            language: Language::parse("pl"),
            lut: [0; 256],
        };
        e.set_darkening(0.8);
        e
    }

    pub fn density(&self) -> crate::geom::Density {
        self.density
    }

    /// Sets the language used for `locl` shaping features (default Polish).
    pub fn set_language(&mut self, tag: &str) {
        self.language = Language::parse(tag);
        self.clear_cache();
    }

    /// Applies `coverage^gamma` to anti-aliased edges. Below 1.0 thickens
    /// strokes slightly. E-ink shows only 16 levels and its light grays
    /// wash out under the frontlight, so unadjusted text looks thin and
    /// gray next to the stock reader; 0.8 restores the weight without
    /// filling counters at small sizes.
    pub fn set_darkening(&mut self, gamma: f32) {
        for (i, v) in self.lut.iter_mut().enumerate() {
            *v = (255.0 * (i as f32 / 255.0).powf(gamma)).round() as u8;
        }
        self.clear_cache();
    }

    fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_order.clear();
        self.cache_bytes = 0;
        self.uncached_glyph = None;
    }

    pub fn metrics(&self, style: &TextStyle) -> LineMetrics {
        let face = self.fonts.resolve(style.family, style.bold, style.italic);
        let m = self.fonts.face(face.face).metrics.scale(style.size);
        LineMetrics {
            ascent: m.ascent,
            descent: m.descent,
            pitch: (style.size * style.line_height).round(),
            x_height: m.x_height,
            cap_height: m.cap_height,
        }
    }

    /// Shapes `text` in `style`. Advances include `style.tracking`.
    ///
    /// A character the style's family has no glyph for is shaped with the
    /// first fallback family that has one, so Chinese in a Latin family
    /// shows instead of a box. The run keeps one baseline and one advance
    /// chain: the fallback only changes which face a glyph comes from.
    pub fn shape(&mut self, text: &str, style: &TextStyle) -> ShapedRun {
        let primary = self.fonts.resolve(style.family, style.bold, style.italic);
        let mut glyphs = Vec::new();
        let mut clusters = Vec::new();
        let mut pen = 0.0f32;
        for (start, segment, face) in self.split_by_face(text, style, primary) {
            pen = self.shape_segment(
                segment,
                start,
                style,
                face,
                script_of(segment),
                pen,
                &mut glyphs,
                &mut clusters,
            );
        }
        ShapedRun {
            face: primary,
            size: style.size,
            gray: style.gray,
            glyphs,
            clusters,
            width: pen,
            text: None,
            style: 0,
        }
    }

    /// The stretches of `text` that one face shapes: the style's family
    /// for the characters it has glyphs for, the first fallback family
    /// with a glyph for the rest.
    fn split_by_face<'a>(
        &self,
        text: &'a str,
        style: &TextStyle,
        primary: ResolvedFace,
    ) -> Vec<(usize, &'a str, ResolvedFace)> {
        let mut out: Vec<(usize, &str, ResolvedFace)> = Vec::new();
        let mut start = 0usize;
        let mut face = primary;
        for (i, ch) in text.char_indices() {
            let f = self.face_for(style, primary, ch);
            if i == start {
                face = f;
            } else if f != face {
                out.push((start, &text[start..i], face));
                start = i;
                face = f;
            }
        }
        if start < text.len() {
            out.push((start, &text[start..], face));
        }
        out
    }

    /// The face a character is shaped with: the style's family when it
    /// has a glyph for it, the first fallback family that has one
    /// otherwise, and the style's family again when none has it.
    fn face_for(&self, style: &TextStyle, primary: ResolvedFace, ch: char) -> ResolvedFace {
        if self.has_glyph(primary, ch) {
            return primary;
        }
        for family in self.fonts.fallbacks() {
            let face = self.fonts.resolve(*family, style.bold, style.italic);
            if self.has_glyph(face, ch) {
                return face;
            }
        }
        primary
    }

    /// True if the face has a glyph for the character. Glyph 0 is the
    /// font's .notdef, the box a character without a glyph shows.
    fn has_glyph(&self, face: ResolvedFace, ch: char) -> bool {
        self.fonts.face(face.face).font_ref().charmap().map(ch) != 0
    }

    /// Shapes one stretch of text with one face, appending its glyphs and
    /// clusters to the run. Returns the pen position after it.
    #[allow(clippy::too_many_arguments)]
    fn shape_segment(
        &mut self,
        text: &str,
        start: usize,
        style: &TextStyle,
        face: ResolvedFace,
        script: Script,
        mut pen: f32,
        glyphs: &mut Vec<RunGlyph>,
        clusters: &mut Vec<RunCluster>,
    ) -> f32 {
        let font = self.fonts.face(face.face).font_ref();
        let mut features: Vec<(&str, u16)> = vec![("kern", 1), ("liga", 1)];
        if style.small_caps {
            features.push(("smcp", 1));
        }
        if style.oldstyle {
            features.push(("onum", 1));
        }
        let mut shaper = self
            .shape
            .builder(font)
            .script(script)
            .language(self.language)
            .size(style.size)
            .features(features.iter().copied())
            .build();
        shaper.add_str(text);
        // Synthetic bold widens every glyph a little; keep advances in step
        // so emboldened words do not collide.
        let extra = if face.embolden {
            style.size / 40.0
        } else {
            0.0
        };
        shaper.shape_with(|c| {
            let first = glyphs.len() as u32;
            let x = pen;
            for g in c.glyphs {
                glyphs.push(RunGlyph {
                    id: g.id,
                    face,
                    x: pen + g.x,
                    y: g.y,
                });
                pen += g.advance;
            }
            if !c.glyphs.is_empty() {
                pen += style.tracking + extra;
            }
            clusters.push(RunCluster {
                source: (start + c.source.start as usize) as u32
                    ..(start + c.source.end as usize) as u32,
                x,
                advance: pen - x,
                glyphs: first..glyphs.len() as u32,
            });
        });
        pen
    }

    /// Advance width of `text` in `style`.
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> f32 {
        self.shape(text, style).width
    }

    /// Draws clusters `range` of `run` with the first one's pen position at
    /// `x` and the baseline at `baseline`.
    pub fn draw(
        &mut self,
        p: &mut Painter,
        run: &ShapedRun,
        range: Range<usize>,
        x: f32,
        baseline: f32,
    ) {
        if range.is_empty() {
            return;
        }
        let x0 = x - run.clusters[range.start].x;
        let g0 = run.clusters[range.start].glyphs.start;
        let g1 = run.clusters[range.end - 1].glyphs.end;
        let by = baseline.round() as i32;
        for g in &run.glyphs[g0 as usize..g1 as usize] {
            let gx = x0 + g.x;
            let ix = gx.floor();
            let phase = ((gx - ix) * 4.0).floor() as u8;
            let key = GlyphKey {
                face: g.face.face,
                glyph: g.id,
                size: (run.size * 16.0).round() as u32,
                phase,
                embolden: g.face.embolden,
                slant: g.face.slant,
            };
            let Some(img) = self.glyph(key) else { continue };
            p.blend_mask(
                ix as i32 + img.left,
                by - (g.y.round() as i32) - img.top,
                img.width,
                img.height,
                &img.data,
                run.gray,
            );
        }
    }

    fn glyph(&mut self, key: GlyphKey) -> Option<&GlyphImage> {
        self.uncached_glyph = None;
        if !self.cache.contains_key(&key) {
            let img = self.rasterize(key);
            return self.cache_glyph(key, img);
        }
        self.cache.get(&key).and_then(|g| g.as_ref())
    }

    fn cache_glyph(&mut self, key: GlyphKey, img: Option<GlyphImage>) -> Option<&GlyphImage> {
        debug_assert!(!self.cache.contains_key(&key));
        let bytes = img.as_ref().map_or(0, |g| g.data.capacity());
        self.uncached_glyph = None;
        if bytes > GLYPH_CACHE_BYTES {
            self.uncached_glyph = img;
            return self.uncached_glyph.as_ref();
        }
        while self.cache_bytes + bytes > GLYPH_CACHE_BYTES
            || self.cache.len() >= GLYPH_CACHE_ENTRIES
        {
            let oldest = self
                .cache_order
                .pop_front()
                .expect("glyph cache entry has a queue slot");
            if let Some(Some(old)) = self.cache.remove(&oldest) {
                self.cache_bytes -= old.data.capacity();
            }
        }
        self.cache_bytes += bytes;
        self.cache_order.push_back(key);
        self.cache.insert(key, img);
        self.cache.get(&key).and_then(|g| g.as_ref())
    }

    fn rasterize(&mut self, key: GlyphKey) -> Option<GlyphImage> {
        if key.embolden {
            return self.rasterize_bold(key);
        }
        let font = self.fonts.face(key.face).font_ref();
        let mut scaler = self
            .scale
            .builder(font)
            .size(key.size as f32 / 16.0)
            .hint(true)
            .build();
        let mut render = Render::new(&[Source::Outline]);
        render
            .format(Format::Alpha)
            .offset(Vector::new(key.phase as f32 / 4.0, 0.0));
        if key.slant {
            render.transform(Some(Transform::skew(
                Angle::from_degrees(12.0),
                Angle::ZERO,
            )));
        }
        let img = render.render(&mut scaler, key.glyph)?;
        let data = img.data.iter().map(|&a| self.lut[a as usize]).collect();
        Some(GlyphImage {
            left: img.placement.left,
            top: img.placement.top,
            width: img.placement.width,
            height: img.placement.height,
            data,
        })
    }

    /// Rasterizes a glyph that has no bold face of its own, fattened.
    ///
    /// The outline is pushed out here rather than by `swash`, whose own
    /// emboldening takes the direction of the push from the glyph as a whole
    /// and so thins the glyphs whose strokes balance out. See
    /// [`crate::text::embolden`].
    fn rasterize_bold(&mut self, key: GlyphKey) -> Option<GlyphImage> {
        let font = self.fonts.face(key.face).font_ref();
        let size = key.size as f32 / 16.0;
        let mut scaler = self.scale.builder(font).size(size).hint(true).build();
        let mut outline = scaler.scale_outline(key.glyph)?;
        {
            let verbs: Vec<Verb> = outline.verbs().to_vec();
            embolden::offset(outline.points_mut(), &verbs, size / 40.0);
        }
        if key.slant {
            outline.transform(&Transform::skew(Angle::from_degrees(12.0), Angle::ZERO));
        }
        let shift = Vector::new(key.phase as f32 / 4.0, 0.0);
        let mut scratch = Scratch::new();
        let mut mask = Mask::with_scratch(outline.path(), &mut scratch);
        mask.format(Format::Alpha)
            .origin(Origin::BottomLeft)
            .offset(shift)
            .render_offset(shift);
        let mut data = Vec::new();
        mask.inspect(|fmt, w, h| {
            data.resize(fmt.buffer_size(w, h), 0);
        });
        let placement = mask.render_into(&mut data, None);
        let data = data.iter().map(|&a| self.lut[a as usize]).collect();
        Some(GlyphImage {
            left: placement.left,
            top: placement.top,
            width: placement.width,
            height: placement.height,
            data,
        })
    }

    /// True if every character of `text` maps to a real glyph in the face
    /// used for `style`, or in a fallback family. Used to check diacritic
    /// and Chinese coverage.
    pub fn covers(&self, text: &str, style: &TextStyle) -> bool {
        let primary = self.fonts.resolve(style.family, style.bold, style.italic);
        text.chars().all(|c| {
            self.has_glyph(primary, c)
                || self.fonts.fallbacks().iter().any(|family| {
                    self.has_glyph(self.fonts.resolve(*family, style.bold, style.italic), c)
                })
        })
    }
}

/// The script a stretch of text is shaped with: Han for Chinese, the Latin
/// script for everything else, as before.
fn script_of(text: &str) -> Script {
    if text.chars().any(is_cjk) {
        Script::Han
    } else {
        Script::Latin
    }
}

/// True for the characters of the CJK scripts: the ideographs, the
/// punctuation, the kana and the fullwidth forms.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2EFF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x312F
        | 0x31A0..=0x31BF | 0x3200..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF
        | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x2F800..=0x2FA1F)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::font::{FamilyId, FontData, Fonts};
    use crate::text::hyphen::NoHyphenation;
    use crate::text::layout::{layout_paragraph, Align, Paragraph};

    /// Noto Sans SC cut to the characters of the test string: the fixture
    /// for the fallback, small enough to keep in the tree (see
    /// assets/test-fonts/README.md).
    const CJK: &[u8] = include_bytes!("../../assets/test-fonts/CjkFixture-Regular.ttf");

    /// The family the interface uses, and the Chinese fallback behind it.
    fn fonts() -> (Fonts, FamilyId, FaceId, FaceId) {
        let mut fonts = Fonts::new();
        let literata = fonts
            .add_face(FontData::Static(include_bytes!(
                "../../assets/fonts/Literata-Regular.ttf"
            )))
            .expect("Literata is valid");
        let cjk = fonts
            .add_face(FontData::Static(CJK))
            .expect("the fixture is valid");
        let literata_family = fonts
            .add_family("Literata", [Some(literata), None, None, None])
            .expect("Literata has a regular face");
        let cjk_family = fonts
            .add_family("Noto Sans SC", [Some(cjk), None, None, None])
            .expect("the fixture has a regular face");
        fonts.add_fallback(cjk_family);
        (fonts, literata_family, literata, cjk)
    }

    fn cache_key(glyph: u16) -> GlyphKey {
        GlyphKey {
            face: FaceId(0),
            glyph,
            size: 32 * 16,
            phase: 0,
            embolden: false,
            slant: false,
        }
    }

    fn cache_image(bytes: usize) -> GlyphImage {
        GlyphImage {
            left: 0,
            top: 0,
            width: bytes as u32,
            height: 1,
            data: vec![42; bytes],
        }
    }

    #[test]
    fn glyph_cache_bounds_pixels_and_empty_entries() {
        let (fonts, ..) = fonts();
        let mut engine = TextEngine::new(fonts);
        for glyph in 0..10_000 {
            engine.cache_glyph(cache_key(glyph), Some(cache_image(2048)));
            assert!(engine.cache_bytes <= GLYPH_CACHE_BYTES);
            assert!(engine.cache.len() <= GLYPH_CACHE_ENTRIES);
            assert_eq!(engine.cache.len(), engine.cache_order.len());
        }
        assert!(!engine.cache.contains_key(&cache_key(0)));
        for glyph in 10_000..20_000 {
            engine.cache_glyph(cache_key(glyph), None);
        }
        assert_eq!(engine.cache_bytes, 0);
        assert_eq!(engine.cache.len(), GLYPH_CACHE_ENTRIES);
        assert_eq!(engine.cache_order.len(), GLYPH_CACHE_ENTRIES);
    }

    #[test]
    fn oversized_glyph_still_draws_and_invalidation_resets_accounting() {
        let (fonts, ..) = fonts();
        let mut engine = TextEngine::new(fonts);
        engine.cache_glyph(cache_key(1), Some(cache_image(32)));
        let image = engine
            .cache_glyph(cache_key(2), Some(cache_image(GLYPH_CACHE_BYTES + 1)))
            .unwrap();
        assert_eq!(image.data.len(), GLYPH_CACHE_BYTES + 1);
        assert_eq!(engine.cache_bytes, 32);
        assert!(!engine.cache.contains_key(&cache_key(2)));
        engine.set_language("zh");
        assert!(engine.cache.is_empty());
        assert!(engine.cache_order.is_empty());
        assert_eq!(engine.cache_bytes, 0);
        assert!(engine.uncached_glyph.is_none());
        engine.cache_glyph(cache_key(3), Some(cache_image(64)));
        engine.set_darkening(0.9);
        assert!(engine.cache.is_empty());
        assert!(engine.cache_order.is_empty());
        assert_eq!(engine.cache_bytes, 0);
    }

    #[test]
    fn evicted_glyph_rasterizes_to_identical_pixels() {
        let (fonts, _, face, _) = fonts();
        let mut engine = TextEngine::new(fonts);
        let mut key = cache_key(36);
        key.face = face;
        let before = engine.glyph(key).unwrap().data.clone();
        for glyph in 0..GLYPH_CACHE_ENTRIES as u16 {
            let mut other = cache_key(glyph);
            other.phase = 1;
            engine.cache_glyph(other, None);
        }
        assert!(!engine.cache.contains_key(&key));
        assert_eq!(engine.glyph(key).unwrap().data, before);
    }

    /// Chinese in a Latin family: every character has a real glyph, and
    /// the Latin ones still come from the first family.
    #[test]
    fn chinese_falls_back_to_the_second_family() {
        let (fonts, literata, literata_face, cjk_face) = fonts();
        let mut engine = TextEngine::new(fonts);
        let style = TextStyle::new(literata, 32.0);
        let text = "紅樓夢 Dream 墨阅排版样张，";
        let run = engine.shape(text, &style);
        assert!(!run.glyphs.is_empty());
        for glyph in &run.glyphs {
            assert_ne!(glyph.id, 0, "a character has no glyph: {text}");
        }
        // Every cluster knows the face its glyphs come from: the Latin
        // ones stay in Literata, the Chinese come from the fallback.
        for cluster in &run.clusters {
            let chunk = &text[cluster.source.start as usize..cluster.source.end as usize];
            let expected = if chunk.is_ascii() {
                literata_face
            } else {
                cjk_face
            };
            for glyph in &run.glyphs[cluster.glyphs.start as usize..cluster.glyphs.end as usize] {
                assert_eq!(glyph.face.face, expected, "wrong face for {chunk}");
            }
        }
    }

    /// The same after a layout: the paragraph breaks into lines, and the
    /// glyphs still come from the right faces.
    #[test]
    fn chinese_survives_the_layout() {
        let (fonts, literata, literata_face, cjk_face) = fonts();
        let mut engine = TextEngine::new(fonts);
        let style = TextStyle::new(literata, 32.0);
        let paragraph = Paragraph::plain("紅樓夢 Dream 墨阅排版样张，", style, Align::Left);
        let layout = layout_paragraph(&mut engine, &paragraph, 600.0, &NoHyphenation);
        assert!(!layout.lines.is_empty());
        let mut chinese = 0;
        let mut latin = 0;
        for run in &layout.runs {
            for glyph in &run.glyphs {
                assert_ne!(glyph.id, 0, "a character has no glyph");
                match glyph.face.face {
                    f if f == literata_face => latin += 1,
                    f if f == cjk_face => chinese += 1,
                    f => panic!("glyph from an unknown face: {f:?}"),
                }
            }
        }
        assert!(chinese > 0, "no Chinese glyph in the layout");
        assert!(latin > 0, "no Latin glyph in the layout");
    }

    /// The fallback does not change the line height: the metrics come
    /// from the first family, so a Chinese line is as tall as a Latin
    /// one.
    #[test]
    fn the_fallback_keeps_the_line_height() {
        let (fonts, literata, _, _) = fonts();
        let mut engine = TextEngine::new(fonts);
        let style = TextStyle::new(literata, 32.0);
        let metrics = engine.metrics(&style);
        assert_eq!(metrics.pitch, (style.size * style.line_height).round());
        let latin = engine.shape("Dream", &style);
        let mixed = engine.shape("紅樓夢 Dream", &style);
        assert!(
            mixed.width > latin.width,
            "the Chinese glyphs have no width"
        );
        // The glyphs of both runs sit on the same baseline: the fallback
        // only changes the face, not the position.
        for run in [&latin, &mixed] {
            for glyph in &run.glyphs {
                assert_eq!(glyph.y, 0.0, "a glyph sits off the baseline");
            }
        }
    }
}
