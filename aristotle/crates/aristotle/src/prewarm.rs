// SPDX-License-Identifier: AGPL-3.0-or-later
//! Snapshots made ahead of time, so almost every open is a snapshot open.
//!
//! A book gets a [snapshot](crate::snapshot) when it is left, but not one
//! that was never opened here, and
//! a font size change makes every snapshot stale. So while the library sits
//! on screen untouched, one thread at a time opens the books most likely to
//! be opened next, renders the saved page and the one after it, and stores
//! them as their snapshot:
//!
//! - **Which books:** the book to continue and the next few by recency
//!   ([`CANDIDATES`] in all), each with a saved position and without a
//!   valid snapshot for the current style. Each is tried once per run,
//!   position, style and UI language, so the work is bounded and then stops.
//!   Imported Kindle positions wait until a real open saves an owned position.
//!   For crengine books, only the top candidate ("continue reading") is prewarmed
//!   to avoid multiple 5–9 second layouts keeping the engine lock busy.
//! - **When:** after [`QUIET`] without input, with the library on screen and
//!   no book opening. For crengine books, [`QUIET_CRENGINE`] is used.
//!   The thread runs at `SCHED_IDLE`, so on the single core of the PW4 it only
//!   gets the CPU while the app waits for input.
//!   When the reader opens the book being prewarmed, the open adopts the
//!   task and raises its priority to normal CFS, avoiding duplicate layout.
//!   When opening a different book, the prewarm task is cancelled and boosted
//!   to normal priority so it finishes quickly and releases locks.
//! - **No page numbers:** the job does not paginate whole books (the page
//!   numbers come with the first real open), so without page numbers on
//!   disk the snapshot shows the page without "x / y", as the first real
//!   open renders it. The footer's minutes use the reader's measured speed,
//!   as a real open does.
//!
//! Nothing here keeps the device awake: when powerd suspends it, the thread
//! is frozen with the rest of the process.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aristotle_library::{BookRecord, Library};
use aristotle_render::handoff::{Action, Handoff};
use aristotle_render::plato_core::anyhow::{format_err, Error};
use aristotle_render::style::Style;

use crate::session::{Prepared, Session, Source, OPEN_GATE};
use crate::snapshot::{self, Key, Snapshot};

/// How many books, by recency, get a snapshot ahead of time for Plato.
pub const CANDIDATES: usize = 5;

/// How long the library must be left alone before a Plato job starts.
pub const QUIET: Duration = Duration::from_secs(2);

/// How long the library must be left alone before a crengine job starts.
/// A full-book crengine layout takes 5–9 seconds, so quiet time is longer
/// to avoid firing during transient browsing.
pub const QUIET_CRENGINE: Duration = Duration::from_secs(3);

pub struct Job {
    pub title: String,
    pub key: Key,
    pub data_dir: PathBuf,
    pub worker: crate::open::Worker,
}

pub struct Prewarm {
    handoff: Handoff<(String, Style), Job>,
    /// Books tried in this run, by id, style, UI language and position.
    tried: HashSet<String>,
    quiet_since: Instant,
    /// Set when no candidate was left; cleared by input.
    exhausted: bool,
}

impl Default for Prewarm {
    fn default() -> Self {
        Prewarm {
            handoff: Handoff::new(),
            tried: HashSet::new(),
            quiet_since: Instant::now(),
            exhausted: false,
        }
    }
}

impl Prewarm {
    /// Any input: reset quiet timer. We do NOT cancel the running job
    /// immediately here, because the touch might be tapping to open this
    /// very book. If the reader opens another book or leaves the screen,
    /// `on_open` or `cancel` handles it with priority boost.
    pub fn input(&mut self) {
        self.quiet_since = Instant::now();
        self.exhausted = false;
    }

    /// Reader opens a book: handoff state machine determines whether to
    /// adopt, cancel & boost, or proceed normally.
    pub fn on_open(&mut self, id: &str, style: &Style) -> Action<Job> {
        self.handoff.open(&(id.to_string(), style.clone()))
    }

    /// Explicitly cancel the in-progress prewarm (e.g. sleep or screen change).
    pub fn cancel(&mut self) {
        if let Some(job) = self.handoff.cancel() {
            job.worker.cancel();
            crate::pagecache::restore_priority(job.worker.tid());
        }
    }

    /// When [`Prewarm::tick`] next has something to do, for the poll timeout.
    pub fn deadline(&self) -> Option<Instant> {
        if self.handoff.is_prewarming() || self.exhausted {
            None
        } else {
            Some(self.quiet_since + QUIET)
        }
    }

    /// Call between events while the library is on screen and no book is
    /// opening: reaps a finished job and starts the next one when quiet.
    /// `secs_per_page` is the reading speed the progress line uses, as for
    /// a real open, or the footers would differ.
    pub fn tick(&mut self, lib: &Library, style: &Style, mobi_cache: &Path, secs_per_page: f32) {
        if let Some(job) = self.handoff.prewarming_task_mut() {
            match job.worker.poll() {
                Some(Ok(prepared)) => {
                    let data_dir = job.data_dir.clone();
                    let key = job.key.clone();
                    let title = job.title.clone();
                    self.handoff.finish();
                    if let Err(e) = make_snapshot(&data_dir, &key, prepared, &title) {
                        eprintln!("aristotle: prewarm: {title}: {e:#}");
                    }
                }
                Some(Err(e)) => {
                    let title = job.title.clone();
                    let was_abandoned = job.worker.abandoned();
                    self.handoff.finish();
                    if !was_abandoned {
                        eprintln!("aristotle: prewarm: {title}: {e:#}");
                    }
                }
                None => {
                    return;
                }
            }
        }
        if self.exhausted {
            return;
        }
        for (idx, rec) in candidates(lib).iter().enumerate() {
            let is_cr = is_crengine(rec);
            // crengine policy: only prewarm the "continue reading" book (top candidate)
            if is_cr && idx > 0 {
                continue;
            }
            let required_quiet = if is_cr { QUIET_CRENGINE } else { QUIET };
            if self.quiet_since.elapsed() < required_quiet {
                return;
            }
            let Some((src, key)) = job_for(lib, rec, style, mobi_cache, secs_per_page) else {
                continue;
            };
            if !self.tried.insert(format!(
                "{} {} {} {:?}",
                rec.id, key.look, key.language, key.locator
            )) {
                continue;
            }
            if snapshot::is_valid(lib.data_dir(), &key, style) {
                continue;
            }
            if let Some(job) = start(src, style.clone(), key) {
                self.handoff.start((rec.id.clone(), style.clone()), job);
                return;
            }
        }
        self.exhausted = true;
    }
}

/// Whether a book record uses crengine-ng.
pub fn is_crengine(rec: &BookRecord) -> bool {
    rec.format == aristotle_library::Format::Epub
        && aristotle_render::book::epub_engine_is_crengine()
}

/// The source (without background pagination) and the key of a snapshot
/// made ahead of time for `rec`, with the reading speed `secs_per_page`
/// (the footer's minutes left); `None` without snapshot-compatible progress.
pub fn job_for(
    lib: &Library,
    rec: &BookRecord,
    style: &Style,
    mobi_cache: &Path,
    secs_per_page: f32,
) -> Option<(Source, Key)> {
    let progress = rec.progress.as_ref()?;
    if !snapshot::compatible_progress(progress) {
        return None;
    }
    let mut src = Source::new(lib, rec, mobi_cache).ok()?;
    src.secs_per_page = secs_per_page;
    let key = Key::new(&rec.id, src.stamp, style, progress.locator.clone(), false);
    src.paginate = false;
    Some((src, key))
}

/// The books to prepare, most likely first: the book to continue, then by
/// when they were last opened here or their progress last changed.
pub fn candidates(lib: &Library) -> Vec<&BookRecord> {
    let mut books: Vec<&BookRecord> = lib
        .books()
        .iter()
        .filter(|b| {
            b.progress
                .as_ref()
                .is_some_and(snapshot::compatible_progress)
        })
        .collect();
    books.sort_by_key(|b| {
        std::cmp::Reverse((b.last_opened, b.progress.as_ref().map(|p| p.updated)))
    });
    books.truncate(CANDIDATES);
    books
}

fn start(mut src: Source, style: Style, key: Key) -> Option<Job> {
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let tid = Arc::new(AtomicI32::new(0));
    let progress = Arc::new(AtomicU8::new(0));
    let (tx, rx) = crate::wake::channel();
    src.cancel = Some(cancel.clone());
    src.progress = Some(progress.clone());

    let title = src.title.clone();
    let data_dir = src.data_dir.clone();
    let started = Instant::now();

    let thread_tid = tid.clone();
    let thread_done = done.clone();
    let thread_style = style.clone();

    let spawned = thread::Builder::new()
        .name("prewarm".into())
        .spawn(move || {
            crate::pagecache::lower_priority();
            // SAFETY: gettid takes no arguments and cannot fail.
            thread_tid.store(
                unsafe { libc::syscall(libc::SYS_gettid) } as i32,
                Ordering::SeqCst,
            );
            let t_lock = Instant::now();
            let _gate = OPEN_GATE.lock().unwrap_or_else(|e| e.into_inner());
            let lock_wait = t_lock.elapsed();
            let mut result = Session::prepare(src, thread_style, true);
            if let Ok(prepared) = &mut result {
                prepared.times.lock_wait = lock_wait;
            }
            thread_done.store(true, Ordering::SeqCst);
            let _ = tx.send(result);
        });

    match spawned {
        Ok(_) => {
            let worker = crate::open::Worker::from_parts(rx, started, tid, cancel, done, progress);
            Some(Job {
                title,
                key,
                data_dir,
                worker,
            })
        }
        Err(e) => {
            eprintln!("aristotle: can't start the prewarm thread: {e}");
            None
        }
    }
}

/// Turn `prepared` into snapshot pages and store them on disk.
fn make_snapshot(data_dir: &Path, key: &Key, prepared: Prepared, title: &str) -> Result<(), Error> {
    let t = Instant::now();
    let mut session = Session::from_prepared(prepared)?;
    let here = session.current;
    let current = session
        .rendered(here)
        .ok_or_else(|| format_err!("the page was not rendered"))?;
    let mut info = crate::open::page_info(&mut session, here);
    info.locator = key.locator.clone();
    let mut pages = vec![(info, (*current).clone())];
    if let Some(next) = session.known_next(here) {
        if let Some(p) = session.rendered(next) {
            pages.push((crate::open::page_info(&mut session, next), (*p).clone()));
        }
    }
    let n = snapshot::store(
        data_dir,
        &Snapshot {
            key: key.clone(),
            pages,
        },
    )?;
    crate::perf!(
        "prewarm: snapshot of \"{title}\" made in {:.0} ms ({} KB, idle priority)",
        crate::ms(t.elapsed()),
        n / 1024
    );
    Ok(())
}

/// Open the book, render its saved page and the next, and store them.
/// `None` when cancelled. Retained for benchmarks.
pub fn make(
    src: Source,
    style: Style,
    key: Key,
    stop: &AtomicBool,
) -> Result<Option<usize>, Error> {
    let data_dir: PathBuf = src.data_dir.clone();
    if stop.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let prepared = match Session::prepare(src, style, true) {
        Err(_) if stop.load(Ordering::Relaxed) => return Ok(None),
        other => other?,
    };
    if stop.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let mut session = Session::from_prepared(prepared)?;
    let here = session.current;
    let current = session
        .rendered(here)
        .ok_or_else(|| format_err!("the page was not rendered"))?;
    let mut info = crate::open::page_info(&mut session, here);
    info.locator = key.locator.clone();
    let mut pages = vec![(info, (*current).clone())];
    if let Some(next) = session.known_next(here) {
        if let Some(p) = session.rendered(next) {
            pages.push((crate::open::page_info(&mut session, next), (*p).clone()));
        }
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let n = snapshot::store(&data_dir, &Snapshot { key, pages })?;
    Ok(Some(n))
}
