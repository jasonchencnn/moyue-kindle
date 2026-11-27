// SPDX-License-Identifier: AGPL-3.0-or-later
//! One open book: where the reader is, the pages rendered around it, and
//! the page numbers once known.
//!
//! Positions are Plato locations (page starts), never page indices, so a
//! book opens at its saved place without laying out everything before it.
//! The pages next to the current one are rendered ahead ("prerender") while
//! the panel shows the current one, so a turn only costs the panel's time.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aristotle_i18n::{tf, Msg};
use aristotle_library::index::AnnotationKind;
use aristotle_library::{BookRecord, Library, Progress};
use aristotle_render::book::{
    Book, CrengineMark, Engine, Link, Mark, PageFootnote, Position, Word,
};
use aristotle_render::page::{Chrome, Page};
use aristotle_render::plato_core::anyhow::{format_err, Context, Error};
use aristotle_render::style::{Style, Underline};
use aristotle_render::txt::{self, CONVERTER_VERSION};

use crate::pagecache::{self, Paginator};
use crate::progress;
use crate::turns::Turn;

/// Rendered pages kept around: the current one and its two neighbours,
/// plus one spare so a back-and-forth does not render twice.
const KEEP_PAGES: usize = 4;

pub struct Session {
    pub id: String,
    pub title: String,
    stamp: (u64, i64),
    pub style: Style,
    /// Set for a TXT book read through the generated EPUB: the converter
    /// version that names this layout (see [`aristotle_render::txt`]).
    txt_version: Option<u32>,
    /// The converter-1 split, loaded by the open worker, for translating
    /// stored marks when the reader adds or removes one.
    txt_legacy: Option<Arc<txt::LegacyMap>>,
    /// Stored converter-1 positions translated for this open; the index is
    /// rewritten once so later opens do not translate again.
    txt_migrated: usize,
    book: Option<Book>,
    pub degraded: bool,
    last_fraction: Cell<f32>,
    chrome: Chrome,
    /// Start of the page on the panel.
    pub current: usize,
    pages: Vec<(usize, Rc<Page>)>,
    /// The location whose entry in `pages` is a kept snapshot page (see
    /// [`Session::keep_page`]).
    kept: Option<usize>,
    next_of: HashMap<usize, Option<usize>>,
    prev_of: HashMap<usize, Option<usize>>,
    paginator: Option<Paginator>,
    /// Highlights, bookmarks and reading speed drawn on pages.
    pub deco: Deco,
    pub raw_marks: Vec<(Option<usize>, Position, Position)>,
    pub raw_bookmarks: Vec<Position>,
    /// Words and links of the last page asked for.
    words: Option<(usize, Rc<Vec<Word>>)>,
    links: Option<(usize, Rc<Vec<Link>>)>,
    /// Footnotes of the current page, resolved on demand.
    footnotes: Option<(usize, Rc<Vec<PageFootnote>>)>,
}

/// What the timing log reports about opening a book.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenTimes {
    pub open: Duration,
    pub place: Duration,
    pub render: Duration,
    /// Rendering the pages on either side, when opened with neighbours.
    pub neighbours: Duration,
    pub pages_cached: bool,
    /// How long the worker waited to acquire OPEN_GATE.
    pub lock_wait: Duration,
}

/// Where a book is and where it was left: everything [`Session::prepare`]
/// needs from the library, owned, so it can go to another thread.
#[derive(Clone)]
pub struct Source {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
    pub stamp: (u64, i64),
    pub data_dir: PathBuf,
    pub mobi_cache: PathBuf,
    pub saved: Option<Progress>,
    /// Highlights and bookmarks made here, as saved positions; resolved
    /// once the book is open, so the first page already shows them.
    pub marks: Vec<(Option<usize>, Position, Position)>,
    pub bookmarks: Vec<Position>,
    pub secs_per_page: f32,
    /// A page start to open at instead of placing `saved`: the location of
    /// a snapshot made for exactly this progress, so the book opens on the
    /// page the snapshot shows, whatever rounding the locator went through.
    pub exact: Option<usize>,
    /// A saved annotation to open at instead of `saved`: the card tapped on
    /// "My highlights and notes". Resolved against the open book, so an
    /// imported Kindle position can use the MOBI position map.
    pub jump: Option<aristotle_library::index::Annotation>,
    /// The book's format, for resolving [`Source::jump`].
    pub format: aristotle_library::Format,
    /// The TXT converter version that lays this book out, when it is a TXT
    /// book read through the generated EPUB. Progress and new annotations
    /// are stamped with it (see [`aristotle_render::txt::source_for`]).
    pub txt_version: Option<u32>,
    /// Converter version tags of [`Source::marks`], in the same order.
    pub mark_versions: Vec<Option<u32>>,
    /// Converter version tags of [`Source::bookmarks`], in the same order.
    pub bookmark_versions: Vec<Option<u32>>,
    /// How many stored converter-1 positions were translated exactly.
    pub txt_mapped: usize,
    /// Stored positions that could not be translated; they are kept in the
    /// index, never moved to different text.
    pub txt_unmapped: Vec<String>,
    /// The converter-1 split of the source text, loaded by the open worker
    /// when a legacy position needs translating.
    pub txt_legacy: Option<std::sync::Arc<txt::LegacyMap>>,
    /// Whether to paginate the whole book in the background when its page
    /// numbers are not cached (not for snapshots made ahead of time).
    pub paginate: bool,
    /// Set by whoever stops waiting for this open; [`Session::prepare`]
    /// gives up between its steps.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Counter updated with formatting progress (0..=100) on engines that report it.
    pub progress: Option<Arc<AtomicU8>>,
    pub night: bool,
    /// The guide under every line of body text (KO2-UNDERLINE-001).
    pub underline: Underline,
}

/// Held by every thread that opens a book in the background (an open
/// worker, a snapshot made ahead of time) from the start of
/// [`Session::prepare`] until its result is handed over or dropped, so an
/// abandoned open never holds a laid-out book next to a new one.
pub static OPEN_GATE: Mutex<()> = Mutex::new(());

impl Source {
    /// What opening `rec` needs from the library. Metadata only: reading the
    /// book (and translating converter-1 positions) happens on the open
    /// worker in [`Session::prepare`], never on the UI thread.
    pub fn new(lib: &Library, rec: &BookRecord, mobi_cache: &Path) -> Result<Source, Error> {
        let path = lib
            .absolute_path(rec)
            .ok_or_else(|| format_err!("the library has no root"))?;
        // The file itself, in case it changed since the last scan.
        let stamp = crate::snapshot::file_stamp(&path).unwrap_or((rec.file.size, rec.file.mtime));
        let txt_converted = rec.format == aristotle_library::Format::Txt
            && aristotle_render::book::epub_engine_is_crengine();
        let highlights = crate::marks::ours_versioned(rec, AnnotationKind::Highlight);
        let bookmarks = crate::marks::ours_versioned(rec, AnnotationKind::Bookmark);
        Ok(Source {
            id: rec.id.clone(),
            title: rec.meta.title.clone(),
            path,
            stamp,
            data_dir: lib.data_dir().to_path_buf(),
            mobi_cache: mobi_cache.to_path_buf(),
            saved: rec.progress.clone(),
            marks: highlights
                .iter()
                .map(|(i, s, e, _)| (Some(*i), s.clone(), e.clone()))
                .collect(),
            mark_versions: highlights.iter().map(|(_, _, _, v)| *v).collect(),
            bookmarks: bookmarks.iter().map(|(_, s, _, _)| s.clone()).collect(),
            bookmark_versions: bookmarks.iter().map(|(_, _, _, v)| *v).collect(),
            secs_per_page: crate::speed::DEFAULT_SECS_PER_PAGE,
            exact: None,
            jump: None,
            format: rec.format,
            txt_version: txt_converted.then_some(CONVERTER_VERSION),
            txt_mapped: 0,
            txt_unmapped: Vec::new(),
            txt_legacy: None,
            paginate: true,
            cancel: None,
            progress: None,
            night: false,
            underline: Underline::Off,
        })
    }

    /// Whether the saved progress names a layout older than this build.
    ///
    /// Cheap on purpose: snapshot and prewarm lookup run on the UI thread and
    /// must not read the TXT. A legacy position is not used for those keys;
    /// the book opens at it through [`Session::prepare`], which translates it,
    /// and the next normal progress save stores the current pointer.
    pub fn saved_needs_txt_migration(&self) -> bool {
        if self.txt_version.is_none() {
            return false;
        }
        let Some(saved) = self.saved.as_ref() else {
            return false;
        };
        if !matches!(saved.locator, aristotle_library::Locator::XPointer { .. }) {
            return false;
        }
        stored_txt_version(txt::version_from_source(saved.source.as_deref())) != CONVERTER_VERSION
    }

    /// Whether any stored highlight or bookmark needs translating.
    fn marks_need_txt_migration(&self) -> bool {
        self.txt_version.is_some()
            && self
                .mark_versions
                .iter()
                .chain(self.bookmark_versions.iter())
                .any(|v| stored_txt_version(*v) != CONVERTER_VERSION)
    }
}

/// Translate the converter-1 positions in `src` for this build's layout.
///
/// Runs on the open worker only: it reads and splits the source TXT. The
/// library records are not modified; untranslatable marks are dropped from
/// this open and reported, never moved to different text.
pub fn migrate_txt_source(src: &mut Source) {
    if src.txt_version.is_none()
        || (!src.saved_needs_txt_migration() && !src.marks_need_txt_migration())
    {
        return;
    }
    let map = match txt::legacy_map(&src.path) {
        Ok(map) => map,
        Err(e) => {
            src.txt_unmapped
                .push(format!("can't read the source text: {e}"));
            if let Some(saved) = src.saved.as_mut() {
                saved.locator = fallback_locator(saved.fraction);
            }
            src.marks.clear();
            src.bookmarks.clear();
            src.mark_versions.clear();
            src.bookmark_versions.clear();
            return;
        }
    };
    src.txt_legacy = Some(map.clone());
    if let Some(saved) = src.saved.as_mut() {
        if matches!(saved.locator, aristotle_library::Locator::XPointer { .. }) {
            let version = stored_txt_version(txt::version_from_source(saved.source.as_deref()));
            if version != CONVERTER_VERSION {
                let aristotle_library::Locator::XPointer { xpointer } = &saved.locator else {
                    unreachable!()
                };
                let mapped = if version == 1 {
                    map.map_xpointer(xpointer)
                } else {
                    Err(txt::MapError::Unsupported)
                };
                match mapped {
                    Ok(xpointer) => {
                        saved.locator = aristotle_library::Locator::XPointer { xpointer };
                        saved.source = Some(txt::source_for(CONVERTER_VERSION));
                        src.txt_mapped += 1;
                    }
                    Err(e) => {
                        src.txt_unmapped
                            .push(format!("progress: {}: {e:?}", e.reason()));
                        saved.locator = fallback_locator(saved.fraction);
                        saved.source = Some(txt::source_for(CONVERTER_VERSION));
                    }
                }
            }
        }
    }
    let mut drops = Vec::new();
    let versions = src.mark_versions.clone();
    for (i, version) in versions.iter().enumerate() {
        if stored_txt_version(*version) == CONVERTER_VERSION {
            continue;
        }
        let mapped = translate_stored_with(
            &map,
            src.mark_versions[i],
            &src.marks[i].1,
            &mut src.txt_unmapped,
        )
        .and_then(|s| {
            translate_stored_with(
                &map,
                src.mark_versions[i],
                &src.marks[i].2,
                &mut src.txt_unmapped,
            )
            .map(|e| (s, e))
        });
        match mapped {
            Some((s, e)) => {
                src.marks[i].1 = s;
                src.marks[i].2 = e;
                src.mark_versions[i] = Some(CONVERTER_VERSION);
                src.txt_mapped += 1;
            }
            None => drops.push(i),
        }
    }
    for i in drops.into_iter().rev() {
        src.marks.remove(i);
        src.mark_versions.remove(i);
    }
    let mut drops = Vec::new();
    let versions = src.bookmark_versions.clone();
    for (i, version) in versions.iter().enumerate() {
        if stored_txt_version(*version) == CONVERTER_VERSION {
            continue;
        }
        match translate_stored_with(
            &map,
            src.bookmark_versions[i],
            &src.bookmarks[i],
            &mut src.txt_unmapped,
        ) {
            Some(bookmark) => {
                src.bookmarks[i] = bookmark;
                src.bookmark_versions[i] = Some(CONVERTER_VERSION);
                src.txt_mapped += 1;
            }
            None => drops.push(i),
        }
    }
    for i in drops.into_iter().rev() {
        src.bookmarks.remove(i);
        src.bookmark_versions.remove(i);
    }
}

/// A locator for the fraction fallback that crengine-ng cannot place as an
/// XPointer, and that still identifies its fraction in a snapshot key.
fn fallback_locator(fraction: f32) -> aristotle_library::Locator {
    aristotle_library::Locator::Page {
        index: (fraction.clamp(0.0, 1.0) * 1_000_000.0) as u32,
    }
}

fn translate_stored_with(
    map: &txt::LegacyMap,
    tagged: Option<u32>,
    position: &Position,
    notes: &mut Vec<String>,
) -> Option<Position> {
    let version = stored_txt_version(tagged);
    if version == CONVERTER_VERSION {
        return Some(position.clone());
    }
    if version != 1 {
        notes.push(format!(
            "{position:?}: written by converter {version}, this build reads {CONVERTER_VERSION}"
        ));
        return None;
    }
    match map.map_position(position) {
        Ok(position) => Some(position),
        Err(e) => {
            notes.push(format!("{position:?}: {}", e.reason()));
            None
        }
    }
}

/// The converter version a stored position names: tagged, or converter 1
/// for records that predate tags (the only version that shipped).
fn stored_txt_version(tagged: Option<u32>) -> u32 {
    tagged.unwrap_or(1)
}

/// A book opened, placed and rendered: the slow part of opening, done by
/// [`Session::prepare`] on any thread. Unlike a [`Session`] it is `Send`
/// (no `Rc` pages, no FreeType chrome fonts), so a worker thread can make
/// it while the main loop shows a snapshot and keeps taking input.
pub struct Prepared {
    src: Source,
    style: Style,
    book: Book,
    pub current: usize,
    pages: Vec<(usize, Page)>,
    next_of: HashMap<usize, Option<usize>>,
    prev_of: HashMap<usize, Option<usize>>,
    paginator: Option<Paginator>,
    deco: Deco,
    pub times: OpenTimes,
}

impl Prepared {
    /// The page start after the current one, if the worker found it.
    pub fn next(&self) -> Option<usize> {
        self.next_of.get(&self.current).copied().flatten()
    }
}

impl Session {
    /// Open the book `rec` at its saved position with `style`.
    pub fn open(
        lib: &Library,
        rec: &BookRecord,
        style: Style,
        mobi_cache: &Path,
    ) -> Result<(Session, OpenTimes), Error> {
        let prepared = Session::prepare(Source::new(lib, rec, mobi_cache)?, style, false)?;
        let times = prepared.times;
        Ok((Session::from_prepared(prepared)?, times))
    }

    /// Open, place at the saved position and render the first page (and,
    /// with `neighbours`, the pages on either side), without touching
    /// anything the main thread owns.
    pub fn prepare(mut src: Source, style: Style, neighbours: bool) -> Result<Prepared, Error> {
        let cancel = src.cancel.clone();
        let cancelled = || {
            if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                Err(format_err!("cancelled"))
            } else {
                Ok(())
            }
        };
        cancelled()?;
        migrate_txt_source(&mut src);
        if !src.txt_unmapped.is_empty() {
            for note in &src.txt_unmapped {
                eprintln!(
                    "aristotle: \"{}\": keeping a stored TXT position instead of moving it: {note}",
                    src.title
                );
            }
        }
        let t = Instant::now();
        let mut book = Book::open_with_progress(
            &src.path,
            &style,
            Some(&src.mobi_cache),
            src.progress.as_ref(),
        )
        .with_context(|| format!("can't open {}", src.path.display()))?;
        book.set_night(src.night);
        book.set_underline(src.underline);
        // The chrome fonts of this thread: FreeType faces can't move to
        // another one, so the session loads its own.
        let mut chrome = Chrome::load(&style).context("can't load the chrome fonts")?;
        let t_open = t.elapsed();
        cancelled()?;
        let (pages_cached, paginator) = start_pagination(
            &mut book,
            &style,
            &src.data_dir,
            &src.id,
            src.stamp,
            &src.path,
            src.paginate.then_some(src.mobi_cache.as_path()),
        );
        let t = Instant::now();
        let jump = src.jump.take().and_then(|annotation| {
            let position =
                crate::marks::target(&annotation, src.format, book.mobi().map(|m| &m.info))?;
            let tagged = annotation
                .position
                .as_ref()
                .and_then(|(start, _)| crate::marks::version_of(start));
            match (src.txt_version, src.txt_legacy.as_ref()) {
                (Some(version), Some(map)) if stored_txt_version(tagged) != version => {
                    map.map_position(&position).ok()
                }
                (Some(version), None) if stored_txt_version(tagged) != version => None,
                _ => Some(position),
            }
        });
        let target = src
            .exact
            .or_else(|| jump.and_then(|position| book.location_of(&position)))
            .unwrap_or_else(|| {
                progress::positions(
                    book.engine(),
                    src.saved.as_ref(),
                    progress::stock_positions_compatible(&book),
                )
                .iter()
                .find_map(|p| book.location_of(p))
                .unwrap_or(0)
            });
        let current = book
            .page_start(target)
            .ok_or_else(|| format_err!("the book has no pages"))?;
        let t_place = t.elapsed();
        cancelled()?;
        let t = Instant::now();
        let deco = Deco {
            marks: src
                .marks
                .iter()
                .filter_map(|(id, s, e)| book.resolve_mark(*id, s, e))
                .collect(),
            bookmarks: src
                .bookmarks
                .iter()
                .filter_map(|p| book.location_of(p))
                .collect(),
            secs_per_page: src.secs_per_page,
            status: Default::default(),
        };
        let first_next = if neighbours {
            book.next_page(current)
        } else {
            None
        };
        let mut pages = vec![(
            current,
            render_page(&mut book, &mut chrome, &deco, current, first_next, false)?,
        )];
        let t_render = t.elapsed();
        cancelled()?;
        let t = Instant::now();
        let (mut next_of, mut prev_of) = (HashMap::new(), HashMap::new());
        if neighbours {
            let next = book.next_page(current);
            let prev = book.previous_page(current);
            next_of.insert(current, next);
            prev_of.insert(current, prev);
            for loc in [next, prev].into_iter().flatten() {
                let after = if Some(loc) == next {
                    None
                } else {
                    Some(current)
                };
                match render_page(&mut book, &mut chrome, &deco, loc, after, false) {
                    Ok(page) => pages.push((loc, page)),
                    Err(e) => eprintln!("aristotle: prerender: {e:#}"),
                }
            }
        }
        let t_neighbours = t.elapsed();
        Ok(Prepared {
            src,
            style,
            book,
            current,
            pages,
            next_of,
            prev_of,
            paginator,
            deco,
            times: OpenTimes {
                open: t_open,
                place: t_place,
                render: t_render,
                neighbours: t_neighbours,
                pages_cached,
                lock_wait: Duration::ZERO,
            },
        })
    }

    /// The session for a prepared book, on the thread that will use it.
    pub fn from_prepared(p: Prepared) -> Result<Session, Error> {
        let chrome = Chrome::load(&p.style).context("can't load the chrome fonts")?;
        let initial_fraction = p
            .src
            .saved
            .as_ref()
            .map(|pr| pr.fraction)
            .unwrap_or_else(|| p.book.fraction(p.current));
        let txt_version = p.src.txt_version;
        let txt_legacy = p.src.txt_legacy.clone();
        let txt_migrated = p.src.txt_mapped;
        Ok(Session {
            id: p.src.id,
            title: p.src.title,
            stamp: p.src.stamp,
            style: p.style,
            txt_version,
            txt_legacy,
            txt_migrated,
            book: Some(p.book),
            degraded: false,
            last_fraction: Cell::new(initial_fraction),
            chrome,
            current: p.current,
            pages: p
                .pages
                .into_iter()
                .map(|(l, pg)| (l, Rc::new(pg)))
                .collect(),
            kept: None,
            next_of: p.next_of,
            prev_of: p.prev_of,
            paginator: p.paginator,
            deco: p.deco,
            raw_marks: p.src.marks,
            raw_bookmarks: p.src.bookmarks,
            words: None,
            links: None,
            footnotes: None,
        })
    }

    /// Adopt a finished background pagination. Returns true if the page
    /// numbers just became known (rendered pages are then stale: their
    /// footer lacks "x / y").
    pub fn poll_pagination(&mut self) -> bool {
        let Some(result) = self.paginator.as_ref().and_then(Paginator::poll) else {
            return false;
        };
        self.paginator = None;
        match result {
            Some(pages) => {
                if let Some(book) = self.book.as_mut() {
                    book.set_pagination(pages);
                    self.pages.clear();
                    self.recompute_deco();
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    /// Whether laying the book out again at the current page (a new font
    /// size, spacing or margins) will take long enough to show the busy
    /// mark: about 300 ms on the device. Plato lays out the current
    /// chapter from its start up to the page, at about 6 ms per KB of
    /// chapter text on the PW4 (a 420 KB one-chapter novel in 2.5 s).
    /// The other engines lay the whole book out again, which takes
    /// seconds whatever the book is like.
    pub fn slow_relayout(&self) -> bool {
        let Some(book) = self.book.as_ref() else {
            return false;
        };
        if book.engine() != Engine::Plato {
            return true;
        }
        const SLOW_BYTES: usize = 48 * 1024;
        book.layout_bytes(self.current) > SLOW_BYTES
    }

    /// The most exact position of the page on the panel.
    pub fn position(&self) -> Position {
        self.book
            .as_ref()
            .map(|b| b.position_at(self.current))
            .unwrap_or(Position::Fraction(0.0))
    }

    /// The page on the panel as a 1-based number and the book's page count
    /// in this layout, once both are known (see
    /// [`Book::known_page_count`]).
    pub fn page_number(&self) -> Option<(u32, u32)> {
        let book = self.book.as_ref()?;
        let index = book.index_of(self.current)?;
        let total = book.known_page_count()?;
        Some((u32::try_from(index + 1).ok()?, u32::try_from(total).ok()?))
    }

    /// The page of this layout that holds `position`, if the book can place
    /// it. This is how the reader comes back to the same words after the
    /// book was laid out again.
    pub fn page_of(&mut self, position: &Position) -> Option<usize> {
        let book = self.book.as_mut()?;
        let location = book.location_of(position)?;
        book.page_start(location)
    }

    /// The book file's size and mtime when the library last saw it.
    pub fn stamp(&self) -> (u64, i64) {
        self.stamp
    }

    pub fn paginating(&self) -> bool {
        self.paginator.is_some()
    }

    /// The rendered page starting at `location`.
    pub fn page(&mut self, location: usize) -> Result<Rc<Page>, Error> {
        if let Some(i) = self.pages.iter().position(|(l, _)| *l == location) {
            let entry = self.pages.remove(i);
            let page = entry.1.clone();
            self.pages.push(entry);
            return Ok(page);
        }
        let next = self.neighbor(location, Turn::Forward);
        let degraded = self.degraded;
        let book = self
            .book
            .as_mut()
            .ok_or_else(|| format_err!("book unavailable"))?;
        let page = Rc::new(render_page(
            book,
            &mut self.chrome,
            &self.deco,
            location,
            next,
            degraded,
        )?);
        if self.pages.len() >= KEEP_PAGES {
            self.pages.remove(0);
        }
        self.pages.push((location, page.clone()));
        if self.kept == Some(location) {
            self.kept = None;
        }
        Ok(page)
    }

    /// Use `page` as the rendered page at `location`: a snapshot's page that
    /// differs from a fresh render only in its header or footer, already on
    /// the panel (see `App::adopt`). It stays while the reader is on that
    /// page; once a turn or a jump leaves it ([`Session::prerender`]), or
    /// new page numbers or marks redraw the pages, coming back renders the
    /// page anew.
    pub fn keep_page(&mut self, location: usize, page: Rc<Page>) {
        self.pages.retain(|(l, _)| *l != location);
        if self.pages.len() >= KEEP_PAGES {
            self.pages.remove(0);
        }
        self.pages.push((location, page));
        self.kept = Some(location);
    }

    /// Forget the kept snapshot page unless it is the current page.
    fn drop_kept_page(&mut self) {
        if let Some(l) = self.kept.filter(|&l| l != self.current) {
            self.pages.retain(|(p, _)| *p != l);
            self.kept = None;
        }
    }

    pub fn is_rendered(&self, location: usize) -> bool {
        self.pages.iter().any(|(l, _)| *l == location)
    }

    /// The page at `location` if it is already rendered (never renders).
    pub fn rendered(&self, location: usize) -> Option<Rc<Page>> {
        self.pages
            .iter()
            .find(|(l, _)| *l == location)
            .map(|(_, p)| p.clone())
    }

    /// The page start after `location` if already known (never lays out).
    pub fn known_next(&self, location: usize) -> Option<usize> {
        self.next_of.get(&location).copied().flatten()
    }

    /// Whether `location` is known to be the last page (never lays out).
    pub fn known_last(&self, location: usize) -> bool {
        self.next_of.get(&location) == Some(&None)
    }

    /// The page start one page away from `location`.
    pub fn neighbor(&mut self, location: usize, dir: Turn) -> Option<usize> {
        let book = self.book.as_mut()?;
        let map = match dir {
            Turn::Forward => &mut self.next_of,
            Turn::Back => &mut self.prev_of,
        };
        *map.entry(location).or_insert_with(|| match dir {
            Turn::Forward => book.next_page(location),
            Turn::Back => book.previous_page(location),
        })
    }

    /// Render the pages on either side of the current one if they are not
    /// ready yet. Returns the time spent on each.
    pub fn prerender(&mut self) -> (Duration, Duration) {
        self.drop_kept_page();
        let mut spent = [Duration::ZERO; 2];
        for (i, dir) in [Turn::Forward, Turn::Back].into_iter().enumerate() {
            let t = Instant::now();
            if let Some(loc) = self.neighbor(self.current, dir) {
                if !self.is_rendered(loc) {
                    if let Err(e) = self.page(loc) {
                        eprintln!("aristotle: prerender: {e:#}");
                    }
                }
            }
            spent[i] = t.elapsed();
        }
        (spent[0], spent[1])
    }

    /// Whether `location` is the last page.
    pub fn at_end(&mut self) -> bool {
        self.neighbor(self.current, Turn::Forward).is_none()
    }

    /// Start of the next or previous chapter.
    pub fn chapter(&mut self, forward: bool) -> Option<usize> {
        self.book.as_mut()?.chapter_page(self.current, forward)
    }

    /// The progress record for the current page.
    pub fn progress(&self, now: i64) -> Progress {
        let Some(book) = self.book.as_ref() else {
            return Progress {
                fraction: self.last_fraction.get(),
                locator: aristotle_library::Locator::Page { index: 0 },
                updated: now,
                source: self.progress_source(),
                page: None,
            };
        };
        let pos = book.position_at(self.current);
        let fraction = if self.degraded {
            self.last_fraction.get()
        } else {
            let f = book.fraction(self.current);
            self.last_fraction.set(f);
            f
        };
        let mut progress = progress::progress_at(&pos, fraction, now);
        progress.source = self.progress_source();
        progress
    }

    /// The provenance written with this book's progress: a TXT book names
    /// the converter version, so a later converter can translate the
    /// XPointer instead of reading it against a different split.
    fn progress_source(&self) -> Option<String> {
        Some(
            self.txt_version
                .map(txt::source_for)
                .unwrap_or_else(|| "aristotle".into()),
        )
    }

    /// The TXT converter version of this book, if it is one.
    pub fn txt_version(&self) -> Option<u32> {
        self.txt_version
    }

    /// Stored converter-1 positions translated for this open.
    pub fn txt_migrated(&self) -> usize {
        self.txt_migrated
    }

    /// Translate a stored position for the current layout.
    ///
    /// `tagged` is the version encoded in the position; untagged positions
    /// are converter 1. Positions of other formats are returned unchanged.
    /// `None` means the position cannot be placed exactly and must not be
    /// attached to other text.
    pub fn map_stored_position(
        &self,
        position: &Position,
        tagged: Option<u32>,
    ) -> Option<Position> {
        if self.txt_version.is_none() {
            return Some(position.clone());
        }
        let version = stored_txt_version(tagged);
        if version == CONVERTER_VERSION {
            return Some(position.clone());
        }
        if version != 1 {
            return None;
        }
        self.txt_legacy.as_ref()?.map_position(position).ok()
    }

    /// Chapter title, "Page x of y" (or a percentage before the pages are
    /// known) and the fraction, for the menu.
    pub fn labels(&mut self) -> (String, String, f32) {
        if self.degraded {
            let chapter = self
                .book
                .as_mut()
                .and_then(|b| b.chapter_title(self.current))
                .unwrap_or_default();
            return (chapter, String::new(), self.last_fraction.get());
        }
        let Some(book) = self.book.as_mut() else {
            return (String::new(), String::new(), 0.0);
        };
        let chapter = book.chapter_title(self.current).unwrap_or_default();
        let fraction = book.fraction(self.current);
        let page = match (book.index_of(self.current), book.known_page_count()) {
            (Some(i), Some(n)) => format!("{} / {}", i + 1, n),
            _ => format!("{:.0}%", (fraction * 100.0).floor()),
        };
        (chapter, page, fraction)
    }

    // ------------------------------------------------ reading interactions

    pub fn book(&mut self) -> &mut Book {
        self.book
            .as_mut()
            .expect("book is currently lent to worker")
    }

    /// The book, unless a worker has it.
    pub fn book_ref(&self) -> Option<&Book> {
        self.book.as_ref()
    }

    /// Whether the loaded book is laid out by the vertical engine.
    ///
    /// The preference alone does not say: PDF and FB2 books never open
    /// vertically, and an EPUB or TXT the vertical engine cannot parse, or
    /// has no font for, falls back to the horizontal engine (VERT-101).
    pub fn is_vertical(&self) -> bool {
        self.book
            .as_ref()
            .is_some_and(|book| book.engine() == Engine::Vertical)
    }

    /// Whether the loaded book is laid out by the fixed-layout engine (CBZ, PDF).
    pub fn is_fixed(&self) -> bool {
        self.book
            .as_ref()
            .is_some_and(|book| book.engine() == Engine::Fixed)
    }

    /// Whether page-turn gestures should mirror horizontally (left tap / right
    /// swipe advances): true for vertical text and RTL fixed-layout comics.
    pub fn is_rtl_turns(&self) -> bool {
        self.is_vertical() || (self.is_fixed() && self.style.fixed_layout.direction.is_rtl())
    }

    /// Whether the book is here rather than with a layout worker.
    pub fn has_book(&self) -> bool {
        self.book.is_some()
    }

    pub fn take_book(&mut self) -> Option<Book> {
        self.book.take()
    }

    pub fn restore_book(&mut self, book: Book) {
        self.book = Some(book);
    }

    pub fn clear_pages(&mut self) {
        self.pages.clear();
        self.next_of.clear();
        self.prev_of.clear();
        self.words = None;
        self.links = None;
        self.footnotes = None;
        self.kept = None;
    }

    /// The words of the page starting at `location` (cached for the last
    /// page asked).
    pub fn words(&mut self, location: usize) -> Rc<Vec<Word>> {
        match &self.words {
            Some((l, w)) if *l == location => w.clone(),
            _ => {
                let w = Rc::new(
                    self.book
                        .as_mut()
                        .map(|b| b.words(location))
                        .unwrap_or_default(),
                );
                self.words = Some((location, w.clone()));
                w
            }
        }
    }

    pub fn links(&mut self, location: usize) -> Rc<Vec<Link>> {
        match &self.links {
            Some((l, w)) if *l == location => w.clone(),
            _ => {
                // While a layout has the book there are no links to give;
                // nothing is kept, so they are asked again once it is back.
                let Some(book) = self.book.as_mut() else {
                    return Rc::new(Vec::new());
                };
                let w = Rc::new(book.links(location));
                self.links = Some((location, w.clone()));
                w
            }
        }
    }

    /// Book footnotes referenced on the page starting at `location`
    /// (cached for the last page asked).
    pub fn footnotes(&mut self, location: usize) -> Rc<Vec<PageFootnote>> {
        match &self.footnotes {
            Some((l, f)) if *l == location => f.clone(),
            _ => {
                let Some(book) = self.book.as_mut() else {
                    return Rc::new(Vec::new());
                };
                let f = Rc::new(book.page_footnotes(location));
                self.footnotes = Some((location, f.clone()));
                f
            }
        }
    }

    /// New highlights and bookmarks: rendered pages are redrawn on demand.
    pub fn set_marks(&mut self, marks: Vec<Mark>, bookmarks: Vec<usize>) {
        if marks != self.deco.marks || bookmarks != self.deco.bookmarks {
            self.deco.marks = marks;
            self.deco.bookmarks = bookmarks;
            self.pages.clear();
        }
    }

    /// Recompute resolved decorations from raw positions using the current book layout.
    pub fn recompute_deco(&mut self) {
        if let Some(book) = self.book.as_ref() {
            self.deco.marks = self
                .raw_marks
                .iter()
                .filter_map(|(id, s, e)| book.resolve_mark(*id, s, e))
                .collect();
            self.deco.bookmarks = self
                .raw_bookmarks
                .iter()
                .filter_map(|p| book.location_of(p))
                .collect();
        }
    }

    /// Current chapter title at `location`, when known.
    pub fn chapter_title(&mut self, location: usize) -> Option<String> {
        self.book.as_mut()?.chapter_title(location)
    }

    pub fn set_speed(&mut self, secs_per_page: f32) {
        self.deco.secs_per_page = secs_per_page;
    }

    /// Update the live device status drawn in the reader's bars.
    /// Drops cached pages only when the displayed status actually changed.
    pub fn set_bar_status(&mut self, status: aristotle_render::page::DeviceBarStatus) {
        if self.deco.status != status {
            self.deco.status = status;
            self.pages.clear();
        }
    }

    /// Update the reader's bar configuration in-place (for changes that do not
    /// alter `text_top`/`text_bottom`). Drops cached page bitmaps so the next
    /// draw picks up the new bars without re-laying out the book.
    pub fn set_bar_config(&mut self, config: aristotle_screen::ReaderBarConfig) {
        if self.style.bar_config != config {
            self.style.bar_config = config;
            self.chrome.set_config(config);
            self.pages.clear();
        }
    }

    /// Whether the page starting at `location` holds a bookmark.
    pub fn bookmarked(&mut self, location: usize) -> Option<usize> {
        let next = self.neighbor(location, Turn::Forward);
        self.deco.bookmark_on(location, next)
    }
}

/// What is drawn on a page besides the book: the progress line, the
/// highlights and the bookmark ribbon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Deco {
    /// Highlights on the book.
    pub marks: Vec<Mark>,
    /// Bookmarks as Plato locations.
    pub bookmarks: Vec<usize>,
    pub secs_per_page: f32,
    /// Live device status (clock, battery, Wi-Fi, frontlight) for bar slots.
    pub status: aristotle_render::page::DeviceBarStatus,
}

impl Deco {
    fn bookmark_on(&self, location: usize, next: Option<usize>) -> Option<usize> {
        let next = next.unwrap_or(usize::MAX);
        self.bookmarks
            .iter()
            .copied()
            .find(|&b| b >= location && b < next)
    }
}

/// Render the page at `location` with the decorations. `next` is the next
/// page's start if known (bookmarks and the minutes estimate use it).
pub fn render_page(
    book: &mut Book,
    chrome: &mut Chrome,
    deco: &Deco,
    location: usize,
    next: Option<usize>,
    degraded: bool,
) -> Result<Page, Error> {
    let remaining = if degraded {
        None
    } else {
        remaining_line(book, deco.secs_per_page, location, next)
    };
    let book_remaining = if degraded {
        None
    } else {
        book_remaining_line(book, deco.secs_per_page, location, next)
    };
    let chapter_page = if degraded {
        None
    } else {
        chapter_page_info(book, location)
    };
    let has_footnotes = book.has_page_footnotes(location);
    chrome.set_status(deco.status.clone());
    chrome.set_page_extras(book_remaining, chapter_page);
    chrome.set_has_footnotes(has_footnotes);
    let mut page = book.render_location_with_degraded(location, chrome, remaining, degraded)?;
    if !deco.marks.is_empty() {
        if book.engine() == Engine::Plato {
            let plato_marks: Vec<(usize, usize)> = deco
                .marks
                .iter()
                .filter_map(|m| match m {
                    Mark::Plato(s, e) => Some((*s, *e)),
                    _ => None,
                })
                .collect();
            if !plato_marks.is_empty() {
                let words = book.words(location);
                let marked = |w: &Word| {
                    plato_marks
                        .iter()
                        .any(|&(s, e)| w.offset >= s && w.offset <= e)
                };
                let mut i = 0;
                while i < words.len() {
                    if !marked(&words[i]) {
                        i += 1;
                        continue;
                    }
                    let start = i;
                    while i + 1 < words.len() && marked(&words[i + 1]) {
                        i += 1;
                    }
                    for b in crate::selection::line_boxes(&words, start, i) {
                        darken(&mut page, &crate::selection::marker_box(&b));
                    }
                    i += 1;
                }
            }
        } else {
            let page_marks: Vec<&CrengineMark> = deco
                .marks
                .iter()
                .filter_map(|m| match m {
                    Mark::Crengine(cm) if cm.start_page <= location && location <= cm.end_page => {
                        Some(cm)
                    }
                    _ => None,
                })
                .collect();

            if !page_marks.is_empty() {
                let words = book.words(location);
                if !words.is_empty() {
                    let mut marked = vec![false; words.len()];
                    for m in page_marks {
                        let start_idx = if location > m.start_page {
                            0
                        } else {
                            aristotle_render::book::mark_start_word_index(
                                &words,
                                m.start_xpointer.as_deref(),
                            )
                        };
                        let end_idx = if location < m.end_page {
                            words.len().saturating_sub(1)
                        } else {
                            aristotle_render::book::mark_end_word_index(
                                &words,
                                m.end_xpointer.as_deref(),
                            )
                        };
                        if start_idx <= end_idx && end_idx < words.len() {
                            for k in start_idx..=end_idx {
                                marked[k] = true;
                            }
                        }
                    }
                    let mut i = 0;
                    while i < words.len() {
                        if !marked[i] {
                            i += 1;
                            continue;
                        }
                        let start = i;
                        while i + 1 < words.len() && marked[i + 1] {
                            i += 1;
                        }
                        for b in crate::selection::line_boxes(&words, start, i) {
                            darken(&mut page, &crate::selection::marker_box(&b));
                        }
                        i += 1;
                    }
                }
            }
        }
    }
    let is_bookmarked = deco.bookmark_on(location, next).is_some();
    if is_bookmarked || chrome.config().show_bookmark_ribbon {
        ribbon(&mut page, is_bookmarked);
    }
    if has_footnotes {
        footnote_badge(&mut page, chrome.dpi());
    }
    Ok(page)
}

/// Estimated reading time remaining in the whole book, using the measured
/// reading speed (`secs_per_page`).
fn book_remaining_line(
    book: &mut Book,
    secs_per_page: f32,
    location: usize,
    next: Option<usize>,
) -> Option<String> {
    let pages_left = match book.pagination() {
        Some(pages) if !pages.is_empty() => {
            let here = pages.partition_point(|&p| p <= location);
            pages.len().saturating_sub(here) as f32
        }
        _ => {
            let total = book.len();
            if total == 0 || location >= total {
                return None;
            }
            let per_page = next.map_or(2000, |n| n.saturating_sub(location)).max(400) as f32;
            (total.saturating_sub(location)) as f32 / per_page
        }
    };
    let total_mins = (pages_left.max(0.0) * secs_per_page / 60.0).round() as u32;
    if total_mins == 0 {
        return None;
    }
    if total_mins >= 60 {
        let hours = total_mins / 60;
        let minutes = total_mins % 60;
        Some(tf!(
            Msg::BarsBookHoursMinsLeft,
            hours = hours,
            minutes = minutes
        ))
    } else {
        Some(tf!(Msg::BarsBookMinsLeft, minutes = total_mins))
    }
}

/// 1-based `(page_in_chapter, total_pages_in_chapter)` when pagination is known.
fn chapter_page_info(book: &mut Book, location: usize) -> Option<(usize, usize)> {
    let ch = book.chapter_info(location)?;
    let pages = book.pagination()?;
    let start = pages.partition_point(|&p| p < ch.start);
    let end = pages.partition_point(|&p| p < ch.end);
    if end <= start {
        return None;
    }
    let here = pages.partition_point(|&p| p <= location).saturating_sub(1);
    let page_in_ch = here.saturating_sub(start) + 1;
    let total_in_ch = end - start;
    Some((page_in_ch.min(total_in_ch), total_in_ch))
}

/// "本章剩约 12 分钟": the minutes left in the chapter at the measured
/// speed. `None` when the chapter cannot be measured or there is less than
/// a minute left, so the footer leaves the item out.
fn remaining_line(
    book: &mut Book,
    secs_per_page: f32,
    location: usize,
    next: Option<usize>,
) -> Option<String> {
    let ch = book.chapter_info(location)?;
    let pages_left = match book.pagination() {
        Some(pages) => {
            let here = pages.partition_point(|&p| p <= location);
            let end = pages.partition_point(|&p| p < ch.end);
            end.saturating_sub(here.saturating_sub(1)) as f32
        }
        None => {
            // Bytes left over bytes per page, from this page's size.
            let per_page = next.map_or(2000, |n| n.saturating_sub(location)).max(400) as f32;
            (ch.end.saturating_sub(location)) as f32 / per_page
        }
    };
    let minutes = (pages_left.max(0.0) * secs_per_page / 60.0).round() as u32;
    (minutes > 0).then(|| tf!(Msg::ReaderChapterMinutesLeft, minutes = minutes))
}

/// Multiply the pixels of a line of marked words by 0.75: a
/// light gray marker that keeps the text black.
fn darken(page: &mut Page, r: &aristotle_render::book::Bounds) {
    let (w, h) = (page.width as i32, page.height as i32);
    let x0 = r.x0.clamp(0, w);
    let x1 = r.x1.clamp(0, w);
    for y in r.y0.clamp(0, h)..r.y1.clamp(0, h) {
        let row = y as usize * page.stride;
        for p in &mut page.data[row + x0 as usize..row + x1 as usize] {
            *p = (*p as u16 * 3 / 4) as u8;
        }
    }
}

/// The bookmark corner in the top right: the prototype's 48×64 path,
/// `M6 3 H42 V57 L24 41 L6 57 Z`. Bookmarked pages get a solid black
/// ribbon; other pages get the same shape as a white fill with a 2 px
/// black outline, so the corner and its tap target are always visible.
fn ribbon(page: &mut Page, bookmarked: bool) {
    const W: i32 = 48;
    const H: i32 = 64;
    const PATH: [(i32, i32); 5] = [(6, 3), (42, 3), (42, 57), (24, 41), (6, 57)];
    // Stroke width 2: pixels whose center is within 1 px of the path.
    const STROKE_RADIUS_SQ: f32 = 1.0;

    let x0 = page.width as i32 - W;
    for y in 0..H {
        for x in 0..W {
            let px = x0 + x;
            if px < 0 || px >= page.width as i32 || y >= page.height as i32 {
                continue;
            }
            let cx = x as f32 + 0.5;
            let cy = y as f32 + 0.5;
            let inside = inside_polygon(cx, cy, &PATH);
            let ink = bookmarked && inside;
            let stroke = !bookmarked
                && PATH
                    .iter()
                    .zip(PATH.iter().cycle().skip(1))
                    .any(|(&(ax, ay), &(bx, by))| {
                        segment_dist_sq(cx, cy, ax, ay, bx, by) <= STROKE_RADIUS_SQ
                    });
            let dst = y as usize * page.stride + px as usize;
            if ink || stroke {
                page.data[dst] = 0;
            } else if inside && !bookmarked {
                // Cover the running head before outlining, as the prototype's
                // white ribbon fill does.
                page.data[dst] = 0xff;
            }
        }
    }
}

/// Draw the top-right magnifying-glass footnote button onto `page`, centred
/// inside [`crate::turns::footnote_button_at`] so it sits cleanly to the left
/// of the bookmark ribbon without overlapping body text.
fn footnote_badge(page: &mut Page, dpi: u16) {
    let density = aristotle_screen::Density::new(dpi).unwrap_or_default();
    let bm_w = density.mm(10.0);
    let btn_w = density.mm(9.0);
    let radius = density.px(11).max(7) as f32;
    let cx = ((page.width as i32 - bm_w - btn_w / 2).max(radius as i32 + 2)) as f32;
    let cy = density.px(32).max(radius as i32 + 2) as f32;

    let border_half = density.scale(0.65).max(0.55);
    let stroke_half = density.scale(0.95).max(0.75);

    // Magnifying-glass geometry matching `Icon::Search` inside the circular badge.
    let lens_cx = cx - radius * 0.14;
    let lens_cy = cy - radius * 0.14;
    let lens_r = radius * 0.36;
    let diag = std::f32::consts::FRAC_1_SQRT_2;
    let hx0 = lens_cx + lens_r * diag;
    let hy0 = lens_cy + lens_r * diag;
    let hx1 = cx + radius * 0.48;
    let hy1 = cy + radius * 0.48;

    let pad = (radius + border_half + 2.0).ceil() as i32;
    let x0 = (cx as i32 - pad).max(0);
    let x1 = (cx as i32 + pad).min(page.width as i32);
    let y0 = (cy as i32 - pad).max(0);
    let y1 = (cy as i32 + pad).min(page.height as i32);

    for y in y0..y1 {
        let py = y as f32 + 0.5;
        let dy_badge = py - cy;
        let dy_lens = py - lens_cy;
        for x in x0..x1 {
            let px = x as f32 + 0.5;
            let dx_badge = px - cx;
            let dist_badge = (dx_badge * dx_badge + dy_badge * dy_badge).sqrt();
            if dist_badge > radius + border_half {
                continue;
            }
            let dst = y as usize * page.stride + x as usize;
            // Outer button circle border.
            if (dist_badge - radius).abs() <= border_half {
                page.data[dst] = 0x20;
                continue;
            }
            // Magnifying-glass lens ring.
            let dx_lens = px - lens_cx;
            let dist_lens = (dx_lens * dx_lens + dy_lens * dy_lens).sqrt();
            let on_lens = (dist_lens - lens_r).abs() <= stroke_half;
            // Magnifying-glass handle segment.
            let handle_d2 = segment_dist_sq_f32(px, py, hx0, hy0, hx1, hy1);
            let on_handle = handle_d2 <= stroke_half * stroke_half;
            if on_lens || on_handle {
                page.data[dst] = 0x00;
            } else {
                page.data[dst] = 0xff;
            }
        }
    }
}

fn segment_dist_sq_f32(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0)
    };
    let (qx, qy) = (ax + t * dx, ay + t * dy);
    (px - qx) * (px - qx) + (py - qy) * (py - qy)
}

/// Whether (`x`, `y`) is inside the closed polygon `path` (even-odd rule).
fn inside_polygon(x: f32, y: f32, path: &[(i32, i32)]) -> bool {
    let mut inside = false;
    let mut j = path.len() - 1;
    for (i, &(xi, yi)) in path.iter().enumerate() {
        let (xi, yi) = (xi as f32, yi as f32);
        let (xj, yj) = (path[j].0 as f32, path[j].1 as f32);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Squared distance from a point to a segment, for the ribbon outline.
fn segment_dist_sq(px: f32, py: f32, ax: i32, ay: i32, bx: i32, by: i32) -> f32 {
    let (ax, ay, bx, by) = (ax as f32, ay as f32, bx as f32, by as f32);
    let (dx, dy) = (bx - ax, by - ay);
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0)
    };
    let (qx, qy) = (ax + t * dx, ay + t * dy);
    (px - qx) * (px - qx) + (py - qy) * (py - qy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_page() -> Page {
        let (width, height) = (200u32, 200u32);
        Page {
            width,
            height,
            stride: width as usize,
            data: vec![0xff; width as usize * height as usize],
            images: Vec::new(),
        }
    }

    fn pixel(page: &Page, x: i32, y: i32) -> u8 {
        page.data[y as usize * page.stride + x as usize]
    }

    #[test]
    fn empty_bookmark_draws_an_outline() {
        let mut page = white_page();
        ribbon(&mut page, false);
        let x0 = page.width as i32 - 48;
        // Left, top and right edges of the corner are black.
        assert_eq!(pixel(&page, x0 + 6, 20), 0);
        assert_eq!(pixel(&page, x0 + 24, 3), 0);
        assert_eq!(pixel(&page, x0 + 42, 20), 0);
        // The middle stays white, and so does the page beyond the corner.
        assert_eq!(pixel(&page, x0 + 24, 20), 0xff);
        assert_eq!(pixel(&page, x0 + 2, 20), 0xff);
        assert_eq!(pixel(&page, x0 + 24, 70), 0xff);
    }

    #[test]
    fn bookmarked_draws_a_solid_ribbon() {
        let mut page = white_page();
        ribbon(&mut page, true);
        let x0 = page.width as i32 - 48;
        assert_eq!(pixel(&page, x0 + 24, 20), 0);
        assert_eq!(pixel(&page, x0 + 24, 3), 0);
        assert_eq!(pixel(&page, x0 + 2, 20), 0xff);
    }

    #[test]
    fn footnote_badge_draws_magnifying_glass_without_touching_ribbon() {
        let (width, height) = (1072u32, 1448u32);
        let mut page = Page {
            width,
            height,
            stride: width as usize,
            data: vec![0xff; width as usize * height as usize],
            images: Vec::new(),
        };
        footnote_badge(&mut page, 300);
        let btn = crate::turns::footnote_button_at(
            aristotle_ui::geom::Rect::new(0, 0, width as i32, height as i32),
            aristotle_ui::geom::Density::default(),
        );
        let mut dark_in_btn = 0usize;
        for y in btn.y..btn.bottom() {
            for x in btn.x..btn.right() {
                if pixel(&page, x, y) < 0x80 {
                    dark_in_btn += 1;
                }
            }
        }
        assert!(
            dark_in_btn > 40,
            "magnifying-glass badge should draw crisp ink inside button rect, got {dark_in_btn}"
        );
        // Bookmark ribbon area at the far right edge remains untouched white.
        let bm = crate::turns::bookmark_corner_at(
            aristotle_ui::geom::Rect::new(0, 0, width as i32, height as i32),
            aristotle_ui::geom::Density::default(),
        );
        for y in bm.y..bm.bottom() {
            for x in bm.x..bm.right() {
                assert_eq!(pixel(&page, x, y), 0xff);
            }
        }
    }
    fn txt_source_for(path: std::path::PathBuf, saved: Progress) -> Source {
        Source {
            id: "txt".into(),
            title: "Synthetic TXT".into(),
            path,
            stamp: (1, 2),
            data_dir: std::path::PathBuf::from("."),
            mobi_cache: std::path::PathBuf::from("."),
            saved: Some(saved),
            marks: vec![(
                Some(0),
                Position::XPointer {
                    xpointer: "/body/DocFragment[1]/body/p[2]/text().0".into(),
                },
                Position::XPointer {
                    xpointer: "/body/DocFragment[1]/body/p[4]/text().0".into(),
                },
            )],
            mark_versions: vec![None],
            bookmarks: vec![Position::XPointer {
                xpointer: "/body/DocFragment[1]/body/p[1]".into(),
            }],
            bookmark_versions: vec![None],
            secs_per_page: 30.0,
            exact: None,
            jump: None,
            format: aristotle_library::Format::Txt,
            txt_version: Some(CONVERTER_VERSION),
            txt_mapped: 0,
            txt_unmapped: Vec::new(),
            txt_legacy: None,
            paginate: false,
            cancel: None,
            progress: None,
            night: false,
            underline: Underline::Off,
        }
    }

    #[test]
    fn future_txt_progress_uses_fraction_without_reinterpreting_its_pointer() {
        let dir = crate::testdir("txt-future-progress");
        let path = dir.join("book.txt");
        std::fs::write(&path, "第一章 开始\n正文").unwrap();
        let mut saved = progress::progress_at(
            &Position::XPointer {
                xpointer: "/body/DocFragment[1]/body/p/text().0".into(),
            },
            0.5,
            1,
        );
        saved.source = Some(txt::source_for(CONVERTER_VERSION + 1));
        let mut source = txt_source_for(path, saved);
        source.marks.clear();
        source.mark_versions.clear();
        source.bookmarks.clear();
        source.bookmark_versions.clear();
        migrate_txt_source(&mut source);
        assert_eq!(source.txt_mapped, 0);
        assert_eq!(source.saved.unwrap().locator, fallback_locator(0.5));
        assert_eq!(source.txt_unmapped.len(), 1);
    }

    #[test]
    fn the_open_worker_translates_legacy_progress_and_marks() {
        let dir = crate::testdir("txt-migrate");
        let src = dir.join("book.txt");
        std::fs::write(
            &src,
            "第一章 开始\n一段。\n二段。\nChapter 5 New\n三段。\n第二章 继续\n四段。\n五段。",
        )
        .unwrap();
        let mut source = txt_source_for(
            src,
            Progress {
                fraction: 0.4,
                locator: aristotle_library::Locator::XPointer {
                    xpointer: "/body/DocFragment[1]/body/p[3]/text().0".into(),
                },
                updated: 1,
                source: Some("aristotle".into()),
                page: None,
            },
        );
        migrate_txt_source(&mut source);
        assert_eq!(source.txt_mapped, 3);
        assert!(source.txt_unmapped.is_empty());
        assert_eq!(
            source.saved.as_ref().unwrap().locator,
            aristotle_library::Locator::XPointer {
                xpointer: "/body/DocFragment[2]/body/h2[1]/text().0".into()
            }
        );
        assert_eq!(
            source.saved.as_ref().unwrap().source.as_deref(),
            Some("aristotle/txt2")
        );
        assert_eq!(
            source.marks[0].1,
            Position::XPointer {
                xpointer: "/body/DocFragment[1]/body/p[2]/text().0".into()
            }
        );
        assert_eq!(
            source.marks[0].2,
            Position::XPointer {
                xpointer: "/body/DocFragment[2]/body/p[1]/text().0".into()
            }
        );
        assert_eq!(source.mark_versions, vec![Some(CONVERTER_VERSION)]);
        assert_eq!(
            source.bookmarks[0],
            Position::XPointer {
                xpointer: "/body/DocFragment[1]/body/p[1]".into()
            }
        );
        assert_eq!(source.bookmark_versions, vec![Some(CONVERTER_VERSION)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unmappable_legacy_mark_is_kept_out_of_this_open() {
        let dir = crate::testdir("txt-migrate-bad");
        let src = dir.join("book.txt");
        std::fs::write(&src, "第一章 开始\n一段。\n二段。").unwrap();
        let mut source = txt_source_for(
            src,
            Progress {
                fraction: 0.4,
                locator: aristotle_library::Locator::XPointer {
                    xpointer: "/body/DocFragment[9]/body/p[1]".into(),
                },
                updated: 1,
                source: Some("aristotle".into()),
                page: None,
            },
        );
        migrate_txt_source(&mut source);
        assert_eq!(
            source.saved.as_ref().unwrap().locator,
            aristotle_library::Locator::Page { index: 400_000 }
        );
        assert!(source.marks.is_empty() && source.mark_versions.is_empty());
        // The bookmark at p[1] still maps exactly and is kept.
        assert_eq!(source.bookmarks.len(), 1);
        assert_eq!(source.txt_unmapped.len(), 2, "{:?}", source.txt_unmapped);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Use the stored pagination for `book` and `style`, or start one in the
/// background (with `paginate_with`, the MOBI cache directory). Returns
/// whether it was cached, and the pagination thread.
fn start_pagination(
    book: &mut Book,
    style: &Style,
    data_dir: &Path,
    id: &str,
    stamp: (u64, i64),
    path: &Path,
    paginate_with: Option<&Path>,
) -> (bool, Option<Paginator>) {
    if book.engine() == Engine::Vertical {
        // The vertical engine finds its pages by laying them out, with no
        // rendering and no cache: do it now, so the footer can show
        // "x / y" from the first page on.
        book.paginate();
        return (true, None);
    }
    if book.engine() != Engine::Plato {
        return (true, None);
    }
    let file = pagecache::cache_path(data_dir, id, stamp.0, stamp.1, style);
    if let Some(pages) = pagecache::load(&file) {
        book.set_pagination(pages);
        return (true, None);
    }
    let Some(mobi_cache) = paginate_with else {
        return (false, None);
    };
    let paginator = Paginator::start(
        path.to_path_buf(),
        style.clone(),
        mobi_cache.to_path_buf(),
        file,
    );
    (false, Some(paginator))
}
