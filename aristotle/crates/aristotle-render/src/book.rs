// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opening a book with Plato's document engine and turning it into pages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

use plato_core::anyhow::{format_err, Error};
use plato_core::document::epub::EpubDocument;
use plato_core::document::html::layout::{hyph_lang, DEFAULT_HYPH_LANG, HYPHENATION_PATTERNS};
use plato_core::document::pdf::PdfOpener;
use plato_core::document::{self, file_kind, Document, Location, TocEntry};
use plato_core::framebuffer::Pixmap;
use plato_core::metadata::TextAlign;

use plato_core::geom::CycleDir;

#[cfg(feature = "crengine")]
use crate::crengine::ChapterAnchor;
use crate::lang;
use crate::mobi::MobiInfo;
use crate::notes;
use crate::page::{Chrome, Folio, Page};
use crate::position::{PosMap, Spine};
use crate::richtext::RichText;
use crate::style::{Style, Underline, UnderlineScope, UNDERLINE_GRAY};

/// A reading position in units that outlive one layout (see
/// [`crate::position`]): what the library index stores.
#[derive(Clone, Debug, PartialEq)]
pub enum Position {
    /// EPUB spine item and byte offset inside it.
    Spine { item: usize, offset: usize },
    /// Stock Kindle position: byte offset into the MOBI text.
    Kindle(u64),
    /// Zero-based page of a fixed-layout document.
    Page(usize),
    /// Share of the book read, 0 to 1: the fallback when nothing exact is
    /// known (KOReader XPointers, a format the renderer reads differently).
    Fraction(f32),
    /// A path into the book's document, as crengine-ng's XPointers spell it.
    ///
    /// The position does not depend on the layout, so it survives a change
    /// of font size or margin: the same words are on the page, wherever the
    /// new layout puts them.
    XPointer { xpointer: String },
}

/// Which engine lays the book out. It decides what a "location" is and how
/// the page is placed on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// Plato's own HTML engine (EPUB, HTML): our font, Knuth–Plass
    /// justification and Liang hyphenation. Locations are byte offsets into
    /// the book's text and pages are discovered by laying the book out.
    Plato,
    /// MuPDF's HTML layout (MOBI, FB2, TXT through Plato's PdfOpener).
    /// Locations are page indices; MuPDF paginates the whole book on layout
    /// and uses its own fonts and line breaking.
    MupdfReflow,
    /// Fixed-layout pages (PDF, CBZ, XPS, DjVu), scaled to fit.
    Fixed,
    /// crengine-ng (EPUB), through the readest engine crate. Locations are
    /// page indices; a place in the book is named by an XPointer, which
    /// survives a re-layout.
    ///
    /// Only books laid out by the `crengine` feature use this engine; the
    /// variant is always there, so callers can tell the engines apart
    /// whatever the crate was built with.
    Crengine,
    /// The pure-Rust vertical engine (see [`crate::vertical`]): classical
    /// Chinese books set top to bottom, right to left. Locations are
    /// character offsets into the flattened book, and pages are found by
    /// laying one page out at a time.
    Vertical,
}

/// A highlight mark resolved for crengine: page span and boundary XPointers.
#[derive(Clone, Debug, PartialEq)]
pub struct CrengineMark {
    pub id: Option<usize>,
    pub start_page: usize,
    pub end_page: usize,
    pub start_xpointer: Option<String>,
    pub end_xpointer: Option<String>,
}

/// A mark resolved for layout and rendering.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    /// Plato highlight: location byte offset span.
    Plato(usize, usize),
    /// Crengine highlight: page span with boundary XPointers.
    Crengine(CrengineMark),
}

/// An open book, laid out for one [`Style`].
pub struct Book {
    doc: Box<dyn Document>,
    engine: Engine,
    style: Style,
    toc: Vec<TocEntry>,
    /// The table of contents of a book laid out by crengine-ng, flattened
    /// with the page every entry starts on.
    #[cfg(feature = "crengine")]
    crengine_toc: Vec<inkread_engine::TocEntry>,
    /// The crengine-ng document, when the book is laid out by it.
    #[cfg(feature = "crengine")]
    crengine: Option<crate::crengine::CrengineDoc>,
    title: Option<String>,
    /// The book's author, from the package metadata, when it has one.
    author: Option<String>,
    language: Option<String>,
    /// Locations of the pages found so far, in order. For the Plato engine
    /// they are found by walking `Location::Next`, which lays out one spine
    /// item (chapter file) at a time.
    pages: Vec<usize>,
    /// Whether `pages` covers the whole book.
    complete: bool,
    mobi: Option<MobiConversion>,
    /// Spine item sizes, to convert Plato locations to spine positions.
    spine: Option<Spine>,
    /// The file Plato reads (the converted EPUB for a MOBI book).
    source: PathBuf,
    /// The book's source, opened on the first footnote.
    notes: Option<notes::Source>,
    /// Locations of table of contents entries found without layout, by
    /// entry index (see [`Book::toc_location_cheap`]).
    toc_locations: HashMap<usize, Option<usize>>,
    /// Whether night mode image inversion is active.
    night: bool,
    /// The guide drawn under every line of body text.
    underline: Underline,
    /// How wide the guide is: under the text only, or across the text block.
    underline_scope: UnderlineScope,
    /// The book as the vertical engine reads it, when `style.writing_mode`
    /// asks for the vertical layout. The engine has no Plato document, so
    /// [`Book::doc`] holds an empty placeholder then.
    vertical: Option<crate::vertical::Document>,
}

/// A rectangle on the screen, in pixels: `x0..x1`, `y0..y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Bounds {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// Squared distance from a point to the rectangle (0 inside).
    pub fn dist2(&self, x: i32, y: i32) -> i64 {
        let dx = (self.x0 - x).max(0).max(x - self.x1 + 1) as i64;
        let dy = (self.y0 - y).max(0).max(y - self.y1 + 1) as i64;
        dx * dx + dy * dy
    }

    /// The overlap of two rectangles, if they overlap.
    pub fn intersect(&self, o: &Bounds) -> Option<Bounds> {
        let b = Bounds {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        (b.x0 < b.x1 && b.y0 < b.y1).then_some(b)
    }

    pub fn union(&self, o: &Bounds) -> Bounds {
        Bounds {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
}

/// A word (or a hyphenated piece of one) on a rendered page.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// As laid out; a piece before a hyphenation break ends in U+00AD.
    pub text: String,
    pub rect: Bounds,
    /// Plato location of the word's first byte (EPUB); for fixed-layout
    /// documents the page index; for crengine-ng `(page << 16) | intra`.
    pub offset: usize,
    /// Starting XPointer into the book's DOM, for engines that provide it.
    pub start_xpointer: Option<String>,
    /// Ending XPointer into the book's DOM, for engines that provide it.
    pub end_xpointer: Option<String>,
}

/// A link on a rendered page.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The `href` as written in the book (percent-decoded).
    pub uri: String,
    pub rect: Bounds,
    pub offset: usize,
}

/// A flattened table of contents line.
#[derive(Clone, Debug, PartialEq)]
pub struct TocItem {
    pub title: String,
    pub depth: usize,
    /// The entry's index, for [`Book::toc_page`].
    pub index: usize,
    /// The page the entry starts on, when the engine knows it without
    /// laying the book out (crengine-ng does; Plato's engine does not).
    pub page: Option<usize>,
}

/// Where the current chapter is, for the progress line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChapterInfo {
    /// 1-based number among the chapters at the chosen level.
    pub number: usize,
    pub count: usize,
    pub start: usize,
    /// Where the next chapter starts (or the end of the book).
    pub end: usize,
}

fn flatten(entries: &[TocEntry], depth: usize, out: &mut Vec<(usize, TocEntry)>) {
    for e in entries {
        out.push((depth, e.clone()));
        flatten(&e.children, depth + 1, out);
    }
}

/// Collect the table of contents anchors that crengine must turn into page
/// breaks.
///
/// An entry names a spine file and an optional fragment. Entries without a
/// fragment are already covered by the body rule that starts every spine
/// item. The fragment is matched to its spine item through the OPF spine, so
/// the CSS names the same `_doc_fragment_N_ id` crengine builds in its DOM.
#[cfg(feature = "crengine")]
fn collect_chapter_anchors(entries: &[TocEntry], spine: &Spine, out: &mut Vec<ChapterAnchor>) {
    for entry in entries {
        if let Location::Uri(uri) = &entry.location {
            if let Some((path, id)) = uri.split_once('#') {
                if !id.is_empty() {
                    if let Some(item) = spine.item_of(path) {
                        let anchor = ChapterAnchor {
                            spine: item,
                            id: id.to_string(),
                        };
                        if !out.contains(&anchor) {
                            out.push(anchor);
                        }
                    }
                }
            }
        }
        collect_chapter_anchors(&entry.children, spine, out);
    }
}

/// A MOBI book opened through its converted EPUB (see [`crate::mobi`]).
#[derive(Clone, Debug)]
pub struct MobiConversion {
    pub info: MobiInfo,
    /// The cached EPUB this book is read from.
    pub epub: PathBuf,
    /// False when the EPUB came from the cache.
    pub converted_now: bool,
    /// MOBI positions to EPUB locations; `None` if the map is missing.
    pub posmap: Option<PosMap>,
}

/// How the book will be hyphenated.
#[derive(Clone, Debug)]
pub struct Hyphenation {
    /// The language tag the engine uses (the book's, or Plato's fallback).
    pub tag: String,
    /// The pattern set it maps to, if any (`Polish`, `EnglishUS`, ...).
    pub language: Option<String>,
    /// Whether that pattern set was found in `hyphenation-patterns/`.
    pub loaded: bool,
}

/// Which engine lays a book of this kind out.
///
/// EPUBs go to crengine-ng, when the crate is built with it. The
/// `ARISTOTLE_EPUB_ENGINE=plato` environment variable asks for Plato's
/// engine instead, so both can be compared on the device without a rebuild.
fn engine_for(kind: &str, reflowable: bool) -> Engine {
    if matches!(kind, "epub" | "html" | "htm") {
        #[cfg(feature = "crengine")]
        if kind == "epub" && epub_engine_is_crengine() {
            return Engine::Crengine;
        }
        Engine::Plato
    } else if reflowable {
        Engine::MupdfReflow
    } else {
        Engine::Fixed
    }
}

/// Whether EPUBs are laid out by crengine-ng.
///
/// `ARISTOTLE_EPUB_ENGINE=plato` switches back to Plato's engine.
#[cfg(feature = "crengine")]
pub fn epub_engine_is_crengine() -> bool {
    !std::env::var("ARISTOTLE_EPUB_ENGINE").is_ok_and(|engine| engine.eq_ignore_ascii_case("plato"))
}

/// Whether EPUBs are laid out by crengine-ng (false when compiled without crengine).
#[cfg(not(feature = "crengine"))]
pub fn epub_engine_is_crengine() -> bool {
    false
}

impl Book {
    /// Open `path` and set it up for `style`. Nothing is laid out yet; the
    /// first call to [`Book::locate`] does that.
    ///
    /// MOBI books are converted to EPUB in `cache_dir` (once per file) and
    /// read with Plato's engine; without a cache directory, or when the
    /// conversion fails, they go through MuPDF.
    ///
    /// Plato resolves its fonts, stylesheets and hyphenation patterns
    /// relative to the current directory, so call [`crate::use_data_dir`]
    /// first.
    pub fn open(path: &Path, style: &Style, cache_dir: Option<&Path>) -> Result<Book, Error> {
        Self::open_with_progress(path, style, cache_dir, None)
    }

    /// Open `path` and set it up for `style`, reporting progress to `progress`.
    pub fn open_with_progress(
        path: &Path,
        style: &Style,
        cache_dir: Option<&Path>,
        progress: Option<&Arc<AtomicU8>>,
    ) -> Result<Book, Error> {
        let detected = aristotle_library::format::sniff(path)
            .map_err(|e| format_err!("{}: {e}", path.display()))?;
        let mut kind = detected
            .map(|f| f.name().to_string())
            .unwrap_or_else(|| file_kind(path).unwrap_or_default());
        let mut source = path.to_path_buf();
        let mut mobi = None;
        let is_mobi = kind == "mobi";
        if let (true, Some(dir)) = (is_mobi, cache_dir) {
            match crate::mobi::cached_epub(path, dir) {
                Ok((epub, info, converted_now)) => {
                    source = epub.clone();
                    kind = "epub".into();
                    mobi = Some(MobiConversion {
                        info,
                        posmap: crate::mobi::load_posmap(&epub),
                        epub,
                        converted_now,
                    });
                }
                Err(e) => eprintln!("{}: {e:#}; reading it with MuPDF", path.display()),
            }
        }
        // The vertical engine reads the book itself: no Plato document, no
        // crengine layout, and no TXT→EPUB conversion. A book it cannot
        // parse, or one with no font to draw a vertical page with, falls
        // through to the horizontal engines below: a broken EPUB or a bare
        // data directory still opens.
        if style.writing_mode.is_vertical()
            && matches!(kind.as_str(), "epub" | "txt")
            && crate::vertical::has_font(style)
        {
            match Self::open_vertical(&source, style, mobi.clone()) {
                Ok(book) => return Ok(book),
                Err(e) => eprintln!(
                    "{}: vertical layout unavailable, reading horizontally: {e:#}",
                    source.display()
                ),
            }
        }
        let is_txt = kind == "txt";
        if is_txt {
            #[cfg(feature = "crengine")]
            if epub_engine_is_crengine() {
                let default_cache = std::env::temp_dir().join("aristotle-cache");
                let dir = cache_dir.unwrap_or(&default_cache);
                match crate::txt::cached_epub_with_progress(path, dir, progress.map(|p| p.as_ref()))
                {
                    Ok(epub) => {
                        source = epub;
                        kind = "epub".into();
                    }
                    Err(e) => eprintln!("{}: {e:#}; reading it directly", path.display()),
                }
            }
        }
        let path = source.as_path();

        let mut language = None;
        let mut doc: Box<dyn Document> = if kind == "epub" {
            // Opened directly rather than through document::open, to pick
            // the hyphenation language when the book's declaration is
            // ambiguous (see crate::lang).
            let mut epub = EpubDocument::new(path)?;
            language = match &style.language {
                Some(l) => Some(l.clone()),
                None => lang::epub_language(path).unwrap_or_else(|e| {
                    eprintln!("{}: can't guess the language: {e:#}", path.display());
                    None
                }),
            };
            if language.is_some() {
                epub.set_language(language.clone());
            }
            Box::new(epub)
        } else if detected.is_some() {
            let mut opener = PdfOpener::new().ok_or_else(|| format_err!("can't create MuPDF"))?;
            if matches!(kind.as_str(), "mobi" | "azw3" | "txt") {
                opener.load_user_stylesheet();
            }
            Box::new(opener.open(path).ok_or_else(|| {
                format_err!("can't open {} (detected kind {:?})", path.display(), kind)
            })?)
        } else {
            document::open(path)
                .ok_or_else(|| format_err!("can't open {} (kind {:?})", path.display(), kind))?
        };
        // The language the book declares. It picks the face the body text is
        // set in; the language the user asked for only steers hyphenation.
        let declared = doc.metadata("dc:language").filter(|l| !l.trim().is_empty());
        let language = language.or_else(|| declared.clone());
        let (_, body_height) = style.body();

        let engine = engine_for(&kind, doc.is_reflowable());

        match engine {
            Engine::Plato => {
                doc.layout(style.width, body_height, style.font_size, style.dpi);
                doc.set_margin_width(style.margin_mm);
                doc.set_font_family(&style.font_family, &style.font_dir);
                doc.set_text_align(TextAlign::Justify);
                doc.set_line_height(style.line_height);
            }
            Engine::MupdfReflow => {
                // MuPDF has no margin setting; lay out into the text block
                // and place the pixmap at the margin.
                let m = style.margin_px() as u32;
                doc.layout(
                    style.width - 2 * m,
                    body_height - 2 * m,
                    style.font_size,
                    style.dpi,
                );
            }
            Engine::Fixed => {}
            Engine::Crengine => {}
            // The vertical engine reads the book itself and is never reached
            // here: `open_with_progress` returns its book before this point.
            Engine::Vertical => {}
        }

        // crengine lays the whole book out as one document and prefixes the
        // ids of every spine item. Read the navigation anchors before the
        // open, so the chapter rules are part of the sheet it lays out with.
        #[cfg(feature = "crengine")]
        let chapter_spine = if engine == Engine::Crengine {
            Spine::read(path).ok()
        } else {
            None
        };
        #[cfg(feature = "crengine")]
        let chapter_anchors = match (&chapter_spine, engine) {
            (Some(spine), Engine::Crengine) => {
                let mut anchors = Vec::new();
                if let Some(toc) = doc.toc() {
                    collect_chapter_anchors(&toc, spine, &mut anchors);
                }
                anchors
            }
            _ => Vec::new(),
        };
        #[cfg(feature = "crengine")]
        let (crengine, crengine_toc) = if engine == Engine::Crengine {
            let doc = crate::crengine::CrengineDoc::open_with_progress_and_chapters(
                path,
                style,
                progress,
                &chapter_anchors,
            )?;
            let toc = doc.toc();
            (Some(doc), toc)
        } else {
            (None, Vec::new())
        };
        let toc = match engine {
            #[cfg(feature = "crengine")]
            Engine::Crengine => Vec::new(),
            _ => doc.toc().unwrap_or_default(),
        };
        let title = doc.title().filter(|t| !t.trim().is_empty());
        let author = doc.author().filter(|a| !a.trim().is_empty());
        // Only trusted when it agrees with Plato's own idea of the size.
        let spine = match engine {
            Engine::Plato => Spine::read(path)
                .ok()
                .filter(|s| s.total() == doc.pages_count()),
            #[cfg(feature = "crengine")]
            Engine::Crengine => chapter_spine,
            _ => None,
        };
        Ok(Book {
            spine,
            doc,
            engine,
            style: style.clone(),
            toc,
            #[cfg(feature = "crengine")]
            crengine,
            #[cfg(feature = "crengine")]
            crengine_toc,
            title,
            author,
            language,
            pages: Vec::new(),
            complete: false,
            mobi,
            source: path.to_path_buf(),
            notes: None,
            toc_locations: HashMap::new(),
            night: false,
            underline: Underline::Off,
            underline_scope: UnderlineScope::TextOnly,
            vertical: None,
        })
    }

    /// Open `path` with the vertical engine.
    fn open_vertical(
        path: &Path,
        style: &Style,
        mobi: Option<MobiConversion>,
    ) -> Result<Book, Error> {
        let document = crate::vertical::Document::open(path)?;
        Ok(Book {
            doc: Box::new(crate::vertical::EmptyDocument),
            engine: Engine::Vertical,
            style: style.clone(),
            toc: Vec::new(),
            #[cfg(feature = "crengine")]
            crengine_toc: Vec::new(),
            #[cfg(feature = "crengine")]
            crengine: None,
            title: document.title.clone().filter(|t| !t.trim().is_empty()),
            author: document.author.clone().filter(|a| !a.trim().is_empty()),
            language: document.language.clone(),
            pages: Vec::new(),
            complete: false,
            mobi,
            spine: None,
            source: path.to_path_buf(),
            notes: None,
            toc_locations: HashMap::new(),
            night: false,
            underline: Underline::Off,
            underline_scope: UnderlineScope::TextOnly,
            vertical: Some(document),
        })
    }

    /// The geometry the vertical engine lays this book out with.
    fn vertical_style(&self) -> crate::vertical::VerticalStyle {
        crate::vertical::VerticalStyle::from_style(&self.style)
    }

    /// The start of the page after the vertical page at `location`.
    fn vertical_next(&self, location: usize) -> Option<usize> {
        let doc = self.vertical.as_ref()?;
        doc.next_page(location, &self.vertical_style())
    }

    /// The page index of a vertical location, laying pages out until it is
    /// known.
    fn vertical_page_index(&mut self, location: usize) -> Option<usize> {
        loop {
            let len = self.pages.len();
            match self.locate(len) {
                Some(start) if start < location => {}
                _ => break,
            }
        }
        self.index_of(location)
    }

    /// Set when this is a MOBI book read through its converted EPUB.
    pub fn mobi(&self) -> Option<&MobiConversion> {
        self.mobi.as_ref()
    }

    pub fn engine(&self) -> Engine {
        self.engine
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn author(&self) -> Option<String> {
        self.author.clone()
    }

    /// The language the text is hyphenated in: chosen by [`crate::lang`]
    /// or `--lang` for ambiguous EPUBs, else the book's first `dc:language`.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    pub fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    /// The table of contents of a book laid out by crengine-ng, as the
    /// engine reports it: flattened, with the page of every entry.
    #[cfg(feature = "crengine")]
    pub fn crengine_toc(&self) -> &[inkread_engine::TocEntry] {
        &self.crengine_toc
    }

    /// The layout crengine-ng laid the book out with, for the diagnostics.
    #[cfg(feature = "crengine")]
    pub fn crengine_layout(&self) -> Option<&inkread_engine::LayoutSettings> {
        self.crengine.as_ref().map(|doc| doc.layout())
    }

    /// Whether this book supports partial rerendering (crengine, multi-fragment, cache on).
    pub fn can_partial_rerender(&self) -> bool {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self
                .crengine
                .as_ref()
                .is_some_and(|doc| doc.can_partial_rerender());
        }
        false
    }

    /// Enable or disable partial rerender mode.
    pub fn enable_partial_rerender(&mut self, enable: bool) -> Result<(), Error> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            if let Some(doc) = self.crengine.as_mut() {
                return doc.enable_partial_rerender(enable);
            }
        }
        let _ = enable;
        Ok(())
    }

    /// Whether the book is currently in a delayed relayout state.
    pub fn is_rerender_delayed(&self) -> bool {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self
                .crengine
                .as_ref()
                .is_some_and(|doc| doc.is_rerender_delayed());
        }
        false
    }

    /// Change layout parameters and rerender (or delay if partial rerender is active).
    pub fn set_layout(&mut self, style: &Style) -> Result<(), Error> {
        self.set_layout_with_progress(style, None)
    }

    /// Change layout parameters and rerender, reporting progress.
    pub fn set_layout_with_progress(
        &mut self,
        style: &Style,
        progress: Option<&Arc<AtomicU8>>,
    ) -> Result<(), Error> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            if let Some(doc) = self.crengine.as_mut() {
                doc.set_layout_with_progress(style, progress)?;
                self.style = style.clone();
                self.crengine_toc = doc.toc();
                return Ok(());
            }
        }
        if self.engine == Engine::Vertical {
            // The vertical document does not depend on the typography: only
            // the page chain does, so forget it and lay out again.
            self.style = style.clone();
            self.pages.clear();
            self.complete = false;
            return Ok(());
        }
        let _ = (style, progress);
        Err(format_err!("engine does not support set_layout"))
    }

    /// Catch up formatting for the remainder of the book.
    pub fn catchup_render(&mut self) -> Result<(), Error> {
        self.catchup_render_with_progress(None)
    }

    /// Catch up formatting for the remainder of the book, reporting progress.
    pub fn catchup_render_with_progress(
        &mut self,
        progress: Option<&Arc<AtomicU8>>,
    ) -> Result<(), Error> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            if let Some(doc) = self.crengine.as_mut() {
                doc.catchup_render_with_progress(progress)?;
                self.crengine_toc = doc.toc();
                return Ok(());
            }
        }
        let _ = progress;
        Ok(())
    }

    /// The page of `xpointer` in the current typography, laying out its
    /// fragment first when a partial relayout has not reached it yet.
    ///
    /// After a typography change with partial rerendering, crengine keeps
    /// the old page numbers until the fragment holding a page is drawn:
    /// asked right away, the page of a place is its page in the old size,
    /// which in the new size shows earlier text (H-013: a reader on
    /// p[14] landed on p[10], and the catch-up kept that place). Drawing
    /// the page it names lays out that fragment; asking again then gives
    /// the new page, which is drawn in turn until the answer stays put.
    pub fn settled_page_of_xpointer(&mut self, xpointer: &str) -> Option<usize> {
        let mut page = self.page_of_xpointer(xpointer)?;
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine && self.is_rerender_delayed() {
            for _ in 0..4 {
                self.crengine.as_mut()?.render_page(page).ok()?;
                let again = self.page_of_xpointer(xpointer)?;
                if again == page {
                    break;
                }
                page = again;
            }
        }
        Some(page)
    }

    /// [`Book::location_of`], with an XPointer placed in the current
    /// typography (see [`Book::settled_page_of_xpointer`]).
    pub fn settled_location_of(&mut self, position: &Position) -> Option<usize> {
        if let Position::XPointer { xpointer } = position {
            if self.engine == Engine::Crengine {
                let page = self.settled_page_of_xpointer(xpointer)?;
                return Some(page.min(self.len().saturating_sub(1)));
            }
        }
        self.location_of(position)
    }

    /// Return the page of the given XPointer in crengine layout.
    pub fn page_of_xpointer(&self, xpointer: &str) -> Option<usize> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine.as_ref()?.page_of_xpointer(xpointer);
        }
        let _ = xpointer;
        None
    }

    /// The hyphenation the Plato engine will apply. Plato picks patterns
    /// per element from `lang`/`xml:lang` and falls back to the book's
    /// `dc:language`, then to English.
    pub fn hyphenation(&self) -> Hyphenation {
        let tag = self
            .language
            .clone()
            .unwrap_or_else(|| DEFAULT_HYPH_LANG.to_string());
        let lang = hyph_lang(&tag);
        Hyphenation {
            language: lang.map(|l| format!("{l:?}")),
            loaded: lang.is_some_and(|l| HYPHENATION_PATTERNS.contains_key(&l)),
            tag,
        }
    }

    /// Location of page `index` (0-based), laying out as much of the book
    /// as needed to find it. `None` past the end.
    pub fn locate(&mut self, index: usize) -> Option<usize> {
        if self.engine == Engine::Vertical {
            if self.pages.is_empty() {
                self.pages.push(0);
            }
            while self.pages.len() <= index {
                let last = *self.pages.last()?;
                match self.vertical_next(last) {
                    Some(next) => self.pages.push(next),
                    None => {
                        self.complete = true;
                        return None;
                    }
                }
            }
            return self.pages.get(index).copied();
        }
        if self.engine != Engine::Plato {
            return (index < self.len()).then_some(index);
        }
        if self.pages.is_empty() {
            let first = self.doc.resolve_location(Location::Exact(0))?;
            self.pages.push(first);
        }
        while self.pages.len() <= index && !self.complete {
            let last = *self.pages.last().expect("non-empty");
            match self.doc.resolve_location(Location::Next(last)) {
                // Guard against a location that does not advance, which
                // would loop forever.
                Some(next) if next > last => self.pages.push(next),
                _ => self.complete = true,
            }
        }
        self.pages.get(index).copied()
    }

    /// Lay out the whole book and return its page count. Cheap for MuPDF
    /// documents; for EPUB it lays out every spine item.
    pub fn paginate(&mut self) -> usize {
        if self.engine == Engine::Vertical {
            while !self.complete {
                let len = self.pages.len();
                if self.locate(len).is_none() {
                    self.complete = true;
                }
            }
            return self.pages.len();
        }
        if self.engine != Engine::Plato {
            return self.len();
        }
        while !self.complete {
            let len = self.pages.len();
            if self.locate(len).is_none() {
                break;
            }
        }
        self.pages.len()
    }

    /// Total pages if already known without further layout.
    pub fn known_page_count(&self) -> Option<usize> {
        match self.engine {
            Engine::Plato | Engine::Vertical => self.complete.then_some(self.pages.len()),
            _ => Some(self.len()),
        }
    }

    /// Position of page `index` in the book, 0.0 to 1.0.
    pub fn progress(&self, index: usize, location: usize) -> f32 {
        if self.engine == Engine::Vertical {
            return self.fraction(location);
        }
        match self.known_page_count() {
            // Agrees with the "x / y" next to it.
            Some(total) => (index + 1) as f32 / total.max(1) as f32,
            // Byte offset over total bytes: available before pagination,
            // though images and markup make it drift from the page count.
            None => location as f32 / self.doc.pages_count().max(1) as f32,
        }
    }

    /// Title of the chapter containing `location`.
    pub fn chapter_title(&mut self, location: usize) -> Option<String> {
        if self.engine == Engine::Vertical {
            return self
                .vertical
                .as_ref()
                .and_then(|doc| doc.chapter_title(location));
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            let index = self.crengine_chapter_index(location)?;
            let title = self.crengine_toc.get(index)?.title.trim().to_string();
            return (!title.is_empty()).then_some(title);
        }
        self.doc
            .chapter(location, &self.toc)
            .map(|(entry, _)| entry.title.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    /// Rasterise the book's content at `location`, without header and
    /// footer, as a grayscale pixmap.
    pub fn render_body(&mut self, location: usize) -> Option<Pixmap> {
        if self.engine == Engine::Vertical {
            return None;
        }
        let scale = match self.engine {
            Engine::Plato | Engine::MupdfReflow => 1.0,
            Engine::Fixed => {
                let (w, h) = self.doc.dims(location)?;
                let (_, body_h) = self.style.body();
                let m = self.style.margin_px() as f32;
                ((self.style.width as f32) / w).min((body_h as f32 - 2.0 * m) / h)
            }
            // crengine-ng renders into a gray buffer, not a pixmap.
            Engine::Crengine => return None,
            // The vertical engine renders its own page (see
            // `render_location_with_degraded`).
            Engine::Vertical => return None,
        };
        self.doc
            .pixmap(Location::Exact(location), scale, 1)
            .map(|(pixmap, _)| pixmap)
    }

    /// Put `body` (from [`Book::render_body`]) and the chrome on a page.
    pub fn compose(&self, body: &Pixmap, folio: &Folio, chrome: &mut Chrome) -> Page {
        let style = &self.style;
        let mut page = Page::white(style);
        let (top, body_h) = style.body();
        let (x, y) = match self.engine {
            Engine::Plato => (0, top),
            Engine::MupdfReflow => (style.margin_px(), top + style.margin_px()),
            Engine::Fixed => (
                (style.width as i32 - body.width as i32) / 2,
                top + (body_h as i32 - body.height as i32) / 2,
            ),
            // crengine-ng renders into a gray buffer, not a pixmap.
            Engine::Crengine => (0, top),
            // The vertical engine renders its own page; nothing to compose.
            Engine::Vertical => (0, top),
        };
        page.blit_pixmap(body, x, y);
        chrome.draw(&mut page, folio);
        page
    }

    /// The header and footer contents for page `index` at `location`.
    pub fn folio(&mut self, index: usize, location: usize) -> Folio {
        let chapter = self
            .chapter_title(location)
            .or_else(|| self.title.clone())
            .unwrap_or_default();
        Folio {
            header: self.header_line(),
            chapter,
            remaining: None,
            page: Some(index + 1),
            pages: self.known_page_count(),
            progress: self.progress(index, location),
            degraded: false,
        }
    }

    /// The running head: "author · title", or whichever is known.
    fn header_line(&self) -> String {
        match (self.author.as_deref(), self.title.as_deref()) {
            (Some(author), Some(title)) if !title.is_empty() => format!("{author} · {title}"),
            (Some(author), _) => author.to_string(),
            (_, Some(title)) => title.to_string(),
            _ => String::new(),
        }
    }

    // Navigation by location, for the reader app. A reader opens a book at
    // a saved position and turns from there, so it cannot depend on page
    // indices, which need the whole book laid out first (seconds on the
    // device). Indices come in later from a background pagination
    // ([`Book::set_pagination`]) and only feed "x / y".

    /// Size of the location space: bytes of text for Plato's engine, pages
    /// otherwise.
    pub fn len(&self) -> usize {
        if self.engine == Engine::Vertical {
            // An empty book still has one (blank) page, so the reader can
            // open it and show nothing rather than failing.
            return self.vertical.as_ref().map_or(0, |doc| doc.len().max(1));
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine.as_ref().map_or(0, |doc| doc.page_count());
        }
        self.doc.pages_count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Start of the page that contains `location` (clamped to the book),
    /// laying out its chapter if needed.
    pub fn page_start(&mut self, location: usize) -> Option<usize> {
        if self.engine == Engine::Vertical {
            let doc = self.vertical.as_ref()?;
            return Some(doc.page_start(location, &self.vertical_style()));
        }
        let last = self.len().checked_sub(1)?;
        let location = location.min(last);
        match self.engine {
            Engine::Plato => self
                .doc
                .resolve_location(Location::Exact(location))
                .or_else(|| self.doc.resolve_location(Location::Exact(0))),
            _ => Some(location),
        }
    }

    /// Start of the page after the one starting at `location`.
    pub fn next_page(&mut self, location: usize) -> Option<usize> {
        if self.engine == Engine::Vertical {
            // `location` need not be a page start: find the page it is on
            // first, then the one after it.
            let doc = self.vertical.as_ref()?;
            let style = self.vertical_style();
            let start = doc.page_start(location, &style);
            return doc.next_page(start, &style);
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            let next = location + 1;
            return (next < self.len()).then_some(next);
        }
        self.doc
            .resolve_location(Location::Next(location))
            .filter(|&n| n > location)
    }

    /// Start of the page before the one starting at `location`.
    pub fn previous_page(&mut self, location: usize) -> Option<usize> {
        if self.engine == Engine::Vertical {
            // The page before the one that starts at `location`: the page
            // holding the character just before it.
            let previous = location.checked_sub(1)?;
            let doc = self.vertical.as_ref()?;
            let start = doc.page_start(previous, &self.vertical_style());
            return (start < location).then_some(start);
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return location.checked_sub(1);
        }
        self.doc
            .resolve_location(Location::Previous(location))
            .filter(|&p| p < location)
    }

    /// Start of the next (or previous) chapter in the table of contents.
    pub fn chapter_page(&mut self, location: usize, forward: bool) -> Option<usize> {
        if self.engine == Engine::Vertical {
            let doc = self.vertical.as_ref()?;
            let chapter = doc.chapter_of(location)?;
            let target = if forward {
                doc.chapter_range(chapter).1
            } else {
                chapter.checked_sub(1)?
            };
            if forward && target >= doc.len() {
                return None;
            }
            let start = if forward {
                target
            } else {
                doc.chapter_range(target).0
            };
            return self.page_start(start);
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine_chapter_page(location, forward);
        }
        let dir = if forward {
            CycleDir::Next
        } else {
            CycleDir::Previous
        };
        let target = self
            .doc
            .chapter_relative(location, dir, &self.toc)
            .map(|entry| entry.location.clone())?;
        let found = self.doc.resolve_location(target)?;
        self.page_start(found)
    }

    /// Take page locations found elsewhere (a background pagination, or a
    /// cache on disk) as the whole book's pagination. They must come from
    /// the same file and style. Ignored for engines that know their pages.
    pub fn set_pagination(&mut self, pages: Vec<usize>) {
        if self.engine == Engine::Vertical {
            // The vertical pages are cheap to find: the background paginator
            // has nothing to add.
            return;
        }
        if self.engine == Engine::Plato && !pages.is_empty() {
            self.pages = pages;
            self.complete = true;
        }
    }

    /// The start of every page, once the whole book has been laid out.
    pub fn pagination(&self) -> Option<&[usize]> {
        (self.engine == Engine::Plato && self.complete).then_some(&self.pages[..])
    }

    /// Index of the page starting at or before `location`, if known
    /// without laying anything out.
    pub fn index_of(&self, location: usize) -> Option<usize> {
        if !matches!(self.engine, Engine::Plato | Engine::Vertical) {
            return Some(location);
        }
        let last = *self.pages.last()?;
        if !self.complete && location > last {
            return None;
        }
        Some(
            self.pages
                .partition_point(|&p| p <= location)
                .saturating_sub(1),
        )
    }

    /// Position in the book, 0 to 1: "x / y" when the pages are known,
    /// else the share of the text before `location`.
    ///
    /// A vertical book always uses the share of the characters, even when
    /// its pages are known: that is the measure [`Book::position_at`] saves,
    /// so the progress on screen and the saved position name the same place.
    pub fn fraction(&self, location: usize) -> f32 {
        if self.engine == Engine::Vertical {
            // The share of the characters before `location`, the same
            // measure [`Book::position_at`] saves for a vertical book. Page
            // numbers would drift when the typography changes: a page is a
            // different share of the book after a font size change.
            return self
                .vertical
                .as_ref()
                .map_or(0.0, |doc| doc.fraction(location));
        }
        match (self.known_page_count(), self.index_of(location)) {
            (Some(total), Some(index)) => (index + 1) as f32 / total.max(1) as f32,
            _ => location as f32 / self.len().max(1) as f32,
        }
    }

    /// The location of a saved position, if this book can place it exactly
    /// or by fraction. Not necessarily a page start (see
    /// [`Book::page_start`]).
    pub fn location_of(&self, position: &Position) -> Option<usize> {
        let len = self.len();
        let by_fraction =
            |f: f32| ((f.clamp(0.0, 1.0) * len as f32) as usize).min(len.saturating_sub(1));
        match (self.engine, position) {
            (_, Position::Fraction(f)) => Some(by_fraction(*f)),
            (Engine::Plato, Position::Spine { item, offset }) if self.mobi.is_none() => {
                self.spine.as_ref()?.location(*item, *offset)
            }
            (Engine::Plato, Position::Kindle(p)) => self
                .mobi
                .as_ref()
                .and_then(|m| m.posmap.as_ref())
                .map(|m| m.location(*p) as usize),
            (Engine::MupdfReflow | Engine::Fixed, Position::Page(i)) => {
                Some((*i).min(len.saturating_sub(1)))
            }
            #[cfg(feature = "crengine")]
            (Engine::Crengine, Position::Page(i)) => Some((*i).min(len.saturating_sub(1))),
            // The XPointer names a place in the book, not a page: the page
            // it is on depends on the layout, so it is asked of the engine.
            #[cfg(feature = "crengine")]
            (Engine::Crengine, Position::XPointer { xpointer }) => {
                let page = self.crengine.as_ref()?.page_of_xpointer(xpointer)?;
                Some(page.min(len.saturating_sub(1)))
            }
            // A position Plato's engine wrote (a spine offset) means nothing
            // to crengine-ng: the caller falls back to the fraction.
            _ => None,
        }
    }

    /// Bytes of its spine item before `location`: what Plato lays out to
    /// place it in a fresh layout (it lays out from the item's start, and
    /// only as far as needed). 0 for fixed-layout and MuPDF books.
    pub fn layout_bytes(&self, location: usize) -> usize {
        match (self.engine, &self.spine) {
            (Engine::Plato, Some(spine)) => spine.coordinates(location).1,
            (Engine::Plato, None) => location,
            _ => 0,
        }
    }

    /// The most exact position of `location` this book can name.
    pub fn position_at(&self, location: usize) -> Position {
        if self.engine == Engine::Vertical {
            // A share of the book: it survives the switch back to the
            // horizontal engine and any change of the typography.
            return Position::Fraction(
                self.vertical
                    .as_ref()
                    .map_or(0.0, |doc| doc.fraction(location)),
            );
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            // A MOBI book read through its converted EPUB has no position
            // map, so its progress is a share of the book.
            if self.mobi.is_some() {
                return Position::Fraction(location as f32 / self.len().max(1) as f32);
            }
            return match self
                .crengine
                .as_ref()
                .and_then(|doc| doc.page_xpointer(location))
            {
                Some(xpointer) => Position::XPointer { xpointer },
                None => Position::Fraction(location as f32 / self.len().max(1) as f32),
            };
        }
        match self.engine {
            Engine::Plato => {
                if let Some(map) = self.mobi.as_ref().and_then(|m| m.posmap.as_ref()) {
                    Position::Kindle(map.position_at_or_after(location as u64))
                } else if let (None, Some(spine)) = (&self.mobi, &self.spine) {
                    let (item, offset) = spine.coordinates(location);
                    Position::Spine { item, offset }
                } else {
                    Position::Fraction(location as f32 / self.len().max(1) as f32)
                }
            }
            _ => Position::Page(location),
        }
    }

    /// Render the page starting at `location`, with header and footer.
    pub fn render_location(&mut self, location: usize, chrome: &mut Chrome) -> Result<Page, Error> {
        self.render_location_with(location, chrome, None)
    }

    /// [`Book::render_location`] with `remaining` at the right of the footer's first row.
    pub fn render_location_with(
        &mut self,
        location: usize,
        chrome: &mut Chrome,
        remaining: Option<String>,
    ) -> Result<Page, Error> {
        self.render_location_with_degraded(location, chrome, remaining, false)
    }

    /// Render location with the optional time left in the chapter, and degraded mode flag.
    pub fn render_location_with_degraded(
        &mut self,
        location: usize,
        chrome: &mut Chrome,
        remaining: Option<String>,
        degraded: bool,
    ) -> Result<Page, Error> {
        if self.engine == Engine::Vertical {
            return self.render_vertical(location, chrome, remaining, degraded);
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.render_crengine(location, chrome, remaining, degraded);
        }
        let body = self
            .render_body(location)
            .ok_or_else(|| format_err!("can't render the page at {location}"))?;
        let chapter = self
            .chapter_title(location)
            .or_else(|| self.title.clone())
            .unwrap_or_default();
        let index = self.index_of(location);
        let folio = Folio {
            header: self.header_line(),
            chapter,
            remaining,
            page: index.map(|i| i + 1),
            pages: self.known_page_count(),
            progress: self.fraction(location),
            degraded,
        };
        let mut page = self.compose(&body, &folio, chrome);
        page.images = self.images(location);
        Ok(page)
    }

    /// Locate, render and compose page `index` (0-based).
    pub fn render_page(&mut self, index: usize, chrome: &mut Chrome) -> Result<Page, Error> {
        let location = self
            .locate(index)
            .ok_or_else(|| format_err!("page {} is past the end of the book", index + 1))?;
        if self.engine == Engine::Vertical {
            return self.render_vertical(location, chrome, None, false);
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.render_crengine(location, chrome, None, false);
        }
        let body = self
            .render_body(location)
            .ok_or_else(|| format_err!("can't render page {}", index + 1))?;
        let folio = self.folio(index, location);
        let mut page = self.compose(&body, &folio, chrome);
        page.images = self.images(location);
        Ok(page)
    }

    /// Set whether night mode image inversion is active.
    pub fn set_night(&mut self, night: bool) {
        self.night = night;
    }

    /// Whether night mode image inversion is active.
    pub fn night(&self) -> bool {
        self.night
    }

    /// Set the guide drawn under every line of body text.
    ///
    /// The guide is a render option, not a typographic one: changing it
    /// needs no new layout, only a new render of the page.
    pub fn set_underline(&mut self, underline: Underline) {
        self.underline = underline;
    }

    /// The guide drawn under every line of body text.
    pub fn underline(&self) -> Underline {
        self.underline
    }

    /// Set how wide the guide is: under the text only, or across the text
    /// block with the blank line slots filled in.
    ///
    /// Like the guide itself, this is a render option: no new layout.
    pub fn set_underline_scope(&mut self, scope: UnderlineScope) {
        self.underline_scope = scope;
    }

    /// How wide the guide is.
    pub fn underline_scope(&self) -> UnderlineScope {
        self.underline_scope
    }

    /// Render the page at `location` as the vertical engine lays it out.
    ///
    /// The engine draws the text block and its column rules; the chrome is
    /// drawn on top, exactly as it is over a crengine page.
    fn render_vertical(
        &mut self,
        location: usize,
        chrome: &mut Chrome,
        remaining: Option<String>,
        degraded: bool,
    ) -> Result<Page, Error> {
        let style = self.vertical_style();
        let layout = self
            .vertical
            .as_ref()
            .ok_or_else(|| format_err!("the book is not laid out vertically"))?
            .layout_page(location, &style);
        let mut page = crate::vertical::render_page(
            &self.style,
            &layout,
            self.underline,
            self.underline_scope,
        )?;
        let chapter = self
            .chapter_title(location)
            .or_else(|| self.title.clone())
            .unwrap_or_default();
        let index = self.index_of(location);
        let folio = Folio {
            header: self.header_line(),
            chapter,
            remaining,
            page: index.map(|i| i + 1),
            pages: self.known_page_count(),
            progress: self.fraction(location),
            degraded,
        };
        chrome.draw(&mut page, &folio);
        Ok(page)
    }

    /// Render the page at `location` as crengine-ng laid it out.
    ///
    /// crengine-ng hands out the page as a gray buffer the size of the text
    /// block; it is copied onto a white page where the block belongs, and
    /// the chrome is drawn on top of it.
    #[cfg(feature = "crengine")]
    fn render_crengine(
        &mut self,
        location: usize,
        chrome: &mut Chrome,
        remaining: Option<String>,
        degraded: bool,
    ) -> Result<Page, Error> {
        let (buffer, top) = {
            let doc = self
                .crengine
                .as_mut()
                .ok_or_else(|| format_err!("the book is not laid out by crengine"))?;
            let options = inkread_engine::RenderOptions {
                invert_images: self.night,
                underline: match self.underline {
                    Underline::Off => inkread_engine::Underline::Off,
                    Underline::Solid => inkread_engine::Underline::Solid,
                    Underline::Dotted => inkread_engine::Underline::Dotted,
                },
                underline_gray: UNDERLINE_GRAY,
                underline_scope: match self.underline_scope {
                    UnderlineScope::TextOnly => inkread_engine::UnderlineScope::TextOnly,
                    UnderlineScope::FullLine => inkread_engine::UnderlineScope::FullLine,
                },
            };
            (doc.render_page_with_options(location, &options)?, doc.top())
        };
        let chapter = self
            .chapter_title(location)
            .or_else(|| self.title.clone())
            .unwrap_or_default();
        let index = self.index_of(location);
        let folio = Folio {
            header: self.header_line(),
            chapter,
            remaining,
            page: index.map(|i| i + 1),
            pages: self.known_page_count(),
            progress: self.fraction(location),
            degraded,
        };
        let mut page = Page::white(&self.style);
        page.blit_gray_buffer(&buffer, 0, top);
        chrome.draw(&mut page, &folio);
        Ok(page)
    }
}

// Reading interactions: words and links on a page, footnotes, the table of
// contents. Coordinates are screen pixels of the composed page.
impl Book {
    /// How the body of the page at `location` is placed on the screen:
    /// (scale, x offset, y offset), as [`Book::render_body`] and
    /// [`Book::compose`] place it.
    fn placement(&mut self, location: usize) -> Option<(f32, i32, i32)> {
        let (top, body_h) = self.style.body();
        let m = self.style.margin_px();
        match self.engine {
            Engine::Plato => Some((1.0, 0, top)),
            Engine::MupdfReflow => Some((1.0, m, top + m)),
            Engine::Fixed => {
                let (w, h) = self.doc.dims(location)?;
                let scale =
                    ((self.style.width as f32) / w).min((body_h as f32 - 2.0 * m as f32) / h);
                let (pw, ph) = ((w * scale) as i32, (h * scale) as i32);
                Some((
                    scale,
                    (self.style.width as i32 - pw) / 2,
                    top + (body_h as i32 - ph) / 2,
                ))
            }
            // crengine-ng renders into a gray buffer, not a pixmap.
            Engine::Crengine => Some((1.0, 0, top)),
            Engine::Vertical => Some((1.0, 0, top)),
        }
    }

    fn to_screen(b: &plato_core::geom::Boundary, (scale, dx, dy): (f32, i32, i32)) -> Bounds {
        Bounds {
            x0: (b.min.x * scale).floor() as i32 + dx,
            y0: (b.min.y * scale).floor() as i32 + dy,
            x1: (b.max.x * scale).ceil() as i32 + dx,
            y1: (b.max.y * scale).ceil() as i32 + dy,
        }
    }

    /// The words of the page starting at `location`, in reading order.
    pub fn words(&mut self, location: usize) -> Vec<Word> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine_words(location);
        }
        if self.engine != Engine::Plato {
            return Vec::new();
        }
        let Some(place) = self.placement(location) else {
            return Vec::new();
        };
        let Some((words, _)) = self.doc.words(Location::Exact(location)) else {
            return Vec::new();
        };
        words
            .into_iter()
            .filter(|w| !w.text.trim().is_empty())
            .map(|w| Word {
                rect: Self::to_screen(&w.rect, place),
                offset: w.location.location(),
                text: w.text,
                start_xpointer: None,
                end_xpointer: None,
            })
            .collect()
    }

    /// The words on a crengine-laid-out page, converted to screen coordinates.
    #[cfg(feature = "crengine")]
    pub fn crengine_words(&self, location: usize) -> Vec<Word> {
        let Some(doc) = self.crengine.as_ref() else {
            return Vec::new();
        };
        let top = doc.top();
        let raw_words = doc.page_words(location);
        if raw_words.is_empty() {
            return Vec::new();
        }
        let base_offset = location << 16;
        let mut intra: usize = 0;
        let mut out: Vec<Word> = Vec::with_capacity(raw_words.len());
        for (i, w) in raw_words.into_iter().enumerate() {
            if i > 0 {
                let prev = &out[i - 1];
                let is_glued = prev.end_xpointer.as_deref() == Some(&w.start_xpointer);
                if is_glued {
                    intra += prev.text.len();
                } else {
                    intra += prev.text.len() + 1;
                }
            }
            let rect = if let Some(first_rect) = w.rects.first() {
                let mut b = Bounds {
                    x0: first_rect.x,
                    y0: first_rect.y + top,
                    x1: first_rect.x + first_rect.width,
                    y1: first_rect.y + first_rect.height + top,
                };
                for r in &w.rects[1..] {
                    b = b.union(&Bounds {
                        x0: r.x,
                        y0: r.y + top,
                        x1: r.x + r.width,
                        y1: r.y + r.height + top,
                    });
                }
                b
            } else {
                Bounds::default()
            };
            out.push(Word {
                text: w.text,
                rect,
                offset: base_offset | (intra & 0xFFFF),
                start_xpointer: Some(w.start_xpointer),
                end_xpointer: Some(w.end_xpointer),
            });
        }
        out
    }

    /// The mark offset for `position`, matching [`Word::offset`] on rendered pages.
    pub fn mark_location(&self, position: &Position) -> Option<usize> {
        self.mark_start_location(position)
    }

    /// The mark start offset for `position`, matching [`Word::offset`] on rendered pages.
    pub fn mark_start_location(&self, position: &Position) -> Option<usize> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            match position {
                Position::XPointer { xpointer } => {
                    let page = self.crengine.as_ref()?.page_of_xpointer(xpointer)?;
                    let words = self.crengine_words(page);
                    let idx = mark_start_word_index(&words, Some(xpointer));
                    return words.get(idx).map(|w| w.offset).or(Some(page << 16));
                }
                Position::Page(p) => return Some(p << 16),
                _ => {}
            }
        }
        self.location_of(position)
    }

    /// The mark end offset for `position`, matching [`Word::offset`] on rendered pages.
    pub fn mark_end_location(&self, position: &Position) -> Option<usize> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            match position {
                Position::XPointer { xpointer } => {
                    let page = self.crengine.as_ref()?.page_of_xpointer(xpointer)?;
                    let words = self.crengine_words(page);
                    let idx = mark_end_word_index(&words, Some(xpointer));
                    return words.get(idx).map(|w| w.offset).or(Some(page << 16));
                }
                Position::Page(p) => return Some(p << 16),
                _ => {}
            }
        }
        self.location_of(position)
    }

    /// Map a mark's start and end positions to page word offsets.
    pub fn mark_range(&self, start: &Position, end: &Position) -> Option<(usize, usize)> {
        Some((
            self.mark_start_location(start)?,
            self.mark_end_location(end)?,
        ))
    }

    /// Resolve a highlight mark into a [`Mark`] without loading page words.
    pub fn resolve_mark(
        &self,
        id: Option<usize>,
        start: &Position,
        end: &Position,
    ) -> Option<Mark> {
        match self.engine {
            Engine::Plato => {
                let s = self.location_of(start)?;
                let e = self.location_of(end)?;
                Some(Mark::Plato(s, e))
            }
            #[cfg(feature = "crengine")]
            Engine::Crengine => {
                let (start_page, start_xpointer) = match start {
                    Position::XPointer { xpointer } => {
                        let page = self.crengine.as_ref()?.page_of_xpointer(xpointer)?;
                        (page, Some(xpointer.clone()))
                    }
                    Position::Page(p) => (*p, None),
                    _ => (self.location_of(start)?, None),
                };
                let (end_page, end_xpointer) = match end {
                    Position::XPointer { xpointer } => {
                        let page = self.crengine.as_ref()?.page_of_xpointer(xpointer)?;
                        (page, Some(xpointer.clone()))
                    }
                    Position::Page(p) => (*p, None),
                    _ => (self.location_of(end)?, None),
                };
                let (start_page, end_page, start_xpointer, end_xpointer) = if start_page <= end_page
                {
                    (start_page, end_page, start_xpointer, end_xpointer)
                } else {
                    (end_page, start_page, end_xpointer, start_xpointer)
                };
                Some(Mark::Crengine(CrengineMark {
                    id,
                    start_page,
                    end_page,
                    start_xpointer,
                    end_xpointer,
                }))
            }
            _ => None,
        }
    }

    /// Number of times crengine's `page_words` has been called on this book.
    #[cfg(feature = "crengine")]
    pub fn page_words_calls(&self) -> usize {
        self.crengine.as_ref().map_or(0, |c| c.page_words_calls())
    }

    /// The images of the page starting at `location`, clipped to the text
    /// block. Only for Plato's engine: MuPDF draws whole pages, which night
    /// mode inverts like text.
    pub fn images(&mut self, location: usize) -> Vec<Bounds> {
        if self.engine != Engine::Plato {
            return Vec::new();
        }
        let Some(place) = self.placement(location) else {
            return Vec::new();
        };
        let Some((rects, _)) = self.doc.images(Location::Exact(location)) else {
            return Vec::new();
        };
        let (top, body_h) = self.style.body();
        let block = Bounds {
            x0: 0,
            y0: top,
            x1: self.style.width as i32,
            y1: top + body_h as i32,
        };
        rects
            .iter()
            .filter_map(|r| Self::to_screen(r, place).intersect(&block))
            .collect()
    }

    /// The links of the page starting at `location`.
    pub fn links(&mut self, location: usize) -> Vec<Link> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine_links(location);
        }
        if self.engine != Engine::Plato {
            return Vec::new();
        }
        let Some(place) = self.placement(location) else {
            return Vec::new();
        };
        let Some((links, _)) = self.doc.links(Location::Exact(location)) else {
            return Vec::new();
        };
        links
            .into_iter()
            .map(|l| Link {
                rect: Self::to_screen(&l.rect, place),
                offset: l.location.location(),
                uri: l.text,
            })
            .collect()
    }

    /// The links on a crengine-laid-out page, converted to screen coordinates.
    #[cfg(feature = "crengine")]
    fn crengine_links(&self, location: usize) -> Vec<Link> {
        let Some(doc) = self.crengine.as_ref() else {
            return Vec::new();
        };
        let top = doc.top();
        let raw_links = doc.page_links(location);
        let mut out = Vec::new();
        for l in raw_links {
            for r in l.rects {
                out.push(Link {
                    uri: l.href.clone(),
                    rect: Bounds {
                        x0: r.x,
                        y0: r.y + top,
                        x1: r.x + r.width,
                        y1: r.y + r.height + top,
                    },
                    offset: location,
                });
            }
        }
        out
    }

    /// The archive path of the spine item holding `location`.
    fn spine_path(&self, location: usize) -> Option<String> {
        let spine = self.spine.as_ref()?;
        let (item, _) = spine.coordinates(location);
        spine.paths.get(item).cloned()
    }

    #[cfg(feature = "crengine")]
    fn parse_doc_fragment_index(xpointer: &str) -> Option<usize> {
        let start = xpointer.find("/DocFragment[")? + "/DocFragment[".len();
        let end = xpointer[start..].find(']')? + start;
        let n: usize = xpointer[start..end].parse().ok()?;
        n.checked_sub(1)
    }

    #[cfg(feature = "crengine")]
    fn crengine_spine_path_of_page(&self, page: usize) -> Option<String> {
        let xp = self.crengine.as_ref()?.page_xpointer(page)?;
        let frag_idx = Self::parse_doc_fragment_index(&xp)?;
        self.spine.as_ref()?.paths.get(frag_idx).cloned()
    }

    #[cfg(feature = "crengine")]
    fn crengine_footnote(&mut self, link: &Link, link_text: &str) -> Option<RichText> {
        let spine = self.spine.as_ref()?;
        let (source_path, target_path, anchor) = {
            let uri = &link.uri;
            if let Some(rest) = uri.strip_prefix("#_doc_fragment_") {
                let (idx_str, frag) = rest.split_once('_')?;
                let idx: usize = idx_str.parse().ok()?;
                let target_path = spine.paths.get(idx)?.clone();
                let anchor = frag.trim().to_string();
                let source_path = self.crengine_spine_path_of_page(link.offset);
                (source_path, target_path, anchor)
            } else {
                let frag = uri.strip_prefix('#')?;
                let source_path = self.crengine_spine_path_of_page(link.offset)?;
                (
                    Some(source_path.clone()),
                    source_path,
                    frag.trim().to_string(),
                )
            }
        };
        if self.notes.is_none() {
            self.notes = notes::Source::open(&self.source);
        }
        self.notes.as_mut()?.footnote_target(
            source_path.as_deref(),
            &target_path,
            &anchor,
            link_text,
        )
    }

    /// The note behind `link` if it is a footnote (see [`crate::notes`]);
    /// `link_text` is the marker the link shows.
    pub fn footnote(&mut self, link: &Link, link_text: &str) -> Option<RichText> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine_footnote(link, link_text);
        }
        if self.engine != Engine::Plato {
            return None;
        }
        let base = self.spine_path(link.offset)?;
        if self.notes.is_none() {
            self.notes = notes::Source::open(&self.source);
        }
        self.notes.as_mut()?.footnote(&base, &link.uri, link_text)
    }

    /// Start of the page a link in the page at `location` leads to.
    pub fn link_page(&mut self, location: usize, uri: &str) -> Option<usize> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            let links = self.crengine.as_ref()?.page_links(location);
            let link = links.into_iter().find(|l| l.href == uri)?;
            return match link.target {
                inkread_engine::LinkTarget::Internal { xpointer, page } => {
                    if self.is_rerender_delayed() && !xpointer.is_empty() {
                        self.settled_page_of_xpointer(&xpointer)
                    } else if let Some(p) = page {
                        Some(p as usize)
                    } else if !xpointer.is_empty() {
                        self.page_of_xpointer(&xpointer)
                    } else {
                        None
                    }
                }
                inkread_engine::LinkTarget::External { .. } => None,
            };
        }
        if self.engine != Engine::Plato {
            return None;
        }
        let target = self
            .doc
            .resolve_location(Location::LocalUri(location, uri.to_string()))?;
        self.page_start(target)
    }

    /// The table of contents, flattened with depths.
    pub fn toc_items(&self) -> Vec<TocItem> {
        if self.engine == Engine::Vertical {
            let Some(doc) = self.vertical.as_ref() else {
                return Vec::new();
            };
            return doc
                .toc()
                .iter()
                .enumerate()
                .map(|(index, entry)| TocItem {
                    title: entry.title.clone(),
                    depth: entry.depth,
                    index,
                    page: None,
                })
                .collect();
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            let omit_page = self.is_rerender_delayed();
            return self
                .crengine_toc
                .iter()
                .enumerate()
                .map(|(index, entry)| TocItem {
                    title: entry.title.trim().to_string(),
                    depth: entry.level as usize,
                    index,
                    page: if omit_page {
                        None
                    } else {
                        Some(entry.page as usize)
                    },
                })
                .collect();
        }
        let mut flat = Vec::new();
        flatten(&self.toc, 0, &mut flat);
        flat.into_iter()
            .map(|(depth, e)| TocItem {
                title: e.title.trim().to_string(),
                depth,
                index: e.index,
                page: None,
            })
            .collect()
    }

    /// Start of the page a table of contents entry points at (lays out its
    /// chapter).
    pub fn toc_page(&mut self, index: usize) -> Option<usize> {
        if self.engine == Engine::Vertical {
            let start = self.vertical.as_ref()?.toc_start(index)?;
            let page_start = self.page_start(start)?;
            return self.vertical_page_index(page_start);
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            let entry = self.crengine_toc.get(index)?;
            if self.is_rerender_delayed() && !entry.xpointer.is_empty() {
                let xpointer = entry.xpointer.clone();
                return self.settled_page_of_xpointer(&xpointer);
            }
            return Some(entry.page as usize);
        }
        let mut flat = Vec::new();
        flatten(&self.toc, 0, &mut flat);
        let (_, entry) = flat.into_iter().find(|(_, e)| e.index == index)?;
        let target = self.doc.resolve_location(entry.location)?;
        self.page_start(target)
    }

    /// The index of the table of contents entry for `location`.
    pub fn toc_current(&mut self, location: usize) -> Option<usize> {
        if self.engine == Engine::Vertical {
            let doc = self.vertical.as_ref()?;
            let chapter = doc.chapter_of(location)?;
            return doc.toc_index_of(chapter);
        }
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine_chapter_index(location);
        }
        self.doc
            .chapter(location, &self.toc)
            .map(|(entry, _)| entry.index)
    }

    /// Where a table of contents entry starts, found from the spine and the
    /// target file's ids without laying anything out (Plato's own resolver
    /// lays out the chapter, a page turn's time per entry).
    fn toc_location_cheap(&mut self, entry: &TocEntry) -> Option<usize> {
        if let Some(found) = self.toc_locations.get(&entry.index) {
            return *found;
        }
        let found = match &entry.location {
            Location::Exact(n) => Some(*n),
            Location::Uri(uri) => {
                let (path, frag) = match uri.split_once('#') {
                    Some((p, f)) => (p, Some(f)),
                    None => (uri.as_str(), None),
                };
                let start = self
                    .spine
                    .as_ref()
                    .and_then(|s| s.item_of(path).and_then(|i| s.start(i)));
                match (start, frag) {
                    (Some(start), Some(frag)) => {
                        if self.notes.is_none() {
                            self.notes = notes::Source::open(&self.source);
                        }
                        let offset = self.notes.as_mut().and_then(|n| n.id_offset(path, frag));
                        Some(start + offset.unwrap_or(0))
                    }
                    (start, _) => start,
                }
            }
            _ => None,
        };
        self.toc_locations.insert(entry.index, found);
        found
    }

    /// Which chapter `location` is in and where it ends. Chapters are the
    /// table of contents entries at the first level with more than one
    /// entry (a book whose contents is one title with chapters under it
    /// counts the chapters).
    pub fn chapter_info(&mut self, location: usize) -> Option<ChapterInfo> {
        #[cfg(feature = "crengine")]
        if self.engine == Engine::Crengine {
            return self.crengine_chapter_info(location);
        }
        if self.engine == Engine::Vertical {
            let doc = self.vertical.as_ref()?;
            let chapter = doc.chapter_of(location)?;
            let (start, end) = doc.chapter_range(chapter);
            return Some(ChapterInfo {
                number: chapter + 1,
                count: doc.chapters.len(),
                start,
                end,
            });
        }
        if self.engine != Engine::Plato || self.toc.is_empty() {
            return None;
        }
        let mut flat = Vec::new();
        flatten(&self.toc, 0, &mut flat);
        let max_depth = flat.iter().map(|(d, _)| *d).max().unwrap_or(0);
        let level = (0..=max_depth)
            .find(|&d| flat.iter().filter(|(x, _)| *x == d).count() > 1)
            .unwrap_or(0);
        let mut starts: Vec<usize> = Vec::new();
        for (_, e) in flat.iter().filter(|(d, _)| *d == level) {
            if let Some(l) = self.toc_location_cheap(e) {
                starts.push(l);
            }
        }
        starts.sort_unstable();
        starts.dedup();
        let number = starts.partition_point(|&s| s <= location);
        if number == 0 {
            return None;
        }
        Some(ChapterInfo {
            number,
            count: starts.len(),
            start: starts[number - 1],
            end: starts.get(number).copied().unwrap_or(self.len()),
        })
    }

    /// The chapter a page belongs to, from crengine-ng's own table of
    /// contents.
    #[cfg(feature = "crengine")]
    fn crengine_chapter_index(&self, location: usize) -> Option<usize> {
        crate::crengine::chapter_of_page(&self.crengine_toc, location as u32)
    }

    /// The level the chapters of the book are at: the first level of the
    /// table of contents with more than one entry.
    #[cfg(feature = "crengine")]
    fn crengine_chapter_level(&self) -> u32 {
        let max_level = self.crengine_toc.iter().map(|e| e.level).max().unwrap_or(0);
        (0..=max_level)
            .find(|&level| {
                self.crengine_toc
                    .iter()
                    .filter(|e| e.level == level)
                    .count()
                    > 1
            })
            .unwrap_or(0)
    }

    /// The pages the chapters of the book start on.
    #[cfg(feature = "crengine")]
    fn crengine_chapter_starts(&self) -> Vec<usize> {
        let level = self.crengine_chapter_level();
        let mut starts: Vec<usize> = self
            .crengine_toc
            .iter()
            .filter(|e| e.level == level)
            .map(|e| e.page as usize)
            .collect();
        starts.sort_unstable();
        starts.dedup();
        starts
    }

    /// Which chapter `location` is in and where it ends.
    #[cfg(feature = "crengine")]
    fn crengine_chapter_info(&self, location: usize) -> Option<ChapterInfo> {
        let starts = self.crengine_chapter_starts();
        if starts.is_empty() {
            return None;
        }
        let number = starts.partition_point(|&s| s <= location);
        if number == 0 {
            return None;
        }
        Some(ChapterInfo {
            number,
            count: starts.len(),
            start: starts[number - 1],
            end: starts.get(number).copied().unwrap_or(self.len()),
        })
    }

    /// The page of the next (or previous) chapter.
    #[cfg(feature = "crengine")]
    fn crengine_chapter_page(&self, location: usize, forward: bool) -> Option<usize> {
        let starts = self.crengine_chapter_starts();
        if forward {
            starts.iter().copied().find(|&page| page > location)
        } else {
            starts.iter().copied().rev().find(|&page| page < location)
        }
    }
}

/// Order two XPointers into the same text node (`…/text().N`) by their
/// character offset. XPointers into different nodes are not ordered here:
/// comparing them as strings is wrong (`p[10]` sorts before `p[2]`,
/// `text().36` before `text().4`).
pub fn xpointer_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (node_a, at_a) = a.rsplit_once('.')?;
    let (node_b, at_b) = b.rsplit_once('.')?;
    if node_a != node_b {
        return None;
    }
    Some(at_a.parse::<u32>().ok()?.cmp(&at_b.parse::<u32>().ok()?))
}

/// Locate the starting word index on a page for `xpointer`.
pub fn mark_start_word_index(words: &[Word], xpointer: Option<&str>) -> usize {
    let Some(xpointer) = xpointer else {
        return 0;
    };
    if let Some(idx) = words
        .iter()
        .position(|w| w.start_xpointer.as_deref() == Some(xpointer))
    {
        return idx;
    }
    // Inside a word (a highlight imported from KOReader may start mid-word):
    // the first word ending after it.
    if let Some(idx) = words.iter().position(|w| {
        w.end_xpointer
            .as_deref()
            .and_then(|xp| xpointer_cmp(xp, xpointer))
            == Some(std::cmp::Ordering::Greater)
    }) {
        return idx;
    }
    0
}

/// Locate the ending word index on a page for `xpointer`.
pub fn mark_end_word_index(words: &[Word], xpointer: Option<&str>) -> usize {
    let Some(xpointer) = xpointer else {
        return words.len().saturating_sub(1);
    };
    if let Some(idx) = words
        .iter()
        .rposition(|w| w.end_xpointer.as_deref() == Some(xpointer))
    {
        return idx;
    }
    // Inside a word: the last word starting before it.
    if let Some(idx) = words.iter().rposition(|w| {
        w.start_xpointer
            .as_deref()
            .and_then(|xp| xpointer_cmp(xp, xpointer))
            == Some(std::cmp::Ordering::Less)
    }) {
        return idx;
    }
    words.len().saturating_sub(1)
}

#[cfg(test)]
mod xpointer_order_tests {
    use super::xpointer_cmp;
    use std::cmp::Ordering;

    #[test]
    fn offsets_in_one_text_node_compare_as_numbers() {
        let node = "/body/DocFragment[2]/body/p[2]/text()";
        assert_eq!(
            xpointer_cmp(&format!("{node}.36"), &format!("{node}.4")),
            Some(Ordering::Greater)
        );
        assert_eq!(
            xpointer_cmp(&format!("{node}.4"), &format!("{node}.4")),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn different_nodes_are_not_ordered() {
        assert_eq!(
            xpointer_cmp(
                "/body/DocFragment[2]/body/p[10]/text().0",
                "/body/DocFragment[2]/body/p[2]/text().0"
            ),
            None
        );
    }
}
