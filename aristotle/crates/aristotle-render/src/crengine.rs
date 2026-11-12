// SPDX-License-Identifier: AGPL-3.0-or-later
//! Laying a book out with crengine-ng, through the readest engine crate.
//!
//! Behind the `crengine` cargo feature (off by default). crengine-ng has its
//! own CSS engine and font handling: it lays the book out once and hands out
//! pages as 8-bit grayscale buffers. Aristotle's side of the deal is small:
//! the geometry (see [`crate::style::Style`]), the fonts, and the conversion
//! of a grayscale buffer into a [`crate::page::Page`].
//!
//! Unlike Plato's engine, crengine addresses a place in the book with an
//! XPointer (a path into the book's DOM), not with a page number, so the
//! engine's positions survive a re-layout: see [`crate::book::Position`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{
    Document as CrengineDocument, Engine as _, LayoutSettings, Margins, TocEntry, MAX_FONT_SIZE_PX,
    MAX_LINE_SPACING_PERCENT, MAX_MARGIN_PX, MIN_FONT_SIZE_PX, MIN_LINE_SPACING_PERCENT,
};
use inkread_platform::GrayBuffer;
use plato_core::anyhow::{format_err, Error};

use crate::style::Style;

/// The fonts crengine registers, relative to the data directory.
///
/// The serif is the Literata the rest of Aristotle uses; the Chinese face is
/// the merged DejaVu Sans + Noto Sans SC file the UI package ships.
const FONT_FILES: &[&str] = &[
    "serif/Literata-Regular.ttf",
    "serif/Literata-Italic.ttf",
    "serif/Literata-Bold.ttf",
    "serif/Literata-BoldItalic.ttf",
    "fonts/DejaVuSans-Symbols.ttf",
];

/// The face the body text is set in.
const LATIN_FACE: &str = "Literata";

/// The face Chinese, Japanese and Korean glyphs are set in.
const CJK_FACE: &str = "Noto Sans SC";

/// The directory the document cache lives in, under the library's data
/// directory.
pub const CACHE_DIR: &str = "crengine-cache";

/// How much space the document cache may use, in bytes.
///
/// 64 MB is about forty books' worth of cache files: a 1.2 MB EPUB lays out
/// to roughly 1.5 MB of cache. The cache is a cache: when it grows past this
/// the oldest books are dropped from it.
pub const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Turn on crengine's document cache, under the library's data directory.
///
/// The cache holds the parsed document, so opening the same book again does
/// not have to parse it a second time; crengine also keeps the layout in
/// there and reuses it when the render context still matches. The library's
/// data directory is the right place for it: it is emptied with the rest of
/// the library's data, and it is on the device's own storage rather than in
/// the slow system partition.
///
/// The cache is process-wide, so this is called once, when the app starts.
/// Not being able to set it up is not fatal: the reader lays books out the
/// slow way, as it did before.
pub fn configure_cache(data_dir: &Path) -> Result<(), Error> {
    let dir = data_dir.join(CACHE_DIR);
    inkread_engine::crengine::configure_cache(&dir, CACHE_MAX_BYTES)
        .map_err(|e| format_err!("crengine cache: {e}"))
}

/// A book laid out by crengine-ng.
pub struct CrengineDoc {
    doc: Box<dyn CrengineDocument>,
    /// The layout the book was laid out with.
    layout: LayoutSettings,
    /// The text block's size in pixels: what crengine lays out into.
    width: u32,
    height: u32,
    /// Where the text block starts on the screen.
    top: i32,
    page_words_calls: AtomicUsize,
}

impl CrengineDoc {
    /// Open `path` and lay it out for `style`.
    ///
    /// The layout is the text block's size in pixels: the same area Plato's
    /// engine is given, so both engines put the text in the same place.
    pub fn open(path: &Path, style: &Style) -> Result<CrengineDoc, Error> {
        Self::open_with_progress(path, style, None)
    }

    /// Open `path` and lay it out for `style`, reporting formatting progress (0..=100)
    /// to `progress` if provided.
    pub fn open_with_progress(
        path: &Path,
        style: &Style,
        progress: Option<&Arc<AtomicU8>>,
    ) -> Result<CrengineDoc, Error> {
        let engine = engine()?;
        let (face, fallback_faces) = faces_for(&engine, style);
        let (top, body_height) = style.body();
        let size = (style.width, body_height);
        let layout = layout_for(style, &face, &fallback_faces);
        let mut cb = progress.map(|p| {
            let p = p.clone();
            move |val: u32| {
                p.store(val.min(100) as u8, Ordering::Relaxed);
            }
        });
        let doc = engine
            .open_with_progress(
                path,
                size,
                &layout,
                cb.as_mut().map(|f| f as &mut dyn FnMut(u32)),
            )
            .map_err(|e| format_err!("{}: {e}", path.display()))?;
        Ok(CrengineDoc {
            doc,
            layout,
            width: size.0,
            height: size.1,
            top,
            page_words_calls: AtomicUsize::new(0),
        })
    }

    /// Lay the document out again with `style`.
    pub fn set_layout(&mut self, style: &Style) -> Result<(), Error> {
        self.set_layout_with_progress(style, None)
    }

    /// Lay the document out again with `style`, reporting formatting progress
    /// to `progress` if provided.
    pub fn set_layout_with_progress(
        &mut self,
        style: &Style,
        progress: Option<&Arc<AtomicU8>>,
    ) -> Result<(), Error> {
        let engine = engine()?;
        let (face, fallback_faces) = faces_for(&engine, style);
        let (top, body_height) = style.body();
        let size = (style.width, body_height);
        let layout = layout_for(style, &face, &fallback_faces);
        let mut cb = progress.map(|p| {
            let p = p.clone();
            move |val: u32| {
                p.store(val.min(100) as u8, Ordering::Relaxed);
            }
        });
        self.doc
            .set_layout_with_progress(&layout, cb.as_mut().map(|f| f as &mut dyn FnMut(u32)))
            .map_err(|e| format_err!("set_layout: {e}"))?;
        self.layout = layout;
        self.width = size.0;
        self.height = size.1;
        self.top = top;
        Ok(())
    }

    /// The layout the book was laid out with.
    pub fn layout(&self) -> &LayoutSettings {
        &self.layout
    }

    /// The book's page count.
    pub fn page_count(&self) -> usize {
        self.doc.page_count() as usize
    }

    /// Render the page at `index` (0-based) into a grayscale buffer.
    pub fn render_page(&mut self, index: usize) -> Result<GrayBuffer, Error> {
        self.render_page_with_options(index, &inkread_engine::RenderOptions::default())
    }

    /// Render the page at `index` (0-based) into a grayscale buffer with options.
    pub fn render_page_with_options(
        &mut self,
        index: usize,
        options: &inkread_engine::RenderOptions,
    ) -> Result<GrayBuffer, Error> {
        let mut buffer = GrayBuffer::new(self.width, self.height);
        self.doc
            .render_page_with_options(index as u32, &mut buffer, options)
            .map_err(|e| format_err!("can't render page {}: {e}", index + 1))?;
        Ok(buffer)
    }

    /// Where the page at `index` starts, as an XPointer: a position that
    /// survives a re-layout of the same book better than a page number.
    pub fn page_xpointer(&self, index: usize) -> Option<String> {
        self.doc.page_xpointer(index as u32).ok()
    }

    /// The page an XPointer points at.
    pub fn page_of_xpointer(&self, xpointer: &str) -> Option<usize> {
        self.doc.page_of_xpointer(xpointer).ok().map(|p| p as usize)
    }

    /// The book's table of contents, flattened, with the page each entry
    /// starts on.
    pub fn toc(&self) -> Vec<TocEntry> {
        self.doc.toc().unwrap_or_default()
    }

    /// The words on the page at `index`, with the XPointers that start and
    /// end them.
    pub fn page_words(&self, index: usize) -> Vec<inkread_engine::Word> {
        self.page_words_calls.fetch_add(1, Ordering::Relaxed);
        self.doc.page_words(index as u32).unwrap_or_default()
    }

    /// Number of times `page_words` has been called on this document.
    pub fn page_words_calls(&self) -> usize {
        self.page_words_calls.load(Ordering::Relaxed)
    }

    /// The links on the page at `index`.
    pub fn page_links(&self, index: usize) -> Vec<inkread_engine::Link> {
        self.doc.page_links(index as u32).unwrap_or_default()
    }

    /// Where the text block starts on the screen.
    pub fn top(&self) -> i32 {
        self.top
    }

    /// Whether the document supports partial rerendering (multi-fragment + cache enabled).
    pub fn can_partial_rerender(&self) -> bool {
        self.doc.can_partial_rerender()
    }

    /// Turn partial rerender mode on or off.
    pub fn enable_partial_rerender(&mut self, enable: bool) -> Result<(), Error> {
        self.doc
            .enable_partial_rerender(enable)
            .map_err(|e| format_err!("{e}"))
    }

    /// Whether the document is currently in delayed relayout state.
    pub fn is_rerender_delayed(&self) -> bool {
        self.doc.is_rerender_delayed()
    }

    /// Complete formatting the rest of the book in the background.
    pub fn catchup_render(&mut self) -> Result<(), Error> {
        self.catchup_render_with_progress(None)
    }

    /// Complete formatting the rest of the book in the background, reporting progress.
    pub fn catchup_render_with_progress(
        &mut self,
        progress: Option<&Arc<AtomicU8>>,
    ) -> Result<(), Error> {
        let mut cb = progress.map(|p| {
            let p = p.clone();
            move |val: u32| {
                p.store(val.min(100) as u8, Ordering::Relaxed);
            }
        });
        self.doc
            .catchup_render_with_progress(cb.as_mut().map(|f| f as &mut dyn FnMut(u32)))
            .map_err(|e| format_err!("{e}"))
    }
}

fn resolve_font_file(file: &str) -> Option<PathBuf> {
    let p = PathBuf::from(file);
    if p.is_file() {
        return Some(p);
    }
    let data_p = Path::new("data").join(file);
    if data_p.is_file() {
        return Some(data_p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cache_p = PathBuf::from(home)
            .join(".cache/aristotle/render/device/data")
            .join(file);
        if cache_p.is_file() {
            return Some(cache_p);
        }
    }
    None
}

/// Register font files with crengine.
pub fn register_fonts<P: AsRef<Path>>(paths: &[P]) -> Result<(), Error> {
    inkread_engine::crengine::register_fonts(paths)
        .map_err(|e| format_err!("crengine register fonts: {e}"))
}

/// Register all TTF and OTF fonts found directly in `dir` with crengine.
pub fn register_font_dir(dir: &Path) -> Result<(), Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("ttf") || ext.eq_ignore_ascii_case("otf") {
                    files.push(path);
                }
            }
        }
    }
    if !files.is_empty() {
        register_fonts(&files)?;
    }
    Ok(())
}

/// The engine, with the package's fonts registered.
///
/// The fonts live in the data directory, next to the ones Plato uses: the
/// serif the rest of Aristotle sets the body in, and the Chinese face the UI
/// package ships. Without them the engine falls back to its own candidates,
/// which on the host are the system's CJK fonts.
fn engine() -> Result<CrengineEngine, Error> {
    let fonts: Vec<PathBuf> = FONT_FILES
        .iter()
        .filter_map(|path| resolve_font_file(path))
        .collect();
    if fonts.is_empty() {
        return CrengineEngine::with_default_fonts().map_err(|e| format_err!("crengine: {e}"));
    }
    CrengineEngine::new(fonts).map_err(|e| format_err!("crengine: {e}"))
}

/// The registered face names, for tests that pick a face by what the font
/// file calls itself rather than by a hard-coded name.
#[cfg(test)]
pub(crate) fn registered_faces() -> Vec<String> {
    engine().map(|e| e.font_faces()).unwrap_or_default()
}

/// The faces the body text is set in: the main one, and the ones that take
/// over the glyphs it does not have.
///
/// The body is set in the Latin face, and the Chinese face takes over the Chinese
/// characters. One font for the whole book would be a compromise: a Chinese
/// book set in the Chinese face has its Latin words in a sans-serif face,
/// and Literata would never be used at all.
fn faces_for(engine: &CrengineEngine, style: &Style) -> (String, Vec<String>) {
    let faces = engine.font_faces();
    let main = if !style.font_family.is_empty() {
        faces
            .iter()
            .find(|face| face.eq_ignore_ascii_case(&style.font_family))
            .cloned()
            .or_else(|| {
                eprintln!(
                    "aristotle-render: font family {:?} not found among {:?}, falling back to {:?}",
                    style.font_family, faces, LATIN_FACE
                );
                faces
                    .iter()
                    .find(|face| face.eq_ignore_ascii_case(LATIN_FACE))
                    .cloned()
            })
    } else {
        faces
            .iter()
            .find(|face| face.eq_ignore_ascii_case(LATIN_FACE))
            .cloned()
    };

    let fallback = if !style.cjk_font_family.is_empty() {
        faces
            .iter()
            .find(|face| face.eq_ignore_ascii_case(&style.cjk_font_family))
            .cloned()
            .or_else(|| {
                eprintln!(
                    "aristotle-render: CJK font family {:?} not found among {:?}, falling back to {:?}",
                    style.cjk_font_family, faces, CJK_FACE
                );
                faces
                    .iter()
                    .find(|face| face.eq_ignore_ascii_case(CJK_FACE))
                    .cloned()
            })
    } else {
        faces
            .iter()
            .find(|face| face.eq_ignore_ascii_case(CJK_FACE))
            .cloned()
    };

    // Without the package's fonts the engine picks its own: let it, and keep
    // the Chinese face as the fallback when there is one.
    (main.unwrap_or_default(), fallback.into_iter().collect())
}

/// Generate CSS rules for CJK first-line indent and paragraph gap.
///
/// The declarations are `!important`: a book's own `text-indent` rules load
/// after the base sheet and would otherwise win (READER-001 C.2). Leading
/// full-width spaces are content, not style; crengine drops them at load
/// (see `strip_leading_ideographic_spaces` in `engine/shim.cpp`), so this
/// sheet alone controls the first line.
pub fn build_cjk_css(indent_chars: usize, gap_em: f32) -> String {
    let gap_str = if gap_em <= 0.001 {
        "0em".to_string()
    } else {
        format!("{gap_em:.1}em")
    };
    format!(
        "body {{\n    text-indent: {indent_chars}em !important;\n}}\np {{\n    text-indent: {indent_chars}em !important;\n    margin-top: 0em !important;\n    margin-bottom: {gap_str} !important;\n}}"
    )
}

/// crengine's layout parameters, from Aristotle's style.
///
/// The margins are the same on all four sides, as in Plato: the layout area
/// is the text block plus its margins, and the chrome (header and footer)
/// lives outside it.
fn layout_for(style: &Style, face: &str, fallback_faces: &[String]) -> LayoutSettings {
    // Plato takes the font size in points and the dpi; crengine wants
    // device pixels.
    let font_size_px = ((style.font_size * style.dpi as f32) / 72.0)
        .round()
        .clamp(MIN_FONT_SIZE_PX as f32, MAX_FONT_SIZE_PX as f32) as u32;
    let line_spacing_percent = ((style.line_height * 100.0).round() as u32)
        .clamp(MIN_LINE_SPACING_PERCENT, MAX_LINE_SPACING_PERCENT);
    let margin = style.margin_px().clamp(0, MAX_MARGIN_PX as i32) as u32;
    let user_css = Some(build_cjk_css(style.cjk_indent, style.cjk_gap));
    LayoutSettings {
        font_size_px,
        line_spacing_percent,
        margins: Margins {
            left: margin,
            right: margin,
            top: margin,
            bottom: margin,
        },
        font_face: face.to_string(),
        // The glyphs the main face does not have: the Chinese ones.
        fallback_font_faces: fallback_faces.to_vec(),
        // The book's own stylesheets stay in charge, as with Plato.
        embedded_styles: true,
        user_css,
    }
}

/// The directory a page belongs to.
///
/// The book's table of contents is a tree; the engine hands it over
/// flattened, with the level of every entry. An entry owns the pages from
/// its own page to the page of the next entry of the same or a shallower
/// level; a page inside several entries' ranges belongs to the deepest one.
/// The pages before the first entry belong to no entry.
pub fn chapter_of_page(entries: &[TocEntry], page: u32) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (index, entry) in entries.iter().enumerate() {
        if entry.page > page {
            continue;
        }
        let end = entries[index + 1..]
            .iter()
            .find(|next| next.level <= entry.level)
            .map(|next| next.page)
            .unwrap_or(u32::MAX);
        if page >= end {
            continue;
        }
        match best {
            Some(current) if entries[current].level >= entry.level => {}
            _ => best = Some(index),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of contents entry.
    fn entry(title: &str, level: u32, page: u32) -> TocEntry {
        TocEntry {
            title: title.to_string(),
            level,
            xpointer: format!("/body/DocFragment[{page}]"),
            page,
        }
    }

    #[test]
    fn the_font_files_are_the_packages_own() {
        // The serif is what the rest of Aristotle sets the body in, the CJK
        // face is what the UI package ships.
        assert!(FONT_FILES.contains(&"serif/Literata-Regular.ttf"));
        assert!(FONT_FILES.contains(&"fonts/DejaVuSans-Symbols.ttf"));
    }

    #[test]
    fn the_layout_follows_aristotles_style() {
        let style = Style::pw4();
        let layout = layout_for(&style, "Literata", &["Noto Sans SC".to_string()]);
        // 10 pt at 300 dpi is 42 px.
        assert_eq!(layout.font_size_px, 42);
        // 1.3 em is 130 %, which the engine does not go below 140 % on.
        assert_eq!(layout.line_spacing_percent, MIN_LINE_SPACING_PERCENT);
        // 5 mm at 300 dpi is 59 px, on all four sides, as in Plato.
        let margin = style.margin_px() as u32;
        assert_eq!(margin, 59);
        assert_eq!(layout.margins.left, margin);
        assert_eq!(layout.margins.right, margin);
        assert_eq!(layout.margins.top, margin);
        assert_eq!(layout.margins.bottom, margin);
        assert_eq!(layout.font_face, "Literata");
        assert!(layout.embedded_styles);
        assert_eq!(
            layout.user_css.as_deref(),
            Some("body {\n    text-indent: 2em !important;\n}\np {\n    text-indent: 2em !important;\n    margin-top: 0em !important;\n    margin-bottom: 0.6em !important;\n}")
        );
    }

    #[test]
    fn cjk_css_mapping_supports_indent_and_gap_variations() {
        assert_eq!(
            build_cjk_css(2, 0.6),
            "body {\n    text-indent: 2em !important;\n}\np {\n    text-indent: 2em !important;\n    margin-top: 0em !important;\n    margin-bottom: 0.6em !important;\n}"
        );
        assert_eq!(
            build_cjk_css(0, 0.0),
            "body {\n    text-indent: 0em !important;\n}\np {\n    text-indent: 0em !important;\n    margin-top: 0em !important;\n    margin-bottom: 0em !important;\n}"
        );
        assert_eq!(
            build_cjk_css(2, 0.3),
            "body {\n    text-indent: 2em !important;\n}\np {\n    text-indent: 2em !important;\n    margin-top: 0em !important;\n    margin-bottom: 0.3em !important;\n}"
        );

        let mut style = Style::pw4();
        style.cjk_indent = 0;
        style.cjk_gap = 0.0;
        let layout = layout_for(&style, LATIN_FACE, &[CJK_FACE.to_string()]);
        assert_eq!(
            layout.user_css.as_deref(),
            Some("body {\n    text-indent: 0em !important;\n}\np {\n    text-indent: 0em !important;\n    margin-top: 0em !important;\n    margin-bottom: 0em !important;\n}")
        );
    }

    #[test]
    fn the_chinese_face_is_the_fallback_for_the_body_face() {
        let style = Style::pw4();
        let layout = layout_for(&style, LATIN_FACE, &[CJK_FACE.to_string()]);
        // The body is set in Literata; the Chinese characters go to the
        // Chinese face.
        assert_eq!(layout.font_face, LATIN_FACE);
        assert_eq!(layout.fallback_font_faces, vec![CJK_FACE.to_string()]);
    }

    #[test]
    fn faces_for_resolves_style_fonts_or_falls_back() {
        if let Ok(eng) = engine() {
            let mut style = Style::pw4();
            style.font_family = "NonExistentLatin".into();
            style.cjk_font_family = "NonExistentCJK".into();
            let (main, fallback) = faces_for(&eng, &style);
            // Must not panic, must fall back to Literata and Noto Sans SC (if available)
            assert_eq!(main, LATIN_FACE);
            assert_eq!(fallback, vec![CJK_FACE.to_string()]);
        }
    }

    #[test]
    fn a_flat_toc_covers_the_pages_up_to_the_next_entry() {
        let toc = vec![
            entry("第一章", 0, 0),
            entry("第二章", 0, 3),
            entry("第三章", 0, 6),
        ];
        assert_eq!(chapter_of_page(&toc, 0), Some(0));
        assert_eq!(chapter_of_page(&toc, 2), Some(0));
        assert_eq!(chapter_of_page(&toc, 3), Some(1));
        assert_eq!(chapter_of_page(&toc, 6), Some(2));
        assert_eq!(chapter_of_page(&toc, 99), Some(2));
    }

    #[test]
    fn a_nested_toc_picks_the_deepest_entry() {
        let toc = vec![
            entry("第一章", 0, 0),
            entry("1.1", 1, 1),
            entry("1.2", 1, 4),
            entry("第二章", 0, 8),
        ];
        assert_eq!(chapter_of_page(&toc, 0), Some(0));
        assert_eq!(chapter_of_page(&toc, 3), Some(1));
        assert_eq!(chapter_of_page(&toc, 4), Some(2));
        assert_eq!(chapter_of_page(&toc, 8), Some(3));
    }

    #[test]
    fn the_pages_before_the_first_entry_have_no_chapter() {
        let toc = vec![entry("第一章", 0, 3), entry("第二章", 0, 7)];
        assert_eq!(chapter_of_page(&toc, 0), None);
        assert_eq!(chapter_of_page(&toc, 2), None);
        assert_eq!(chapter_of_page(&toc, 3), Some(0));
    }
}
