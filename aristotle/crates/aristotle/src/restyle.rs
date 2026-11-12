// SPDX-License-Identifier: AGPL-3.0-or-later
//! Laying the book out again after a change of typography, without freezing
//! the reader (ADR-015, APP-007).
//!
//! Changing font size, line height or margins means laying the book out again.
//! On crengine books with partial rerendering support:
//! 1. Changes are debounced (600 ms).
//! 2. Fast anchor-only relayout (~1 s on device) formats only the current DocFragment.
//! 3. The book enters degraded state: page turns format subsequent fragments on
//!    demand; footers, TOC, and reading progress suppress inaccurate page counts.
//! 4. After 2 seconds of reader inactivity ([`aristotle_render::partial::IDLE`]),
//!    background catchup formats the remainder of the book.
//! 5. Catchup completion exits degraded state, re-anchors to current XPointer,
//!    clears page cache, and writes authoritative progress.
//!
//! For Plato books or single-fragment EPUBs, the layout falls back to full
//! background relayout with progress bar.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aristotle_library::index::AnnotationKind;
use aristotle_library::Progress;
use aristotle_render::book::{Book, Position};
use aristotle_render::page::{Chrome, Page};
use aristotle_render::partial::{CatchupOutcome, Outcome, PartialScheduler, StartAction};
use aristotle_render::plato_core::anyhow::{format_err, Context, Error};
use aristotle_render::style::Style;
use aristotle_ui::screens::reader::Settings;
use aristotle_ui::tree::Refresh;

use crate::session::{render_page, Deco, Prepared, Session, Source, OPEN_GATE};
use crate::turns::Turn;
use crate::{fit_menu, ms, perf, reader_tree, update_menu_labels, App, Content, PageView, Screen};

/// Result of partial anchor relayout.
pub struct PartialReady {
    pub book: Book,
    pub page: Page,
    pub current: usize,
    pub style: Style,
    pub elapsed: Duration,
}

/// Background worker for partial anchor relayout.
pub struct PartialWorker {
    rx: Receiver<Result<PartialReady, (Option<Book>, Error)>>,
    #[allow(dead_code)]
    pub started: Instant,
    done: Arc<AtomicBool>,
}

impl PartialWorker {
    pub fn poll(&self) -> Option<Result<PartialReady, (Option<Book>, Error)>> {
        if !self.done.load(Ordering::SeqCst) {
            return None;
        }
        self.rx.try_recv().ok()
    }
}

/// Spawn partial anchor relayout on a background thread.
pub fn spawn_partial(
    book: Book,
    style: Style,
    anchor: Progress,
    marks: Vec<(Option<usize>, Position, Position)>,
    bookmarks: Vec<Position>,
    secs_per_page: f32,
) -> PartialWorker {
    let (tx, rx) = crate::wake::channel();
    let done = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let work = {
        let done = done.clone();
        move || {
            let t = Instant::now();
            let _gate = OPEN_GATE.lock().unwrap_or_else(|e| e.into_inner());
            let mut book_slot = Some(book);
            let result = (|| -> Result<PartialReady, Error> {
                let book = book_slot.as_mut().unwrap();
                book.enable_partial_rerender(true)?;
                book.set_layout(&style)?;
                let target = crate::progress::positions(
                    book.engine(),
                    Some(&anchor),
                    crate::progress::stock_positions_compatible(book),
                )
                .iter()
                // Placed in the new size: the old page numbers stand until
                // the anchor's fragment is laid out again.
                .find_map(|p| book.settled_location_of(p))
                .unwrap_or(0);
                let current = book
                    .page_start(target)
                    .ok_or_else(|| format_err!("the book has no pages"))?;
                let mut chrome = Chrome::load(&style).context("can't load chrome fonts")?;
                let deco = Deco {
                    marks: marks
                        .iter()
                        .filter_map(|(id, s, e)| book.resolve_mark(*id, s, e))
                        .collect(),
                    bookmarks: bookmarks
                        .iter()
                        .filter_map(|p| book.location_of(p))
                        .collect(),
                    secs_per_page,
                };
                let page = render_page(book, &mut chrome, &deco, current, None, true)?;
                let book = book_slot.take().unwrap();
                Ok(PartialReady {
                    book,
                    page,
                    current,
                    style,
                    elapsed: t.elapsed(),
                })
            })();
            done.store(true, Ordering::SeqCst);
            match result {
                Ok(ready) => {
                    let _ = tx.send(Ok(ready));
                }
                Err(e) => {
                    let _ = tx.send(Err((book_slot.take(), e)));
                }
            }
        }
    };
    if let Err(e) = thread::Builder::new()
        .name("partial-restyle".into())
        .spawn(work)
    {
        eprintln!("aristotle: can't start partial restyle thread: {e}");
        done.store(true, Ordering::SeqCst);
    }
    PartialWorker { rx, started, done }
}

/// Result of background catchup render.
pub struct CatchupReady {
    pub book: Book,
    pub anchor: Progress,
    pub elapsed: Duration,
}

/// Background worker for catchup render.
pub struct CatchupWorker {
    rx: Receiver<Result<CatchupReady, (Option<Book>, Error)>>,
    #[allow(dead_code)]
    pub started: Instant,
    pub progress: Arc<AtomicU8>,
    done: Arc<AtomicBool>,
}

impl CatchupWorker {
    pub fn poll(&self) -> Option<Result<CatchupReady, (Option<Book>, Error)>> {
        if !self.done.load(Ordering::SeqCst) {
            return None;
        }
        self.rx.try_recv().ok()
    }
}

/// Spawn catchup render on a background thread.
pub fn spawn_catchup(mut book: Book, anchor: Progress) -> CatchupWorker {
    let (tx, rx) = crate::wake::channel();
    let done = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU8::new(0));
    let started = Instant::now();
    let work = {
        let (done, progress) = (done.clone(), progress.clone());
        move || {
            let t = Instant::now();
            let _gate = OPEN_GATE.lock().unwrap_or_else(|e| e.into_inner());
            let res = book.catchup_render_with_progress(Some(&progress));
            done.store(true, Ordering::SeqCst);
            match res {
                Ok(()) => {
                    let _ = tx.send(Ok(CatchupReady {
                        book,
                        anchor,
                        elapsed: t.elapsed(),
                    }));
                }
                Err(e) => {
                    let _ = tx.send(Err((Some(book), e)));
                }
            }
        }
    };
    if let Err(e) = thread::Builder::new()
        .name("catchup-restyle".into())
        .spawn(work)
    {
        eprintln!("aristotle: can't start catchup thread: {e}");
        done.store(true, Ordering::SeqCst);
    }
    CatchupWorker {
        rx,
        started,
        progress,
        done,
    }
}

/// Active background relayout or catchup worker.
pub struct RestyleWorker {
    pub kind: RestyleWorkerKind,
    pub progress: Arc<AtomicU8>,
}

pub enum RestyleWorkerKind {
    Full(crate::open::Worker),
    Partial(PartialWorker),
    Catchup(CatchupWorker),
}

/// The book being laid out again, and the policy around it.
pub struct Restyler {
    /// When to lay the book out again, and what for.
    scheduler: PartialScheduler<Style, Progress>,
    /// The layout running on a worker thread.
    worker: Option<RestyleWorker>,
    /// The text the layout is asked at: the page on the panel when the
    /// reader asked for the change.
    anchor: Option<Progress>,
    /// When the reader asked for the change.
    asked_at: Option<Instant>,
    /// When the layout started.
    started_at: Option<Instant>,
    /// The typography the page on the panel was laid out with.
    applied: Settings,
    /// A page turn that waits for the layout to finish.
    queued: Option<(Turn, u64)>,
    /// Whether the busy mark is on the panel.
    mark: bool,
    /// Whether the layout is slow enough to show the busy mark.
    slow: bool,
    /// The text on the panel when the catch-up started. Page numbers
    /// before the current fragment change with the catch-up, so the page
    /// is found again by this position, not by its old number.
    catchup_at: Option<Position>,
}

impl Default for Restyler {
    fn default() -> Self {
        Self {
            scheduler: PartialScheduler::new(),
            worker: None,
            anchor: None,
            asked_at: None,
            started_at: None,
            applied: Settings::default(),
            queued: None,
            mark: false,
            slow: false,
            catchup_at: None,
        }
    }
}

impl Restyler {
    /// The text the layout is asked at, if it is known.
    pub fn anchor(&self) -> Option<&Progress> {
        self.anchor.as_ref()
    }

    /// The worker currently laying the book out, if one is running.
    pub fn worker(&self) -> Option<&RestyleWorker> {
        self.worker.as_ref()
    }

    /// Ask for the book to be laid out again in `style`, at `anchor`.
    pub fn request(&mut self, style: Style, anchor: Progress, can_partial: bool, now: Instant) {
        self.scheduler
            .request(style, anchor.clone(), can_partial, now);
        self.asked_at = Some(now);
        if self.scheduler.busy() {
            if self.anchor.is_none() {
                self.anchor = Some(anchor);
            }
        } else {
            self.anchor = None;
        }
    }

    /// When the layout will start, if one is waiting for debounce or idle timeout.
    pub fn deadline(&self) -> Option<Instant> {
        self.scheduler.deadline()
    }

    /// Whether a change is waiting or a layout is running.
    pub fn busy(&self) -> bool {
        self.scheduler.busy()
    }

    /// Whether the book is being laid out or caught up right now.
    pub fn running(&self) -> bool {
        self.scheduler.running()
    }

    /// Whether the session's book is with a partial layout or a catch-up,
    /// which only run for books that can be partially laid out again.
    pub fn lent_to_partial_worker(&self) -> bool {
        matches!(
            self.worker.as_ref().map(|w| &w.kind),
            Some(RestyleWorkerKind::Partial(_) | RestyleWorkerKind::Catchup(_))
        )
    }

    /// Reset idle timeout on reader action.
    pub fn user_action(&mut self, now: Instant) {
        self.scheduler.user_action(now);
    }

    /// A page turn that has to wait for the layout.
    pub fn queue_turn(&mut self, dir: Turn, at_us: u64) {
        self.queued = Some((dir, at_us));
        self.scheduler.queue_turn(at_us);
    }

    /// The page turn waiting for the layout, if there is one.
    pub fn take_turn(&mut self) -> Option<(Turn, u64)> {
        self.scheduler.take_turns();
        self.queued.take()
    }

    /// A new book: forget the layout in flight and the change waiting.
    pub fn reset(&mut self, applied: Settings) {
        self.scheduler = PartialScheduler::new();
        self.worker = None;
        self.anchor = None;
        self.asked_at = None;
        self.started_at = None;
        self.applied = applied;
        self.queued = None;
        self.mark = false;
        self.slow = false;
        self.catchup_at = None;
    }

    /// The typography the book is laid out with now: a change back to it
    /// needs no layout.
    pub fn set_applied(&mut self, style: &Style) {
        self.scheduler.set_applied(style.clone());
    }

    /// Whether the layout is slow enough to show the busy mark.
    pub fn slow(&self) -> bool {
        self.slow
    }

    /// The layout is slow enough to show the busy mark.
    pub fn set_slow(&mut self, slow: bool) {
        self.slow = slow;
    }

    /// Whether the busy mark is on the panel.
    pub fn mark(&self) -> bool {
        self.mark
    }

    /// The busy mark goes up or comes down.
    pub fn set_mark(&mut self, mark: bool) {
        self.mark = mark;
    }

    /// The typography the page on the panel was laid out with.
    pub fn panel_typography(&self) -> Settings {
        self.applied
    }

    #[allow(dead_code)]
    pub fn scheduler(&self) -> &PartialScheduler<Style, Progress> {
        &self.scheduler
    }

    pub fn scheduler_mut(&mut self) -> &mut PartialScheduler<Style, Progress> {
        &mut self.scheduler
    }
}

impl App {
    /// New typography from the menu: ask for the book to be laid out again
    /// at the same place.
    pub(crate) fn request_restyle(&mut self, settings: Settings) -> Result<(), Error> {
        let mut new_prefs = self.prefs.clone();
        new_prefs.apply(&settings);
        let new_style = new_prefs
            .style()
            .with_screen(self.ui.profile(), self.panel.stride());
        let (anchor, slow, can_partial) = {
            let Screen::Reader(r) = &mut self.screen else {
                return Ok(());
            };
            if settings == r.settings && new_style == r.session.style {
                return Ok(());
            }
            r.settings = settings;
            update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
            self.ui.tree.invalidate(r.menu.size_steps, Refresh::Auto);
            let anchor = match self.restyler.anchor() {
                Some(anchor) => anchor.clone(),
                None => r.session.progress(aristotle_library::library::now()),
            };
            // The book may be lent to a worker: a partial layout or a catch-up
            // only runs for a book that can be partially laid out again.
            let can_partial = match r.session.book_ref() {
                Some(book) => book.can_partial_rerender(),
                None => self.restyler.lent_to_partial_worker(),
            };
            (anchor, r.session.slow_relayout(), can_partial)
        };
        self.prefs.apply(&settings);
        if let Err(e) = self.prefs.save(self.lib.data_dir()) {
            eprintln!("aristotle: can't save preferences: {e}");
        }
        let style = self.page_style();
        self.restyler.set_slow(slow);
        self.restyler
            .request(style, anchor, can_partial, Instant::now());
        self.sync_busy_mark()?;
        Ok(())
    }

    /// The busy mark shows while the book is laid out again.
    fn sync_busy_mark(&mut self) -> Result<(), Error> {
        let wanted = self.restyler.busy() && self.restyler.slow();
        if wanted == self.restyler.mark() {
            return Ok(());
        }
        self.restyler.set_mark(wanted);
        let node = match &self.screen {
            Screen::Reader(r) => r.menu.busy,
            _ => return Ok(()),
        };
        self.ui.tree.set_hidden(node, !wanted);
        self.render_ui()?;
        Ok(())
    }

    /// The layout: start it when debounce or idle has passed, take it over
    /// when worker comes back.
    pub(crate) fn poll_restyle(&mut self) -> Result<(), Error> {
        self.poll_restyle_worker()?;
        self.start_restyle_work()?;
        // Busy mark follows
        self.sync_busy_mark()?;
        Ok(())
    }

    /// Wait for the worker that holds the session's book to hand it back.
    ///
    /// A partial layout and a catch-up lay the reader's own book out, so
    /// the book is away from the session while they run. Anything other
    /// than a page turn (which queues) needs the book: it waits here for
    /// the worker, which is at most one engine call (about 3.5 s for a
    /// catch-up on the Kindle), instead of finding the book gone.
    pub(crate) fn settle_restyle(&mut self) -> Result<(), Error> {
        let lent = matches!(&self.screen, Screen::Reader(r) if !r.session.has_book());
        if !lent || self.restyler.worker.is_none() {
            return Ok(());
        }
        let t = Instant::now();
        while self.restyler.worker.is_some() {
            self.poll_restyle_worker()?;
            if self.restyler.worker.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.sync_busy_mark()?;
        perf!(
            "restyle: input waited {:.0} ms for the worker to hand the book back",
            ms(t.elapsed())
        );
        Ok(())
    }

    /// Take over the worker's result if it has finished.
    fn poll_restyle_worker(&mut self) -> Result<(), Error> {
        if let Some(worker) = self.restyler.worker.as_ref() {
            match &worker.kind {
                RestyleWorkerKind::Full(w) => {
                    if let Some(result) = w.poll() {
                        self.restyler.worker = None;
                        let outcome = self.restyler.scheduler_mut().finish_full();
                        match (outcome, result) {
                            (Outcome::Ready, Ok(prepared)) => self.adopt_restyle(prepared)?,
                            (Outcome::Ready, Err(e)) => {
                                eprintln!("aristotle: can't lay the book out again: {e:#}");
                                self.revert_restyle()?;
                            }
                            (Outcome::Discarded, Ok(prepared)) => {
                                let times = prepared.times;
                                perf!(
                                    "restyle: full layout discarded ({:.0} ms)",
                                    ms(times.open + times.place + times.render)
                                );
                            }
                            (Outcome::Discarded, Err(e)) => {
                                eprintln!("aristotle: can't lay the book out again: {e:#}");
                            }
                        }
                    }
                }
                RestyleWorkerKind::Partial(w) => {
                    if let Some(result) = w.poll() {
                        self.restyler.worker = None;
                        let outcome = self.restyler.scheduler_mut().finish_partial(Instant::now());
                        match result {
                            Ok(ready) => {
                                if outcome == Outcome::Ready {
                                    self.adopt_partial_restyle(ready)?;
                                } else if let Screen::Reader(r) = &mut self.screen {
                                    r.session.restore_book(ready.book);
                                }
                            }
                            Err((book, e)) => {
                                if let Some(book) = book {
                                    if let Screen::Reader(r) = &mut self.screen {
                                        r.session.restore_book(book);
                                    }
                                }
                                eprintln!("aristotle: partial restyle error: {e:#}");
                                self.revert_restyle()?;
                            }
                        }
                    }
                }
                RestyleWorkerKind::Catchup(w) => {
                    if let Some(result) = w.poll() {
                        self.restyler.worker = None;
                        let outcome = self.restyler.scheduler_mut().finish_catchup();
                        match result {
                            Ok(ready) => {
                                if outcome == CatchupOutcome::Ready {
                                    self.adopt_catchup_restyle(ready)?;
                                } else if let Screen::Reader(r) = &mut self.screen {
                                    r.session.restore_book(ready.book);
                                }
                            }
                            Err((book, e)) => {
                                if let Some(book) = book {
                                    if let Screen::Reader(r) = &mut self.screen {
                                        r.session.restore_book(book);
                                    }
                                }
                                eprintln!("aristotle: catchup render error: {e:#}");
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Start the next layout or catch-up when debounce or idle has passed.
    fn start_restyle_work(&mut self) -> Result<(), Error> {
        if self.restyler.worker.is_none() {
            let now = Instant::now();
            if let Some(action) = self.restyler.scheduler_mut().poll(now) {
                match action {
                    StartAction::StartPartial {
                        key: style,
                        payload: anchor,
                    } => {
                        self.start_partial_restyle(style, anchor)?;
                    }
                    StartAction::StartFull {
                        key: style,
                        payload: anchor,
                    } => {
                        self.start_full_restyle(style, anchor)?;
                    }
                    StartAction::StartCatchup => {
                        self.start_catchup()?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Lay the book out partially in `style`, formatting only the anchor page.
    fn start_partial_restyle(&mut self, style: Style, anchor: Progress) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let Some(book) = r.session.take_book() else {
            return Ok(());
        };
        let id = r.session.id.clone();
        let rec = self.lib.book(&id).cloned();
        let marks = rec
            .as_ref()
            .map(|rec| {
                crate::marks::ours(rec, AnnotationKind::Highlight)
                    .into_iter()
                    .map(|(i, s, e)| (Some(i), s, e))
                    .collect()
            })
            .unwrap_or_default();
        let bookmarks = rec
            .as_ref()
            .map(|rec| {
                crate::marks::ours(rec, AnnotationKind::Bookmark)
                    .into_iter()
                    .map(|(_, s, _)| s)
                    .collect()
            })
            .unwrap_or_default();
        let secs_per_page = self.speed.secs_per_page();
        let worker = spawn_partial(book, style, anchor, marks, bookmarks, secs_per_page);
        let progress = Arc::new(AtomicU8::new(0));
        self.restyler.worker = Some(RestyleWorker {
            kind: RestyleWorkerKind::Partial(worker),
            progress,
        });
        self.restyler.started_at = Some(Instant::now());
        Ok(())
    }

    /// Lay the book out fully in `style` on a worker thread (fallback path).
    fn start_full_restyle(&mut self, style: Style, anchor: Progress) -> Result<(), Error> {
        let Screen::Reader(r) = &self.screen else {
            return Ok(());
        };
        let id = r.session.id.clone();
        let Some(rec) = self.lib.book(&id).cloned() else {
            return Ok(());
        };
        let mut src = Source::new(&self.lib, &rec, &self.mobi_cache)?;
        src.saved = Some(anchor);
        src.night = self.prefs.night;
        let worker = crate::open::spawn_with(src, style, false);
        let progress = worker.progress.clone();
        self.restyler.worker = Some(RestyleWorker {
            kind: RestyleWorkerKind::Full(worker),
            progress,
        });
        self.restyler.started_at = Some(Instant::now());
        Ok(())
    }

    /// Start background catchup render for a degraded book.
    fn start_catchup(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        // Where the reader is, asked while the book is still here.
        let anchor = r.session.progress(aristotle_library::library::now());
        let here = r.session.position();
        let Some(book) = r.session.take_book() else {
            return Ok(());
        };
        self.restyler.catchup_at = Some(here);
        let worker = spawn_catchup(book, anchor);
        let progress = worker.progress.clone();
        self.restyler.worker = Some(RestyleWorker {
            kind: RestyleWorkerKind::Catchup(worker),
            progress,
        });
        self.restyler.started_at = Some(Instant::now());
        Ok(())
    }

    /// Adopt finished partial relayout: enter degraded mode and show anchor page.
    fn adopt_partial_restyle(&mut self, ready: PartialReady) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.session.restore_book(ready.book);
        r.session.degraded = true;
        r.session.style = ready.style.clone();
        r.session.clear_pages();
        r.session.recompute_deco();
        r.session.current = ready.current;
        let page = Rc::new(ready.page);
        r.session.keep_page(ready.current, page.clone());
        let settings = r.settings;
        let (tree, menu, page_node, nav, progress) =
            reader_tree(&self.styles, &r.session.title, &settings, page.clone());
        r.menu = menu;
        r.page_node = page_node;
        r.nav = nav;
        r.progress = progress;
        self.ui.set_tree_with(tree, Refresh::Auto);
        fit_menu(
            &mut self.ui,
            &self.styles,
            &r.session.title,
            &settings,
            &page,
            &mut r.menu,
            &mut r.page_node,
            &mut r.nav,
            &mut r.progress,
        );
        update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
        if !r.menu_open {
            self.ui.tree.set_hidden(r.menu.root, true);
            self.ui.tree.set_hidden(r.nav, true);
        }
        self.render_ui_with(Content::Page)?;
        if let Screen::Reader(r) = &mut self.screen {
            r.session.prerender();
        }
        self.restyler.anchor = None;
        self.restyler.applied = settings;
        self.restyler.asked_at = None;
        self.restyler.started_at = None;
        self.restyler.set_applied(&ready.style);
        self.restyler.set_mark(false);
        perf!(
            "restyle (partial): anchor ready in {:.0} ms",
            ms(ready.elapsed)
        );
        if let Some((dir, at_us)) = self.restyler.take_turn() {
            self.turn(dir, at_us)?;
        }
        Ok(())
    }

    /// Adopt finished background catchup: exit degraded mode and re-anchor.
    fn adopt_catchup_restyle(&mut self, ready: CatchupReady) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.session.restore_book(ready.book);
        r.session.degraded = false;
        r.session.clear_pages();
        r.session.recompute_deco();
        let here = match self.restyler.catchup_at.take() {
            Some(here) => here,
            None => r.session.position(),
        };
        let at = r.session.page_of(&here).unwrap_or(r.session.current);
        r.session.current = at;
        let page = r.session.page(at)?;
        if let Some(w) = self.ui.tree.widget_mut::<PageView>(r.page_node) {
            w.page = page.clone();
        }
        self.ui.tree.invalidate(r.page_node, Refresh::Auto);
        update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
        self.render_ui_with(Content::Page)?;
        if let Screen::Reader(r) = &mut self.screen {
            r.session.prerender();
        }
        let now = aristotle_library::library::now();
        if let Screen::Reader(r) = &self.screen {
            self.lib
                .set_progress(&r.session.id, r.session.progress(now));
        }
        self.debounce.changed(Instant::now());
        self.page_changed();
        self.restyler.set_mark(false);
        perf!(
            "restyle (catchup): completed in {:.0} ms for locator {:?}; re-anchored to location {}",
            ms(ready.elapsed),
            ready.anchor.locator,
            at
        );
        if let Some((dir, at_us)) = self.restyler.take_turn() {
            self.turn(dir, at_us)?;
        }
        Ok(())
    }

    /// The layout is ready (fallback full path): take it over.
    fn adopt_restyle(&mut self, prepared: Prepared) -> Result<(), Error> {
        let times = prepared.times;
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let here = r.session.position();
        let mut session = Session::from_prepared(prepared)?;
        session.set_speed(self.speed.secs_per_page());
        let at = session.page_of(&here).unwrap_or(session.current);
        session.current = at;
        let page = session.page(at)?;
        let settings = r.settings;
        r.session = session;
        let new_style = r.session.style.clone();
        let (tree, menu, page_node, nav, progress) =
            reader_tree(&self.styles, &r.session.title, &settings, page.clone());
        r.menu = menu;
        r.page_node = page_node;
        r.nav = nav;
        r.progress = progress;
        self.ui.set_tree_with(tree, Refresh::Auto);
        fit_menu(
            &mut self.ui,
            &self.styles,
            &r.session.title,
            &settings,
            &page,
            &mut r.menu,
            &mut r.page_node,
            &mut r.nav,
            &mut r.progress,
        );
        update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
        if !r.menu_open {
            self.ui.tree.set_hidden(r.menu.root, true);
            self.ui.tree.set_hidden(r.nav, true);
        }
        let (family, size, line_height, margin) = (
            r.session.style.font_family.clone(),
            r.session.style.font_size,
            r.session.style.line_height,
            r.session.style.margin_mm,
        );
        let asked_at = self.restyler.asked_at;
        let started_at = self.restyler.started_at;
        let merged = self.restyler.scheduler.merged_count();
        let discarded = self.restyler.scheduler.discarded_count();
        let t_panel = Instant::now();
        self.render_ui_with(Content::Page)?;
        let panel = t_panel.elapsed();
        if let Screen::Reader(r) = &mut self.screen {
            r.session.prerender();
        }
        let now = aristotle_library::library::now();
        if let Screen::Reader(r) = &self.screen {
            self.lib
                .set_progress(&r.session.id, r.session.progress(now));
        }
        self.debounce.changed(Instant::now());
        self.page_changed();
        self.restyler.anchor = None;
        self.restyler.applied = settings;
        self.restyler.asked_at = None;
        self.restyler.started_at = None;
        self.restyler.set_applied(&new_style);
        self.restyler.set_mark(false);
        let layout = times.open + times.place + times.render;
        let debounce = match (started_at, asked_at) {
            (Some(started), Some(asked)) => started.duration_since(asked),
            _ => Duration::default(),
        };
        perf!(
            "restyle: {} {} pt, line height {}, margin {} mm: layout and page {:.0} ms; on the panel {:.0} ms after the change ({} ms of debounce, {:.0} ms of panel update); {} changes merged, {} layouts discarded",
            family,
            size,
            line_height,
            margin,
            ms(layout),
            ms(asked_at.map(|a| a.elapsed()).unwrap_or_default()),
            ms(debounce),
            ms(panel),
            merged,
            discarded
        );
        if let Some((dir, at_us)) = self.restyler.take_turn() {
            self.turn(dir, at_us)?;
        }
        Ok(())
    }

    /// The layout failed: put the typography of the panel back.
    fn revert_restyle(&mut self) -> Result<(), Error> {
        let applied = self.restyler.panel_typography();
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.settings = applied;
        update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
        self.ui.tree.invalidate(r.menu.size_steps, Refresh::Auto);
        self.prefs.apply(&applied);
        if let Err(e) = self.prefs.save(self.lib.data_dir()) {
            eprintln!("aristotle: can't save preferences: {e}");
        }
        self.restyler.set_mark(false);
        self.ui.tree.set_hidden(r.menu.busy, true);
        self.render_ui()?;
        if let Some((dir, at_us)) = self.restyler.take_turn() {
            self.turn(dir, at_us)?;
        }
        Ok(())
    }
}
