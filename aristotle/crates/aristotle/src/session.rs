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
use aristotle_render::book::{Book, CrengineMark, Engine, Link, Mark, Position, Word};
use aristotle_render::page::{Chrome, Page};
use aristotle_render::plato_core::anyhow::{format_err, Context, Error};
use aristotle_render::style::Style;

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
    /// Whether to paginate the whole book in the background when its page
    /// numbers are not cached (not for snapshots made ahead of time).
    pub paginate: bool,
    /// Set by whoever stops waiting for this open; [`Session::prepare`]
    /// gives up between its steps.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Counter updated with formatting progress (0..=100) on engines that report it.
    pub progress: Option<Arc<AtomicU8>>,
    pub night: bool,
}

/// Held by every thread that opens a book in the background (an open
/// worker, a snapshot made ahead of time) from the start of
/// [`Session::prepare`] until its result is handed over or dropped, so an
/// abandoned open never holds a laid-out book next to a new one.
pub static OPEN_GATE: Mutex<()> = Mutex::new(());

impl Source {
    pub fn new(lib: &Library, rec: &BookRecord, mobi_cache: &Path) -> Result<Source, Error> {
        let path = lib
            .absolute_path(rec)
            .ok_or_else(|| format_err!("the library has no root"))?;
        // The file itself, in case it changed since the last scan.
        let stamp = crate::snapshot::file_stamp(&path).unwrap_or((rec.file.size, rec.file.mtime));
        Ok(Source {
            id: rec.id.clone(),
            title: rec.meta.title.clone(),
            path,
            stamp,
            data_dir: lib.data_dir().to_path_buf(),
            mobi_cache: mobi_cache.to_path_buf(),
            saved: rec.progress.clone(),
            marks: crate::marks::ours(rec, AnnotationKind::Highlight)
                .into_iter()
                .map(|(i, s, e)| (Some(i), s, e))
                .collect(),
            bookmarks: crate::marks::ours(rec, AnnotationKind::Bookmark)
                .into_iter()
                .map(|(_, s, _)| s)
                .collect(),
            secs_per_page: crate::speed::DEFAULT_SECS_PER_PAGE,
            exact: None,
            jump: None,
            format: rec.format,
            paginate: true,
            cancel: None,
            progress: None,
            night: false,
        })
    }
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
    pub fn prepare(src: Source, style: Style, neighbours: bool) -> Result<Prepared, Error> {
        let cancelled = || {
            if src
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                Err(format_err!("cancelled"))
            } else {
                Ok(())
            }
        };
        cancelled()?;
        let t = Instant::now();
        let mut book = Book::open_with_progress(
            &src.path,
            &style,
            Some(&src.mobi_cache),
            src.progress.as_ref(),
        )
        .with_context(|| format!("can't open {}", src.path.display()))?;
        book.set_night(src.night);
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
        let target = src
            .exact
            .or_else(|| {
                let annotation = src.jump.as_ref()?;
                let position =
                    crate::marks::target(annotation, src.format, book.mobi().map(|m| &m.info))?;
                book.location_of(&position)
            })
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
        Ok(Session {
            id: p.src.id,
            title: p.src.title,
            stamp: p.src.stamp,
            style: p.style,
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
                source: Some("aristotle".into()),
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
        progress::progress_at(&pos, fraction, now)
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

    pub fn set_speed(&mut self, secs_per_page: f32) {
        self.deco.secs_per_page = secs_per_page;
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
    ribbon(&mut page, deco.bookmark_on(location, next).is_some());
    Ok(page)
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
