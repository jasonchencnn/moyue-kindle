// SPDX-License-Identifier: AGPL-3.0-or-later
//! WeRead cloud highlights and thoughts in the app (WR-006, contract §14.3,
//! §14.6).
//!
//! The wire work is `core::weread::notes`'s; this module owns what the app
//! adds: the `weread-notes.json` cache, the pull worker, the synckey cursors,
//! and the bridge from the raw cloud ranges to the rebuilt EPUB's XPointers
//! through the chapter map and anchors `download.rs` keeps.
//!
//! # When a pull runs
//!
//! Only when a WeRead book is opened or the reader asks for a manual refresh
//! from the reading menu. Display, page turns and the highlights list read
//! the in-memory cache and never touch the network or the disk.
//!
//! # Durability
//!
//! The cache is one JSON document written like `positions.json`:
//! `weread-notes.json.part` is `fsync`ed, the previous generation is rotated
//! to `.bak`, the part file is renamed into place and the folder is `fsync`ed.
//! A torn main file loads from `.bak`; a document this build cannot read is
//! moved aside instead of being overwritten by the next save.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aristotle_i18n::{tf, tr, Msg};
use aristotle_net::{Policy, ReadestTransport};
use aristotle_render::book::Position;
use aristotle_render::plato_core::anyhow::Error;
use inkread_core::sync::transport::HttpTransport;
use inkread_core::weread::notes::{
    map_notes, BookNotes, MappingStats, NoteAnchor, NoteKind, NotesMapping,
};
use inkread_core::weread::{BookPositions, Credentials, WeReadClient, WeReadError};

use crate::{App, Screen};

/// The notes cache in the app's private folder.
pub const NOTES_FILE: &str = "weread-notes.json";
/// Version of the on-disk notes document.
pub const NOTES_SCHEMA_VERSION: u32 = 1;
/// Timeout of one notes request.
const NOTES_TIMEOUT_SECS: u64 = 20;
/// Most review pages one pull follows (`hasMore`), 50 each.
const MAX_REVIEW_PAGES: usize = 8;
/// Entries asked of one `/review/list` page.
const REVIEW_PAGE_SIZE: u32 = 50;
/// An automatic pull is skipped when the book was pulled this recently.
const ENTER_THROTTLE: Duration = Duration::from_secs(60);

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Why a pull was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchReason {
    /// A WeRead book was opened; the same book is not pulled again for
    /// [`ENTER_THROTTLE`].
    EnterBook,
    /// Triggered alongside another action (such as a progress pull); bypasses
    /// the enter throttle and updates marks silently without clobbering
    /// `manual_message`.
    Auto,
    /// The reader tapped the menu's pull control.
    Manual,
}

/// One cloud note with the string the highlights list shows.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudRow {
    /// The list row's main text.
    pub text: String,
    /// The small line under it.
    pub meta: String,
    /// Where the row jumps, when the note has a place in this edition.
    pub target: Option<Position>,
    /// The detail sheet; `Some` when tapping opens the thought or explains
    /// why the note has no position.
    pub detail: Option<String>,
}

/// A pull's result, handed from the worker to the UI thread.
struct NotesOutcome {
    book_id: String,
    notes: BookNotes,
    changed: bool,
    error: Option<WeReadError>,
    /// `(refresh token the client started with, renewed credentials)`.
    rotated: Option<(String, Credentials)>,
}

/// A pull that finished, in the reader's language.
pub(crate) struct NotesEvent {
    book_id: String,
    reason: FetchReason,
    highlights: usize,
    thoughts: usize,
    message: Option<String>,
    rotated: Option<(String, Credentials)>,
}

struct ActiveNotesWorker {
    reason: FetchReason,
    rx: mpsc::Receiver<NotesOutcome>,
    cancel: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for ActiveNotesWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl ActiveNotesWorker {
    /// Stop caring about the pull without waiting for its request timeout:
    /// the channel is dropped with the state, so a late answer has nowhere
    /// to go, and the thread ends on its own.
    fn detach(mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.handle.take();
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NotesDocument {
    version: u32,
    books: BTreeMap<String, BookNotes>,
}

/// The app's cloud-note cache and its one pull worker.
pub struct WereadNotes {
    loaded: bool,
    dir: Option<PathBuf>,
    path: Option<PathBuf>,
    books: BTreeMap<String, BookNotes>,
    worker: Option<ActiveNotesWorker>,
    last_fetch: BTreeMap<String, Instant>,
    message: Option<String>,
    /// Requests started, for the tests' "page turns do no I/O" assertion.
    pub(crate) fetches: u64,
    /// Cache writes, for the same assertion.
    pub(crate) writes: u64,
}

impl Default for WereadNotes {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for WereadNotes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WereadNotes")
            .field("loaded", &self.loaded)
            .field("books", &self.books.len())
            .field(
                "notes",
                &self
                    .books
                    .values()
                    .map(|book| book.bookmark_count() + book.review_count())
                    .sum::<usize>(),
            )
            .field("fetching", &self.worker.is_some())
            .finish()
    }
}

impl WereadNotes {
    /// An empty cache; nothing is read until [`Self::ensure_loaded`].
    pub fn new() -> Self {
        Self {
            loaded: false,
            dir: None,
            path: None,
            books: BTreeMap::new(),
            worker: None,
            last_fetch: BTreeMap::new(),
            message: None,
            fetches: 0,
            writes: 0,
        }
    }

    /// Read the cache once from the state folder.
    pub fn ensure_loaded(&mut self, dir: &Path) {
        if self.loaded && self.dir.as_deref() == Some(dir) {
            return;
        }
        self.loaded = true;
        self.dir = Some(dir.to_path_buf());
        let weread = dir.join("weread");
        if let Err(error) = std::fs::create_dir_all(&weread) {
            eprintln!("aristotle: weread: can't create {weread:?}: {error}");
        }
        let _ = std::fs::set_permissions(&weread, std::fs::Permissions::from_mode(0o700));
        let path = dir.join(NOTES_FILE);
        self.path = Some(path.clone());
        let backup = sibling(&path, ".bak");
        let mut loaded = None;
        for candidate in [&path, &backup] {
            let Ok(bytes) = std::fs::read(candidate) else {
                continue;
            };
            match serde_json::from_slice::<NotesDocument>(&bytes) {
                Ok(document) if document.version == NOTES_SCHEMA_VERSION => {
                    loaded = Some(document);
                    break;
                }
                Ok(document) => {
                    // A schema this build cannot read: keep the file beside the
                    // live one instead of letting the next save overwrite it.
                    isolate(candidate, &format!(".unread-v{}", document.version));
                }
                Err(_) => {
                    isolate(candidate, ".unread");
                }
            }
        }
        self.books = loaded.map(|document| document.books).unwrap_or_default();
    }

    /// Whether a pull is running.
    pub fn fetching(&self) -> bool {
        self.worker.is_some()
    }

    /// Number of books with notes, highlights and thoughts in the cache.
    pub fn summary(&self) -> (usize, usize, usize) {
        let books = self
            .books
            .values()
            .filter(|book| book.bookmark_count() + book.review_count() > 0)
            .count();
        let highlights = self.books.values().map(BookNotes::bookmark_count).sum();
        let thoughts = self.books.values().map(BookNotes::review_count).sum();
        (books, highlights, thoughts)
    }

    /// Start one pull on a worker. Returns whether it started.
    ///
    /// Only the book-open hook and the reader's manual refresh call this.
    pub fn start_fetch(
        &mut self,
        dir: &Path,
        book_id: &str,
        credentials: Credentials,
        policy: Policy,
        reason: FetchReason,
    ) -> bool {
        self.ensure_loaded(dir);
        if self.worker.is_some() {
            return false;
        }
        let now = Instant::now();
        if reason == FetchReason::EnterBook {
            if let Some(last) = self.last_fetch.get(book_id) {
                if now.duration_since(*last) < ENTER_THROTTLE {
                    return false;
                }
            }
        }
        let notes = self.books.get(book_id).cloned().unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_book = book_id.to_string();
        let worker_notes = notes.clone();
        let handle = thread::Builder::new()
            .name("weread-notes".into())
            .spawn(move || {
                let transport = ReadestTransport::with_timeout(policy, NOTES_TIMEOUT_SECS);
                let (outcome, _) = fetch_notes_with(
                    transport,
                    credentials,
                    &worker_book,
                    worker_notes,
                    now_unix(),
                    Some(&worker_cancel),
                );
                let _ = tx.send(outcome);
            })
            .ok();
        let Some(handle) = handle else {
            self.message = Some(tr(Msg::WereadErrUnknown).to_string());
            return false;
        };
        self.last_fetch.insert(book_id.to_string(), now);
        self.fetches += 1;
        self.worker = Some(ActiveNotesWorker {
            reason,
            rx,
            cancel,
            handle: Some(handle),
        });
        true
    }

    /// Apply finished pulls; called between events.
    pub(crate) fn poll(&mut self) -> Vec<NotesEvent> {
        let mut events = Vec::new();
        while let Some(worker) = &self.worker {
            match worker.rx.try_recv() {
                Ok(outcome) => {
                    let book_id = outcome.book_id.clone();
                    let reason = worker.reason;
                    let mut event = self.apply(outcome);
                    event.reason = reason;
                    events.push(event);
                    self.worker = None;
                    self.last_fetch.insert(book_id, Instant::now());
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    break;
                }
            }
        }
        events
    }

    fn apply(&mut self, outcome: NotesOutcome) -> NotesEvent {
        let NotesOutcome {
            book_id,
            notes,
            changed,
            error,
            rotated,
        } = outcome;
        let mut message = None;
        if changed {
            if let Err(error) = self.save_with(&book_id, &notes) {
                eprintln!("aristotle: weread: can't save {NOTES_FILE}: {error}");
                message = Some(tr(Msg::WereadErrStore).to_string());
            } else {
                self.writes += 1;
            }
            // The display reads the memory even when the file could not take
            // the new generation.
            self.books.insert(book_id.clone(), notes);
        }
        if let Some(error) = &error {
            message = Some(tf!(
                Msg::WereadNotesFailed,
                error = super::error_message(error)
            ));
        }
        let counts = self.books.get(&book_id);
        let highlights = counts.map_or(0, BookNotes::bookmark_count);
        let thoughts = counts.map_or(0, BookNotes::review_count);
        self.message = message.clone();
        NotesEvent {
            book_id,
            reason: FetchReason::Manual,
            highlights,
            thoughts,
            message,
            rotated,
        }
    }

    /// Write the whole cache with the new generation of one book.
    fn save_with(&self, book_id: &str, notes: &BookNotes) -> io::Result<()> {
        let mut books = self.books.clone();
        books.insert(book_id.to_string(), notes.clone());
        self.save_books(&books)
    }

    fn save_books(&self, books: &BTreeMap<String, BookNotes>) -> io::Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let document = NotesDocument {
            version: NOTES_SCHEMA_VERSION,
            books: books.clone(),
        };
        let mut bytes = serde_json::to_vec(&document)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        bytes.push(b'\n');
        write_atomic(path, &bytes)
    }

    /// Forget every cached note and delete the file (the settings row).
    pub fn clear(&mut self) -> io::Result<()> {
        // Stopping the worker first keeps a finished pull from repopulating
        // the cache after the delete.
        if let Some(worker) = self.worker.take() {
            worker.detach();
        }
        self.books.clear();
        self.last_fetch.clear();
        self.message = None;
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        for candidate in [path.clone(), sibling(&path, ".bak")] {
            match std::fs::remove_file(&candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// The notes of `book_id` placed on the rebuilt EPUB, when cached.
    pub fn mapping(&self, book_id: &str, positions: &BookPositions) -> Option<NotesMapping> {
        let notes = self.books.get(book_id)?;
        Some(map_notes(
            &notes.compose(),
            &positions.map,
            &positions.anchors,
        ))
    }

    /// The placed notes as the reader's stored position pairs.
    pub fn positions_for(
        &self,
        book_id: &str,
        positions: &BookPositions,
    ) -> Vec<(Option<usize>, Position, Position)> {
        let Some(mapping) = self.mapping(book_id, positions) else {
            return Vec::new();
        };
        mapping
            .notes
            .iter()
            .filter_map(|note| match &note.anchor {
                NoteAnchor::Exact { start, end } => Some((
                    Position::XPointer {
                        xpointer: start.clone(),
                    },
                    Position::XPointer {
                        xpointer: end.clone(),
                    },
                )),
                NoteAnchor::Chapter { xpointer } => Some((
                    Position::XPointer {
                        xpointer: xpointer.clone(),
                    },
                    Position::XPointer {
                        xpointer: xpointer.clone(),
                    },
                )),
                NoteAnchor::Unplaced => None,
            })
            .map(|(start, end)| (None, start, end))
            .collect()
    }

    /// The highlights list's rows for `book_id`.
    pub fn rows(&self, book_id: &str, positions: &BookPositions) -> Vec<CloudRow> {
        let Some(mapping) = self.mapping(book_id, positions) else {
            return Vec::new();
        };
        let mut rows: Vec<CloudRow> = mapping
            .notes
            .iter()
            .map(|note| {
                let quote = note.note.quote.trim();
                let thought = note.note.thought.as_deref().unwrap_or("").trim();
                let target = match &note.anchor {
                    NoteAnchor::Exact { start, .. } => Some(Position::XPointer {
                        xpointer: start.clone(),
                    }),
                    NoteAnchor::Chapter { xpointer } => Some(Position::XPointer {
                        xpointer: xpointer.clone(),
                    }),
                    NoteAnchor::Unplaced => None,
                };
                let chapter = if note.note.chapter_title.is_empty() {
                    tr(Msg::WereadNotesWholeBook).to_string()
                } else {
                    note.note.chapter_title.clone()
                };
                let date = (note.note.create_time > 0)
                    .then(|| crate::marks::local_time(note.note.create_time))
                    .map(|time| time.chars().take(10).collect::<String>());
                let mut meta_parts = vec![tr(Msg::WereadTitle).to_string(), chapter];
                meta_parts.extend(date);
                meta_parts.retain(|part| !part.is_empty());
                let meta = meta_parts.join(" · ");
                let text = match note.note.kind {
                    NoteKind::Highlight if !quote.is_empty() => aristotle_i18n::quoted(quote),
                    NoteKind::Highlight => tr(Msg::ReaderHighlight).to_string(),
                    NoteKind::Thought if !thought.is_empty() => thought.to_string(),
                    NoteKind::Thought => aristotle_i18n::quoted(quote),
                };
                let detail = if !thought.is_empty() || target.is_none() {
                    let mut parts = vec![meta.clone()];
                    if !quote.is_empty() {
                        parts.push(aristotle_i18n::quoted(quote));
                    }
                    if !thought.is_empty() {
                        parts.push(thought.to_string());
                    }
                    if target.is_none() {
                        parts.push(tr(Msg::ReaderAnnotationNoPosition).to_string());
                    }
                    Some(parts.join("\n\n"))
                } else {
                    None
                };
                CloudRow {
                    text,
                    meta,
                    target,
                    detail,
                }
            })
            .collect();
        if let Some(row) = degrade_row(&mapping.stats) {
            rows.push(row);
        }
        rows
    }
}

fn degrade_row(stats: &MappingStats) -> Option<CloudRow> {
    if stats.chapter == 0 && stats.unplaced == 0 {
        return None;
    }
    let text = tf!(
        Msg::WereadNotesDegraded,
        chapter = stats.chapter,
        unplaced = stats.unplaced
    );
    Some(CloudRow {
        text,
        meta: tr(Msg::WereadTitle).to_string(),
        target: None,
        detail: Some(tf!(
            Msg::WereadNotesDegradedDetail,
            chapter = stats.chapter,
            unplaced = stats.unplaced
        )),
    })
}

/// Fetch one book's notes over an injected transport.
///
/// The highlights are pulled first; a failure there keeps the cache and skips
/// the thoughts, so a broken endpoint costs one request. A review page
/// failure keeps the pages that already merged. Everything the server did not
/// answer stays in the cache.
#[allow(clippy::too_many_arguments)]
fn fetch_notes_with<T: HttpTransport>(
    transport: T,
    credentials: Credentials,
    book_id: &str,
    mut notes: BookNotes,
    now: i64,
    cancel: Option<&AtomicBool>,
) -> (NotesOutcome, Credentials) {
    let original = credentials.clone();
    let mut client = WeReadClient::new(transport, credentials);
    let mut changed = false;
    let mut error = None;
    match client.bookmark_list(book_id, notes.bookmark_synckey) {
        Ok(list) => changed |= notes.merge_bookmarks(&list, now),
        Err(failure) => error = Some(failure),
    }
    if error.is_none() {
        let mut cursor = notes.review_synckey;
        for _ in 0..MAX_REVIEW_PAGES {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                break;
            }
            let page = match client.review_list(book_id, cursor, REVIEW_PAGE_SIZE) {
                Ok(page) => page,
                Err(failure) => {
                    error = Some(failure);
                    break;
                }
            };
            let next = page.synckey;
            let more = page.has_more;
            changed |= notes.merge_reviews(&page, now);
            if !more {
                break;
            }
            match next {
                Some(next) if next != cursor => cursor = next,
                _ => break,
            }
        }
    }
    let updated = client
        .credentials()
        .cloned()
        .unwrap_or_else(|| original.clone());
    let rotated = (updated.refresh_token != original.refresh_token)
        .then(|| (original.refresh_token.clone(), updated.clone()));
    (
        NotesOutcome {
            book_id: book_id.to_string(),
            notes,
            changed,
            error,
            rotated,
        },
        updated,
    )
}

/// `fsync` + `.bak` rotation, the FAT discipline of `positions.json`.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let part = sibling(path, ".part");
    let backup = sibling(path, ".bak");
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&part)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if path.exists() {
            let _ = std::fs::remove_file(&backup);
            std::fs::rename(path, &backup)?;
        }
        std::fs::rename(&part, path)?;
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// Move a cache generation this build cannot read out of the live slot, so a
/// later save cannot overwrite it.
fn isolate(path: &Path, suffix: &str) -> Option<PathBuf> {
    let mut target = sibling(path, suffix);
    let mut serial = 1;
    while target.exists() {
        target = sibling(path, &format!("{suffix}.{serial}"));
        serial += 1;
    }
    std::fs::rename(path, &target).ok()?;
    Some(target)
}

// ---------------------------------------------------------------- app hooks

impl App {
    /// The placed cloud marks of the open book, for `session::Source`.
    pub(crate) fn weread_notes_positions(
        &mut self,
        hash: &str,
    ) -> Vec<(Option<usize>, Position, Position)> {
        self.weread.downloads.ensure_loaded(&self.net.paths.state);
        let Some(book_id) = self.weread.downloads.book_id_of_hash(hash) else {
            return Vec::new();
        };
        let Some(positions) = self.weread.downloads.positions_of(&book_id) else {
            return Vec::new();
        };
        self.weread.notes.positions_for(&book_id, &positions)
    }

    /// A WeRead book is being opened: pull its notes unless that just ran.
    pub(crate) fn weread_notes_enter_book(&mut self, hash: &str) {
        self.weread.downloads.ensure_loaded(&self.net.paths.state);
        self.weread.notes.ensure_loaded(&self.net.paths.state);
        let Some(book_id) = self.weread.downloads.book_id_of_hash(hash) else {
            return;
        };
        let Some(credentials) = self.weread.credentials() else {
            return;
        };
        self.weread.notes.start_fetch(
            &self.net.paths.state,
            &book_id,
            credentials,
            crate::net::Net::policy(),
            FetchReason::EnterBook,
        );
    }

    /// The open reader's WeRead book id, when the book is one.
    pub(crate) fn weread_notes_open_book(&self) -> Option<String> {
        let Screen::Reader(reader) = &self.screen else {
            return None;
        };
        self.weread.downloads.book_id_of_hash(&reader.session.id)
    }

    /// Refresh the open WeRead book's notes. When `reason` is [`FetchReason::Auto`],
    /// runs silently in the background without touching `manual_message`.
    pub(crate) fn weread_notes_refresh_now(&mut self, reason: FetchReason) -> Result<(), Error> {
        let hash = match &self.screen {
            Screen::Reader(reader) => reader.session.id.clone(),
            _ => return Ok(()),
        };
        let Some(book_id) = self.weread.downloads.book_id_of_hash(&hash) else {
            if reason == FetchReason::Manual {
                self.readest.manual_message = Some(tr(Msg::WereadNotesNotWeread).to_string());
                return self.refresh_reader_sync_status();
            }
            return Ok(());
        };
        let Some(credentials) = self.weread.credentials() else {
            if reason == FetchReason::Manual {
                self.readest.manual_message = Some(tr(Msg::WereadErrAuth).to_string());
                return self.refresh_reader_sync_status();
            }
            return Ok(());
        };
        self.weread.notes.ensure_loaded(&self.net.paths.state);
        if self.weread.notes.fetching() {
            if reason == FetchReason::Manual {
                self.readest.manual_message = Some(tr(Msg::WereadNotesRefreshing).to_string());
                return self.refresh_reader_sync_status();
            }
            return Ok(());
        }
        if reason == FetchReason::Manual {
            self.readest.manual_message = Some(tr(Msg::WereadNotesRefreshing).to_string());
        }
        self.weread.notes.start_fetch(
            &self.net.paths.state,
            &book_id,
            credentials,
            crate::net::Net::policy(),
            reason,
        );
        if reason == FetchReason::Manual {
            self.refresh_reader_sync_status()?;
        }
        Ok(())
    }

    /// Apply finished pulls and show the new marks on the open page.
    pub(crate) fn poll_weread_notes(&mut self) -> Result<(), Error> {
        let events = self.weread.notes.poll();
        if events.is_empty() {
            return Ok(());
        }
        for event in events {
            if let Some((from, credentials)) = event.rotated {
                // Another worker may have rotated again meanwhile; never roll
                // the account back to the token this pull replaced.
                let current = self.weread.credentials();
                if current
                    .as_ref()
                    .map(|current| current.refresh_token.as_str())
                    == Some(from.as_str())
                {
                    self.weread.set_credentials(credentials);
                    self.weread.save();
                }
            }
            if event.reason == FetchReason::Manual {
                let message = event.message.clone().unwrap_or_else(|| {
                    tf!(
                        Msg::WereadNotesRefreshed,
                        highlights = event.highlights,
                        thoughts = event.thoughts
                    )
                });
                self.readest.manual_message = Some(message.clone());
                self.weread.message = Some(message);
                // The menu's one-line status follows a pull that finishes while
                // it is open.
                if matches!(&self.screen, Screen::Reader(reader) if reader.menu_open) {
                    self.refresh_reader_sync_status()?;
                }
            }
            let open = match &self.screen {
                Screen::Reader(reader) => {
                    self.weread
                        .downloads
                        .book_id_of_hash(&reader.session.id)
                        .as_deref()
                        == Some(event.book_id.as_str())
                }
                _ => false,
            };
            if !open {
                continue;
            }
            self.reload_marks();
            let marks_open = matches!(
                &self.screen,
                Screen::Reader(reader) if matches!(reader.overlay, crate::reading::Overlay::Marks { .. })
            );
            if marks_open {
                self.open_marks()?;
                continue;
            }
            let page_open = matches!(
                &self.screen,
                Screen::Reader(reader) if reader.overlay.is_none() && !reader.menu_open
            );
            if page_open {
                let page = {
                    let Screen::Reader(reader) = &mut self.screen else {
                        return Ok(());
                    };
                    reader.session.page(reader.session.current)?
                };
                self.ui.render();
                self.panel.show_page(&page, false, "weread notes")?;
            }
        }
        Ok(())
    }

    /// The settings row: cached books, highlights and thoughts.
    pub(crate) fn weread_notes_summary(&mut self) -> (usize, usize, usize) {
        self.weread.notes.ensure_loaded(&self.net.paths.state);
        self.weread.notes.summary()
    }

    /// The settings action: forget every cached cloud note.
    pub(crate) fn weread_notes_clear(&mut self) {
        self.weread.notes.ensure_loaded(&self.net.paths.state);
        match self.weread.notes.clear() {
            Ok(()) => self.settings_message = Some(tr(Msg::WereadNotesCleared).to_string()),
            Err(error) => {
                eprintln!("aristotle: weread: can't clear {NOTES_FILE}: {error}");
                self.settings_message = Some(tr(Msg::WereadErrStore).to_string());
            }
        }
    }
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
