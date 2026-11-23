// SPDX-License-Identifier: AGPL-3.0-or-later
//! WeRead books in the app: chapter downloads, the book-id map, and the
//! cloud position that opens a rebuilt EPUB at the right place.
//!
//! The pipeline itself is `core::weread::download` (WR-003); this module only
//! supplies the paths and the transport, reports chapter progress, and keeps
//! the results on disk:
//!
//! - `weread/books.json`: `bookId -> partialMD5` (`BookMappingStore`);
//! - `weread/positions/<bookId>.json`: the chapter map and the XPointer anchors;
//! - `weread/cache/<bookId>/`: the resumable chapter cache;
//! - `<library>/WeRead/<bookId>.epub`: the rebuilt book.
//!
//! A worker thread sends progress and results through the app's wake channel,
//! so the shelf keeps taking page turns while a book downloads. Opening a
//! downloaded card asks `/book/getProgress` on a worker and turns the cloud
//! coordinate into a crengine XPointer; the book itself opens through the
//! existing open path with that XPointer as its jump target. Neither path
//! writes to disk.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aristotle_i18n::{tr, Msg};
use aristotle_library::index::{Annotation, AnnotationKind};
use aristotle_net::{Policy, ReadestTransport};
use inkread_core::sync::transport::{HttpTransport, TransportError};
use inkread_core::weread::{
    download_book_with_progress, plan_download, AnchorMap, BookMappingStore, BookPositions,
    ChapterInfos, Credentials, DownloadError, DownloadOutcome, DownloadRequest, PositionMap,
    SourceFormat, WeReadClient, WeReadCoordinate, WeReadError,
};

use aristotle_ui::screens::weread_shelf::WereadDownloadState;

use crate::marks;
use crate::wake;
use crate::{App, Screen};

/// Version of the on-disk position document.
pub const POSITIONS_SCHEMA_VERSION: u32 = 1;
/// Timeout of chapter downloads and metadata requests during a book download.
const DOWNLOAD_TIMEOUT_SECS: u64 = 60;
/// Body size limit for illustrated WeRead chapter batches (64 MiB).
const DOWNLOAD_MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
/// Minimum interval between shelf download progress refreshes.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(500);
/// Minimum percentage delta that triggers a progress refresh before the interval elapses.
const PROGRESS_MIN_PERCENT_STEP: u8 = 5;
/// Timeout of the open-position request.
const OPEN_TIMEOUT_SECS: u64 = 10;

/// One download request, with the paths the app owns.
#[derive(Debug, Clone)]
pub struct DownloadParams {
    pub book_id: String,
    pub title: String,
    pub cache_dir: PathBuf,
    pub epub_path: PathBuf,
    pub mapping_path: PathBuf,
}

/// A completed download and the coordinate map built from it.
pub struct DownloadedBook {
    pub outcome: DownloadOutcome,
    pub positions: BookPositions,
    /// Present when `WeReadClient` silently renewed credentials during the run.
    pub renewed_credentials: Option<Credentials>,
}

/// What can stop a download before it commits a book.
#[derive(Debug)]
pub enum DownloadFailure {
    Cancelled,
    UnsupportedFormat,
    NoEligibleChapters,
    MissingBookVersion,
    WeRead(WeReadError),
    Core(DownloadError),
}

impl From<DownloadError> for DownloadFailure {
    fn from(error: DownloadError) -> Self {
        match error {
            DownloadError::UnsupportedFormat(_) => Self::UnsupportedFormat,
            DownloadError::NoEligibleChapters { .. } => Self::NoEligibleChapters,
            DownloadError::WeRead(error) => Self::WeRead(error),
            other => Self::Core(other),
        }
    }
}

impl From<WeReadError> for DownloadFailure {
    fn from(error: WeReadError) -> Self {
        Self::WeRead(error)
    }
}

/// The reader's words for a failed download.
pub fn download_error_message(failure: &DownloadFailure) -> String {
    if !matches!(failure, DownloadFailure::Cancelled) {
        eprintln!("aristotle: weread download failed: {failure:?}");
    }
    match failure {
        DownloadFailure::Cancelled => tr(Msg::WereadErrDownloadCancelled).to_string(),
        DownloadFailure::UnsupportedFormat => tr(Msg::WereadErrDownloadFormat).to_string(),
        DownloadFailure::NoEligibleChapters => tr(Msg::WereadErrDownloadEligibility).to_string(),
        DownloadFailure::MissingBookVersion => tr(Msg::WereadErrDownloadVersion).to_string(),
        DownloadFailure::WeRead(error) => super::error_message(error),
        DownloadFailure::Core(error) => match error {
            DownloadError::Storage(_) | DownloadError::Mapping(_) | DownloadError::Epub(_) => {
                tr(Msg::WereadErrDownloadStorage).to_string()
            }
            DownloadError::WeRead(error) => super::error_message(error),
            DownloadError::NoEligibleChapters { .. } => {
                tr(Msg::WereadErrDownloadEligibility).to_string()
            }
            DownloadError::UnsupportedFormat(_) => tr(Msg::WereadErrDownloadFormat).to_string(),
            _ => tr(Msg::WereadErrDownloadFailed).to_string(),
        },
    }
}

/// What opening a downloaded card led to.
#[derive(Debug)]
pub enum OpenAction {
    /// The EPUB or its coordinate map is gone: download it again.
    NeedDownload,
    /// The EPUB is on disk but the library has not indexed it yet.
    AwaitingIndex { path: PathBuf },
    /// The progress request is running on a worker.
    Started,
    /// A download or an open is already running.
    Busy,
    /// There are no credentials to read the cloud position with.
    NoCredentials,
}

/// A message from a WeRead worker to the app.
enum WorkerMsg {
    Progress {
        done: usize,
        total: usize,
    },
    Downloaded {
        result: Box<Result<DownloadedBook, DownloadFailure>>,
    },
    OpenReady {
        xpointer: Option<String>,
        message: Option<String>,
        renewed_credentials: Option<Credentials>,
    },
}

/// An event the app applies on the UI thread.
pub enum WereadEvent {
    Progress {
        book_id: String,
        percent: u8,
    },
    CredentialsRefreshed(Credentials),
    DownloadFinished {
        book_id: String,
        path: PathBuf,
        message: Option<String>,
    },
    OpenReady {
        book_id: String,
        hash: String,
        xpointer: Option<String>,
        message: Option<String>,
    },
}

struct ActiveWorker {
    book_id: String,
    kind: WorkerKind,
    rx: mpsc::Receiver<WorkerMsg>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerKind {
    Download,
    Open,
}

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PositionDocument {
    version: u32,
    books: BTreeMap<String, BookPositions>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookPositionFile {
    version: u32,
    book_id: String,
    positions: BookPositions,
}

fn positions_dir_for_legacy(legacy_path: &Path) -> PathBuf {
    legacy_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("positions")
}

fn read_legacy_document(legacy_path: &Path) -> Option<PositionDocument> {
    let backup = sibling(legacy_path, ".bak");
    for candidate in [legacy_path, &backup] {
        let Ok(bytes) = fs::read(candidate) else {
            continue;
        };
        if let Ok(document) = serde_json::from_slice::<PositionDocument>(&bytes) {
            if document.version == POSITIONS_SCHEMA_VERSION {
                return Some(document);
            }
        }
    }
    None
}

fn read_book_position_file(path: &Path) -> Option<BookPositionFile> {
    let backup = sibling(path, ".bak");
    for candidate in [path, &backup] {
        let Ok(bytes) = fs::read(candidate) else {
            continue;
        };
        if let Ok(document) = serde_json::from_slice::<BookPositionFile>(&bytes) {
            if document.version == POSITIONS_SCHEMA_VERSION {
                return Some(document);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PositionStore {
    path: Option<PathBuf>,
    books: BTreeMap<String, BookPositions>,
}

impl PositionStore {
    pub(crate) fn load(legacy_path: PathBuf) -> Self {
        let mut store = Self {
            path: Some(legacy_path.clone()),
            books: BTreeMap::new(),
        };
        let positions_dir = positions_dir_for_legacy(&legacy_path);
        let mut per_book_ids = BTreeSet::new();
        if let Ok(entries) = fs::read_dir(&positions_dir) {
            let mut candidates = BTreeSet::new();
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.ends_with(".json") {
                    candidates.insert(path);
                } else if let Some(base) = name.strip_suffix(".bak") {
                    if base.ends_with(".json") {
                        candidates.insert(positions_dir.join(base));
                    }
                }
            }
            for candidate in candidates {
                if let Some(doc) = read_book_position_file(&candidate) {
                    per_book_ids.insert(doc.book_id.clone());
                    store.books.insert(doc.book_id, doc.positions);
                }
            }
        }
        if let Some(legacy) = read_legacy_document(&legacy_path) {
            let mut migrated_all = true;
            for (book_id, positions) in legacy.books {
                if !per_book_ids.contains(&book_id) {
                    store.books.insert(book_id.clone(), positions);
                    if store.save_book(&book_id).is_err() {
                        migrated_all = false;
                    }
                }
            }
            if migrated_all {
                let _ = fs::remove_file(&legacy_path);
                let _ = fs::remove_file(sibling(&legacy_path, ".bak"));
            }
        }
        store
    }

    /// Read a single book's positions directly from `weread/positions/<safe_book_id>.json`,
    /// falling back to legacy `weread/positions.json` only when the per-book file is absent.
    pub(crate) fn load_book(state_dir: &Path, book_id: &str) -> Option<BookPositions> {
        let weread_dir = state_dir.join("weread");
        let per_book = weread_dir
            .join("positions")
            .join(format!("{}.json", safe_book_id(book_id)));
        if let Some(doc) = read_book_position_file(&per_book) {
            if doc.book_id == book_id {
                return Some(doc.positions);
            }
        }
        let legacy_path = weread_dir.join("positions.json");
        read_legacy_document(&legacy_path).and_then(|mut doc| doc.books.remove(book_id))
    }

    pub(crate) fn get(&self, book_id: &str) -> Option<&BookPositions> {
        self.books.get(book_id)
    }

    pub(crate) fn insert(&mut self, book_id: impl Into<String>, positions: BookPositions) {
        self.books.insert(book_id.into(), positions);
    }

    /// Persist a single book's positions as compact JSON under `weread/positions/<safe_book_id>.json`.
    pub(crate) fn save_book(&self, book_id: &str) -> io::Result<()> {
        let Some(legacy_path) = self.path.as_ref() else {
            return Ok(());
        };
        let Some(positions) = self.books.get(book_id) else {
            return Ok(());
        };
        let dir = positions_dir_for_legacy(legacy_path);
        fs::create_dir_all(&dir)?;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        let path = dir.join(format!("{}.json", safe_book_id(book_id)));
        let document = BookPositionFile {
            version: POSITIONS_SCHEMA_VERSION,
            book_id: book_id.to_string(),
            positions: positions.clone(),
        };
        let bytes = serde_json::to_vec(&document)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        write_atomic(&path, &bytes)
    }

    #[cfg(test)]
    pub(crate) fn save(&self) -> io::Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let document = PositionDocument {
            version: POSITIONS_SCHEMA_VERSION,
            books: self.books.clone(),
        };
        let bytes = serde_json::to_vec(&document)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        write_atomic(path, &bytes)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let part = sibling(path, ".part");
    let backup = sibling(path, ".bak");
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&part)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if path.exists() {
            let _ = fs::remove_file(&backup);
            fs::rename(path, &backup)?;
        }
        fs::rename(&part, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// A book id is server data; the path must stay one file name.
pub(crate) fn safe_book_id(book_id: &str) -> String {
    let safe: String = book_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect();
    if safe.is_empty() {
        "book".to_string()
    } else {
        safe
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The app's WeRead download state: paths, stored maps and the one worker.
#[derive(Default)]
pub struct WereadDownloads {
    loaded: bool,
    state_dir: Option<PathBuf>,
    root: Option<PathBuf>,
    mapping: Option<BookMappingStore>,
    positions: PositionStore,
    states: BTreeMap<String, WereadDownloadState>,
    worker: Option<ActiveWorker>,
    pending_open: Option<String>,
    pending_jump: Option<(String, String)>,
    message: Option<String>,
}

impl fmt::Debug for WereadDownloads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WereadDownloads")
            .field("loaded", &self.loaded)
            .field("states", &self.states)
            .field("worker", &self.worker.as_ref().map(|worker| worker.kind))
            .field("pending_open", &self.pending_open)
            .field("message", &self.message)
            .finish()
    }
}

impl WereadDownloads {
    /// Read the maps once; later calls are free.
    pub fn ensure_loaded(&mut self, dir: &Path) {
        if self.loaded && self.state_dir.as_deref() == Some(dir) {
            return;
        }
        self.loaded = true;
        self.state_dir = Some(dir.to_path_buf());
        let weread = dir.join("weread");
        if let Err(error) = fs::create_dir_all(&weread) {
            self.message = Some(tr(Msg::WereadErrStore).to_string());
            eprintln!("aristotle: weread: can't create {weread:?}: {error}");
            return;
        }
        let _ = fs::set_permissions(&weread, fs::Permissions::from_mode(0o700));
        let mapping = weread.join("books.json");
        match BookMappingStore::load(&mapping) {
            Ok(store) => self.mapping = Some(store),
            Err(error) => {
                eprintln!("aristotle: weread: can't read {mapping:?}: {error}");
                self.mapping = None;
            }
        }
        self.positions = PositionStore::load(weread.join("positions.json"));
    }

    fn set_root(&mut self, root: &Path) {
        self.root = Some(root.to_path_buf());
    }

    fn weread_dir(&self) -> PathBuf {
        self.state_dir
            .as_ref()
            .map(|dir| dir.join("weread"))
            .unwrap_or_else(|| PathBuf::from("weread"))
    }

    fn mapping_path(&self) -> PathBuf {
        self.weread_dir().join("books.json")
    }

    fn epub_path(&self, book_id: &str) -> PathBuf {
        self.root
            .as_ref()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("WeRead")
            .join(format!("{}.epub", safe_book_id(book_id)))
    }

    /// The UI state of one book.
    pub fn state(&self, book_id: &str) -> WereadDownloadState {
        if let Some(state) = self.states.get(book_id) {
            return *state;
        }
        if self
            .mapping
            .as_ref()
            .is_some_and(|mapping| mapping.get(book_id).is_some())
        {
            WereadDownloadState::Downloaded
        } else {
            WereadDownloadState::NotDownloaded
        }
    }

    /// The hash `download_book` stored for `book_id`.
    pub fn hash_of(&self, book_id: &str) -> Option<String> {
        self.mapping
            .as_ref()
            .and_then(|mapping| mapping.get(book_id))
            .map(|row| row.hash.clone())
    }

    /// The coordinate map of `book_id`, when the book is downloaded.
    pub fn positions_of(&self, book_id: &str) -> Option<BookPositions> {
        self.positions.get(book_id).cloned()
    }

    /// The WeRead book id whose rebuilt EPUB has `hash`.
    pub fn book_id_of_hash(&self, hash: &str) -> Option<String> {
        self.mapping
            .as_ref()?
            .iter()
            .find(|(_, mapping)| mapping.hash.eq_ignore_ascii_case(hash))
            .map(|(book_id, _)| book_id.to_string())
    }

    /// Lowercased content hashes of every book downloaded via WeRead.
    pub fn known_hashes(&self) -> std::collections::HashSet<String> {
        self.mapping
            .as_ref()
            .map(|m| {
                m.iter()
                    .map(|(_, row)| row.hash.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Re-read the map a finished download appended to.
    ///
    /// A failed read keeps the in-memory map: the card still says
    /// "Downloaded", so dropping the hash here would make the next open
    /// download the book again.
    pub(super) fn reload_mapping(&mut self) {
        let path = self.mapping_path();
        if let Ok(store) = BookMappingStore::load(&path) {
            self.mapping = Some(store);
        } else {
            eprintln!("aristotle: weread: can't reload {path:?}; keeping the in-memory map");
        }
    }

    /// The latest download/open message, if any.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Start downloading `book_id` on a worker.
    pub fn start(
        &mut self,
        state_dir: &Path,
        root: &Path,
        book_id: &str,
        title: &str,
        format: &str,
        credentials: Credentials,
        policy: Policy,
    ) -> Result<(), String> {
        self.ensure_loaded(state_dir);
        self.set_root(root);
        if self.worker.is_some() {
            return Err(tr(Msg::WereadErrDownloadBusy).to_string());
        }
        let format = match SourceFormat::parse(format) {
            Ok(format) => format,
            Err(_) => {
                self.states
                    .insert(book_id.to_string(), WereadDownloadState::Failed);
                return Err(tr(Msg::WereadErrDownloadFormat).to_string());
            }
        };
        if format != SourceFormat::Epub {
            self.states
                .insert(book_id.to_string(), WereadDownloadState::Failed);
            return Err(tr(Msg::WereadErrDownloadFormat).to_string());
        }
        let params = DownloadParams {
            book_id: book_id.to_string(),
            title: title.to_string(),
            // Keep resumable chapter cache on ext4 (`state_dir/weread/cache`)
            // rather than FAT32 `/mnt/us` to avoid FAT filename/fsync errors;
            // chapter 0 is fetched once (~10 MB) and the cache is removed on
            // completion.
            cache_dir: self.weread_dir().join("cache").join(safe_book_id(book_id)),
            epub_path: self.epub_path(book_id),
            mapping_path: self.mapping_path(),
        };
        if matches!(self.states.get(book_id), Some(WereadDownloadState::Failed)) {
            let _ = fs::remove_dir_all(&params.cache_dir);
        }
        if let Some(parent) = params.epub_path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                self.states
                    .insert(book_id.to_string(), WereadDownloadState::Failed);
                return Err(format!("{}: {error}", tr(Msg::WereadErrDownloadStorage)));
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = wake::channel();
        let worker_tx = tx.clone();
        let worker_cancel = cancel.clone();
        let worker_params = params.clone();
        let handle = thread::Builder::new()
            .name("weread-download".into())
            .spawn(move || {
                let progress_tx = worker_tx.clone();
                let transport = ReadestTransport::with_timeout_and_limit(
                    policy,
                    DOWNLOAD_TIMEOUT_SECS,
                    DOWNLOAD_MAX_BODY_BYTES,
                );
                let result = run_download(
                    credentials,
                    &worker_params,
                    transport,
                    worker_cancel,
                    move |done, total| {
                        let _ = progress_tx.send(WorkerMsg::Progress { done, total });
                    },
                );
                let _ = worker_tx.send(WorkerMsg::Downloaded {
                    result: Box::new(result),
                });
            })
            .map_err(|error| {
                self.states
                    .insert(book_id.to_string(), WereadDownloadState::Failed);
                format!("can't start the download: {error}")
            })?;
        self.worker = Some(ActiveWorker {
            book_id: book_id.to_string(),
            kind: WorkerKind::Download,
            rx,
            cancel,
            handle: Some(handle),
        });
        self.states.insert(
            book_id.to_string(),
            WereadDownloadState::Downloading { percent: 0 },
        );
        self.message = None;
        Ok(())
    }

    /// Ask the cloud position of a downloaded book, then open it.
    pub fn open(
        &mut self,
        state_dir: &Path,
        root: &Path,
        book_id: &str,
        credentials: Option<Credentials>,
        policy: Policy,
        indexed: bool,
    ) -> OpenAction {
        self.ensure_loaded(state_dir);
        self.set_root(root);
        if self.worker.is_some() {
            return OpenAction::Busy;
        }
        if self.hash_of(book_id).is_none() {
            return OpenAction::NeedDownload;
        }
        let path = self.epub_path(book_id);
        if !path.is_file() {
            return OpenAction::NeedDownload;
        }
        let Some(positions) = self.positions.get(book_id).cloned() else {
            return OpenAction::NeedDownload;
        };
        if !indexed {
            return OpenAction::AwaitingIndex { path };
        }
        let Some(credentials) = credentials else {
            return OpenAction::NoCredentials;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = wake::channel();
        let worker_cancel = cancel.clone();
        let worker_book = book_id.to_string();
        let handle = thread::Builder::new()
            .name("weread-open".into())
            .spawn(move || {
                let (xpointer, message, renewed_credentials) = run_open(
                    credentials,
                    policy,
                    &worker_book,
                    &positions.map,
                    &positions.anchors,
                    &worker_cancel,
                );
                let _ = tx.send(WorkerMsg::OpenReady {
                    xpointer,
                    message,
                    renewed_credentials,
                });
            })
            .ok();
        self.worker = Some(ActiveWorker {
            book_id: book_id.to_string(),
            kind: WorkerKind::Open,
            rx,
            cancel,
            handle,
        });
        OpenAction::Started
    }

    /// Stop the worker of `book_id`, if it is a download.
    pub fn cancel(&mut self, book_id: &str) {
        if let Some(worker) = &self.worker {
            if worker.kind == WorkerKind::Download && worker.book_id == book_id {
                worker.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Stop whatever worker is running (leaving the screen).
    pub fn cancel_all(&mut self) {
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Remember the book whose scan should open it.
    pub fn set_pending_open(&mut self, book_id: &str) {
        self.pending_open = Some(book_id.to_string());
    }

    /// Take the book whose scan should open it.
    pub fn take_pending_open(&mut self) -> Option<String> {
        self.pending_open.take()
    }

    /// The jump target of the next `open_book` for `hash`, if any.
    pub fn take_open_jump(&mut self, hash: &str) -> Option<Annotation> {
        let (wanted, xpointer) = self.pending_jump.take()?;
        if wanted != hash {
            self.pending_jump = Some((wanted, xpointer));
            return None;
        }
        Some(jump_annotation(&xpointer))
    }

    /// Results that arrived since the last call.
    pub fn poll(&mut self) -> Vec<WereadEvent> {
        let mut messages = Vec::new();
        let mut disconnected = false;
        if let Some(worker) = &self.worker {
            loop {
                match worker.rx.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        let mut events = Vec::new();
        let mut terminal = disconnected;
        let mut saw_terminal = false;
        for message in messages {
            match message {
                WorkerMsg::Progress { done, total } => {
                    let Some(worker) = &self.worker else {
                        continue;
                    };
                    let percent = if total == 0 {
                        0
                    } else {
                        ((done.saturating_mul(100) / total).min(100)) as u8
                    };
                    let book_id = worker.book_id.clone();
                    self.states.insert(
                        book_id.clone(),
                        WereadDownloadState::Downloading { percent },
                    );
                    events.push(WereadEvent::Progress { book_id, percent });
                }
                WorkerMsg::Downloaded { result } => {
                    terminal = true;
                    saw_terminal = true;
                    let Some(worker) = &self.worker else {
                        continue;
                    };
                    let book_id = worker.book_id.clone();
                    match *result {
                        Ok(downloaded) => {
                            if let Some(renewed) = downloaded.renewed_credentials {
                                events.push(WereadEvent::CredentialsRefreshed(renewed));
                            }
                            let path = downloaded.outcome.epub_path.clone();
                            self.positions.insert(book_id.clone(), downloaded.positions);
                            self.reload_mapping();
                            match self.positions.save_book(&book_id) {
                                Ok(()) => {
                                    self.states
                                        .insert(book_id.clone(), WereadDownloadState::Downloaded);
                                    self.message = Some(tr(Msg::WereadDownloadReady).to_string());
                                    events.push(WereadEvent::DownloadFinished {
                                        book_id,
                                        path,
                                        message: None,
                                    });
                                }
                                Err(error) => {
                                    self.states
                                        .insert(book_id.clone(), WereadDownloadState::Failed);
                                    let message =
                                        format!("{}: {error}", tr(Msg::WereadErrDownloadStorage));
                                    self.message = Some(message.clone());
                                    events.push(WereadEvent::DownloadFinished {
                                        book_id,
                                        path,
                                        message: Some(message),
                                    });
                                }
                            }
                        }
                        Err(failure) => {
                            let message = download_error_message(&failure);
                            let state = if matches!(failure, DownloadFailure::Cancelled) {
                                WereadDownloadState::NotDownloaded
                            } else {
                                WereadDownloadState::Failed
                            };
                            self.states.insert(book_id.clone(), state);
                            self.message = Some(message.clone());
                            events.push(WereadEvent::DownloadFinished {
                                book_id,
                                path: PathBuf::new(),
                                message: Some(message),
                            });
                        }
                    }
                }
                WorkerMsg::OpenReady {
                    xpointer,
                    message,
                    renewed_credentials,
                } => {
                    terminal = true;
                    saw_terminal = true;
                    if let Some(renewed) = renewed_credentials {
                        events.push(WereadEvent::CredentialsRefreshed(renewed));
                    }
                    let Some(worker) = &self.worker else {
                        continue;
                    };
                    let book_id = worker.book_id.clone();
                    let Some(hash) = self.hash_of(&book_id) else {
                        continue;
                    };
                    if let Some(xpointer) = &xpointer {
                        self.pending_jump = Some((hash.clone(), xpointer.clone()));
                    }
                    if let Some(message) = &message {
                        self.message = Some(message.clone());
                    }
                    events.push(WereadEvent::OpenReady {
                        book_id,
                        hash,
                        xpointer,
                        message,
                    });
                }
            }
        }
        if terminal {
            if let Some(mut worker) = self.worker.take() {
                let kind = worker.kind;
                if let Some(handle) = worker.handle.take() {
                    let _ = handle.join();
                }
                if disconnected && !saw_terminal {
                    let message = tr(Msg::WereadErrDownloadFailed).to_string();
                    self.message = Some(message.clone());
                    if kind == WorkerKind::Download {
                        self.states
                            .insert(worker.book_id.clone(), WereadDownloadState::Failed);
                        events.push(WereadEvent::DownloadFinished {
                            book_id: worker.book_id.clone(),
                            path: PathBuf::new(),
                            message: Some(message),
                        });
                    }
                }
            }
        }
        events
    }
}

/// Deduplicated candidate `bookVersion` values in preference order:
/// `getProgress.book.bookVersion`, `chapterInfos.book.version`, `chapterInfos.synckey`.
fn candidate_book_versions(progress: Option<i64>, toc: &ChapterInfos) -> Vec<String> {
    let mut versions = Vec::new();
    for value in [progress, toc.book_version, toc.synckey]
        .into_iter()
        .flatten()
    {
        let text = value.to_string();
        if !versions.contains(&text) {
            versions.push(text);
        }
    }
    versions
}

/// §14.3 `bookVersion`: the progress record, the TOC's book version, then the
/// TOC's `synckey`. A book with no progress record still has a version.
pub(super) fn choose_book_version(progress: Option<i64>, toc: &ChapterInfos) -> Option<String> {
    candidate_book_versions(progress, toc).into_iter().next()
}

fn collect_targets(toc: &ChapterInfos) -> Vec<i64> {
    toc.chapters
        .iter()
        .filter_map(|chapter| chapter.chapter_uid)
        .filter(|uid| *uid != 0)
        .collect()
}

/// Run the core download pipeline with chapter progress and cancellation.
pub fn run_download<T: HttpTransport>(
    credentials: Credentials,
    params: &DownloadParams,
    transport: T,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(usize, usize) + Send + Sync + 'static,
) -> Result<DownloadedBook, DownloadFailure> {
    let transport = DownloadTransport {
        inner: transport,
        cancel: cancel.clone(),
    };
    let initial_credentials = credentials.clone();
    let mut client = WeReadClient::new(transport, credentials);
    let mut toc = client.chapter_infos(&params.book_id)?;
    let progress_response = client.get_progress(&params.book_id)?;
    // §14.3: a book without a progress record still carries its version on the
    // TOC; only all three sources missing is an error.
    let mut pending_versions: VecDeque<String> = candidate_book_versions(
        progress_response
            .book
            .as_ref()
            .and_then(|book| book.book_version),
        &toc,
    )
    .into();
    if pending_versions.is_empty() {
        return Err(DownloadFailure::MissingBookVersion);
    }
    let targets = collect_targets(&toc);
    let plan = plan_download(&toc, &targets)?;
    let mut planned = plan.eligible.len();
    let mut last_emit: Option<Instant> = None;
    let mut last_percent: u8 = 0;
    let mut last_done: Option<usize> = None;
    let mut report_progress = |done: usize, total: usize| {
        let clamped_done = done.min(total);
        let percent = if total == 0 {
            100
        } else {
            ((clamped_done.saturating_mul(100) / total).min(100)) as u8
        };
        let now = Instant::now();
        let completed = total > 0 && clamped_done >= total;
        let should_emit = if completed {
            last_done != Some(clamped_done)
        } else {
            match last_emit {
                None => true,
                Some(prev) => {
                    now.duration_since(prev) >= PROGRESS_MIN_INTERVAL
                        || percent.saturating_sub(last_percent) >= PROGRESS_MIN_PERCENT_STEP
                }
            }
        };
        if should_emit {
            last_emit = Some(now);
            last_percent = percent;
            last_done = Some(clamped_done);
            progress(clamped_done, total);
        }
    };
    let mut request = DownloadRequest {
        book_id: params.book_id.clone(),
        title: params.title.clone(),
        format: SourceFormat::Epub,
        book_version: String::new(),
        chapters: targets,
        epub_path: params.epub_path.clone(),
        mapping_path: params.mapping_path.clone(),
        work_dir: params.cache_dir.clone(),
    };
    let mut tried_versions: BTreeSet<String> = BTreeSet::new();
    let mut refetched_toc = false;
    let (outcome, book_version) = loop {
        let Some(candidate) = pending_versions.pop_front() else {
            return Err(DownloadFailure::MissingBookVersion);
        };
        tried_versions.insert(candidate.clone());
        request.book_version = candidate.clone();
        match download_book_with_progress(&mut client, &toc, &request, &mut report_progress) {
            Ok(outcome) => break (outcome, candidate),
            Err(_error) if cancel.load(Ordering::SeqCst) => {
                return Err(DownloadFailure::Cancelled);
            }
            Err(DownloadError::WeRead(WeReadError::Api {
                code: -2063,
                message,
            })) => {
                if !pending_versions.is_empty() {
                    continue;
                }
                if !refetched_toc {
                    refetched_toc = true;
                    let fresh_toc = match client.chapter_infos(&params.book_id) {
                        Ok(fresh_toc) => fresh_toc,
                        Err(_error) if cancel.load(Ordering::SeqCst) => {
                            return Err(DownloadFailure::Cancelled);
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let fresh_versions: Vec<String> = candidate_book_versions(None, &fresh_toc)
                        .into_iter()
                        .filter(|v| !tried_versions.contains(v))
                        .collect();
                    if !fresh_versions.is_empty() {
                        toc = fresh_toc;
                        let fresh_targets = collect_targets(&toc);
                        let fresh_plan = plan_download(&toc, &fresh_targets)?;
                        planned = fresh_plan.eligible.len();
                        request.chapters = fresh_targets;
                        pending_versions.extend(fresh_versions);
                        continue;
                    }
                }
                return Err(DownloadFailure::WeRead(WeReadError::Api {
                    code: -2063,
                    message,
                }));
            }
            Err(error) => return Err(error.into()),
        }
    };
    if cancel.load(Ordering::SeqCst) {
        return Err(DownloadFailure::Cancelled);
    }
    // Report the terminal count before the completion event so the shelf shows
    // 100% while anchors are indexed.
    report_progress(planned, planned);
    let mut map = PositionMap::from_spine(
        &params.book_id,
        &book_version,
        &outcome.spine,
        &toc.chapters,
    );
    let anchors = AnchorMap::from_epub(&outcome.epub_path, &map).unwrap_or_else(|error| {
        eprintln!(
            "aristotle: weread: no anchors for {}: {error}",
            params.book_id
        );
        AnchorMap {
            book_id: params.book_id.clone(),
            chapters: Vec::new(),
        }
    });
    map.apply_anchor_lengths(&anchors);
    // Final EPUB is atomically committed on `/mnt/us`; remove the temporary
    // chapter cache so `/var/local` returns to zero overhead.
    let _ = fs::remove_dir_all(&params.cache_dir);
    let renewed_credentials = client
        .credentials()
        .filter(|creds| {
            creds.access_token != initial_credentials.access_token
                || creds.refresh_token != initial_credentials.refresh_token
        })
        .cloned();
    Ok(DownloadedBook {
        positions: BookPositions { map, anchors },
        outcome,
        renewed_credentials,
    })
}

struct DownloadTransport<T: HttpTransport> {
    inner: T,
    cancel: Arc<AtomicBool>,
}

impl<T: HttpTransport> HttpTransport for DownloadTransport<T> {
    fn send(
        &self,
        request: inkread_core::sync::transport::HttpRequest,
    ) -> Result<inkread_core::sync::transport::HttpResponse, TransportError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(TransportError::Other("cancelled".to_string()));
        }
        self.inner.send(request)
    }
}

fn run_open(
    credentials: Credentials,
    policy: Policy,
    book_id: &str,
    map: &PositionMap,
    anchors: &AnchorMap,
    cancel: &AtomicBool,
) -> (Option<String>, Option<String>, Option<Credentials>) {
    if cancel.load(Ordering::SeqCst) {
        return (None, None, None);
    }
    let transport = ReadestTransport::with_timeout(policy, OPEN_TIMEOUT_SECS);
    let initial_credentials = credentials.clone();
    let mut client = WeReadClient::new(transport, credentials);
    let response = match client.get_progress(book_id) {
        Ok(response) => response,
        Err(error) => return (None, Some(super::error_message(&error)), None),
    };
    let renewed = client
        .credentials()
        .filter(|creds| {
            creds.access_token != initial_credentials.access_token
                || creds.refresh_token != initial_credentials.refresh_token
        })
        .cloned();
    let Some(cloud) = response.book.as_ref().and_then(|book| book.position()) else {
        return (
            None,
            Some(tr(Msg::WereadPositionUnavailable).to_string()),
            renewed,
        );
    };
    let coordinate = WeReadCoordinate {
        chapter_uid: cloud.chapter_uid,
        chapter_idx: cloud.chapter_idx,
        chapter_offset: cloud.chapter_offset,
    };
    match map.xpointer_for(coordinate, Some(anchors)) {
        Ok(target) => (Some(target.xpointer), None, renewed),
        Err(_) => (
            None,
            Some(tr(Msg::WereadPositionUnavailable).to_string()),
            renewed,
        ),
    }
}

pub(crate) fn jump_annotation(xpointer: &str) -> Annotation {
    let encoded = marks::encode(&aristotle_render::book::Position::XPointer {
        xpointer: xpointer.to_string(),
    });
    Annotation {
        id: String::new(),
        kind: AnnotationKind::Bookmark,
        text: None,
        page: None,
        location: None,
        position: Some((encoded.clone(), encoded)),
        created: None,
        modified: None,
        color: None,
        style: None,
        source: marks::SOURCE.to_string(),
        chapter: None,
        note: None,
    }
}

impl crate::weread::WereadState {
    /// Keep the shelf's copy of a book's download state in step.
    pub fn set_download_state(&mut self, book_id: &str, state: WereadDownloadState) {
        if let Some(book) = self.books.iter_mut().find(|book| book.id == book_id) {
            book.download = state;
        }
    }

    /// The jump target waiting for the next open of `hash`.
    pub fn take_open_jump(&mut self, hash: &str) -> Option<Annotation> {
        self.downloads.take_open_jump(hash)
    }

    /// The book whose scan should open it, if any.
    pub fn open_pending(&mut self) -> Option<String> {
        self.downloads.take_pending_open()
    }
}

impl App {
    /// Start downloading the WeRead book `book_id` on a worker.
    pub(crate) fn start_weread_download(
        &mut self,
        book_id: &str,
    ) -> Result<(), aristotle_render::plato_core::anyhow::Error> {
        self.weread.ensure_loaded(&self.net.paths.state);
        let Some(credentials) = self.weread.credentials() else {
            self.weread.message = Some(tr(Msg::WereadErrAuth).to_string());
            return self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, self.list_page());
        };
        let Some(book) = self.weread.books.iter().find(|book| book.id == book_id) else {
            return Ok(());
        };
        let (title, format) = (book.title.clone(), book.format.clone());
        match self.weread.downloads.start(
            &self.net.paths.state,
            &self.root,
            book_id,
            &title,
            &format,
            credentials,
            crate::net::Net::policy(),
        ) {
            Ok(()) => {
                self.weread
                    .set_download_state(book_id, WereadDownloadState::Downloading { percent: 0 });
                self.weread.message = Some(tr(Msg::WereadDownloadStarting).to_string());
            }
            Err(message) => {
                self.weread.message = Some(message);
            }
        }
        self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, self.list_page())
    }

    /// Cancel the download of `book_id`, if it is the running one.
    pub(crate) fn cancel_weread_download(&mut self, book_id: &str) {
        self.weread.downloads.cancel(book_id);
    }

    /// Open a downloaded card: read the cloud position, then use the
    /// existing book-open path with an XPointer jump.
    pub(crate) fn open_weread_book(
        &mut self,
        book_id: &str,
    ) -> Result<(), aristotle_render::plato_core::anyhow::Error> {
        self.weread.ensure_loaded(&self.net.paths.state);
        let indexed_hash = self
            .weread
            .downloads
            .hash_of(book_id)
            .filter(|hash| self.lib.book(hash).is_some());
        let credentials = self.weread.credentials();
        let action = self.weread.downloads.open(
            &self.net.paths.state,
            &self.root,
            book_id,
            credentials,
            crate::net::Net::policy(),
            indexed_hash.is_some(),
        );
        match action {
            OpenAction::NeedDownload => self.start_weread_download(book_id),
            OpenAction::AwaitingIndex { path } => {
                self.weread.downloads.set_pending_open(book_id);
                self.admit_explicit_file(&path);
                self.weread.message = Some(tr(Msg::WereadDownloadIngesting).to_string());
                self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, self.list_page())
            }
            OpenAction::Started => {
                self.weread.message = Some(tr(Msg::WereadPositionLoading).to_string());
                self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, self.list_page())
            }
            OpenAction::Busy => {
                self.weread.message = Some(tr(Msg::WereadErrDownloadBusy).to_string());
                self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, self.list_page())
            }
            OpenAction::NoCredentials => {
                self.weread.message = Some(tr(Msg::WereadErrAuth).to_string());
                self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, self.list_page())
            }
        }
    }

    /// Drain the WeRead workers between events.
    pub(crate) fn poll_weread(
        &mut self,
    ) -> Result<(), aristotle_render::plato_core::anyhow::Error> {
        let events = self.weread.downloads.poll();
        if !events.is_empty() {
            if let Some(message) = self.weread.downloads.message() {
                self.weread.message = Some(message.to_string());
            }
        }
        for event in events {
            match event {
                WereadEvent::Progress { book_id, percent } => {
                    let state = WereadDownloadState::Downloading { percent };
                    self.weread.set_download_state(&book_id, state);
                    if matches!(self.screen, Screen::WereadShelf(_)) {
                        if let Screen::WereadShelf(view) = &mut self.screen {
                            view.sync_download(&mut self.ui.tree, &book_id, state);
                        }
                        self.render_ui()?;
                    }
                }
                WereadEvent::CredentialsRefreshed(credentials) => {
                    self.weread.set_credentials(credentials);
                    self.weread.save();
                }
                WereadEvent::DownloadFinished {
                    book_id,
                    path,
                    message,
                } => {
                    if message.is_none() {
                        self.weread
                            .set_download_state(&book_id, WereadDownloadState::Downloaded);
                        self.admit_explicit_file(&path);
                    } else {
                        let state = self.weread.downloads.state(&book_id);
                        self.weread.set_download_state(&book_id, state);
                    }
                    if matches!(self.screen, Screen::WereadShelf(_)) {
                        self.show_weread_shelf(
                            aristotle_ui::tree::Refresh::Auto,
                            self.list_page(),
                        )?;
                    }
                }
                WereadEvent::OpenReady {
                    book_id,
                    hash,
                    xpointer,
                    message,
                } => {
                    if let Some(message) = message {
                        self.weread.message = Some(message);
                    }
                    if matches!(self.screen, Screen::WereadShelf(_)) {
                        if self.lib.book(&hash).is_some() {
                            self.open_book(&hash, None)?;
                        } else {
                            let path = self.weread.downloads.epub_path(&book_id);
                            self.weread.downloads.set_pending_open(&book_id);
                            self.admit_explicit_file(&path);
                        }
                        let _ = xpointer;
                    } else if matches!(&self.screen, Screen::Reader(r) if r.session.id == hash) {
                        let target_loc = xpointer.and_then(|xp| {
                            let Screen::Reader(r) = &mut self.screen else {
                                return None;
                            };
                            let pos = aristotle_render::book::Position::XPointer { xpointer: xp };
                            if r.session.has_book() {
                                r.session.book().location_of(&pos)
                            } else {
                                None
                            }
                        });
                        if let Some(loc) = target_loc {
                            self.jump_to(loc)?;
                            self.readest.manual_message =
                                Some(tr(Msg::ReaderWereadPullDone).to_string());
                        } else if let Some(msg) = self.weread.message.clone() {
                            self.readest.manual_message = Some(msg);
                        } else {
                            self.readest.manual_message =
                                Some(tr(Msg::WereadPositionUnavailable).to_string());
                        }
                        self.refresh_reader_sync_status()?;
                    }
                }
            }
        }
        Ok(())
    }

    /// A scan finished: open the WeRead book that was waiting for it.
    pub(crate) fn weread_open_pending(
        &mut self,
    ) -> Result<(), aristotle_render::plato_core::anyhow::Error> {
        let Some(book_id) = self.weread.open_pending() else {
            return Ok(());
        };
        if !matches!(self.screen, Screen::WereadShelf(_)) {
            return Ok(());
        }
        let indexed = self
            .weread
            .downloads
            .hash_of(&book_id)
            .is_some_and(|hash| self.lib.book(&hash).is_some());
        if !indexed {
            self.weread.message = Some(tr(Msg::WereadErrDownloadFailed).to_string());
            self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, self.list_page())?;
            return Ok(());
        }
        self.open_weread_book(&book_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkread_core::sync::transport::{HttpRequest, HttpResponse};
    use inkread_core::weread::mock::MockTransport;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    const FIXTURE_ZIP_B64: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/weread/chapter-package.zip.b64"
    ));

    fn decode_b64(input: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buffer = 0u32;
        let mut bits = 0u32;
        for byte in input.bytes() {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' | b'\n' | b'\r' | b' ' | b'\t' => continue,
                other => panic!("invalid base64 byte {other}"),
            };
            buffer = (buffer << 6) | u32::from(value);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        out
    }

    struct VersionRetryTransport {
        inner: MockTransport,
        zip: Vec<u8>,
        accept_version: String,
        toc_calls: Arc<AtomicUsize>,
        refetched_version: i64,
        tried_versions: Arc<Mutex<Vec<String>>>,
    }

    impl HttpTransport for VersionRetryTransport {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
            let url = request.url.clone();
            if url.contains("/book/chapterInfos") {
                let call = self.toc_calls.fetch_add(1, Ordering::SeqCst);
                let (book_ver, synckey) = if call == 0 {
                    (20, 30)
                } else {
                    (self.refetched_version, 30)
                };
                let body = serde_json::json!({
                    "data": [{
                        "bookId": "100001",
                        "synckey": synckey,
                        "book": { "version": book_ver },
                        "chapters": [
                            {"chapterUid": 1, "chapterIdx": 1, "title": "One",
                             "wordCount": 100, "price": 0, "paid": 0,
                             "files": ["Text/ch1.xhtml"]}
                        ]
                    }]
                });
                return Ok(HttpResponse::new(200, serde_json::to_vec(&body).unwrap()));
            }
            if url.contains("/book/getProgress") {
                let body = serde_json::json!({
                    "bookId": "100001",
                    "book": {
                        "appId": "synthetic-app",
                        "bookVersion": 10,
                        "chapterUid": 1,
                        "chapterIdx": 1,
                        "chapterOffset": 0
                    }
                });
                return Ok(HttpResponse::new(200, serde_json::to_vec(&body).unwrap()));
            }
            if url.contains("/book/chapterdownload") {
                let version = url
                    .split_once('?')
                    .and_then(|(_, q)| {
                        q.split('&')
                            .find_map(|pair| pair.strip_prefix("bookVersion="))
                    })
                    .unwrap_or_default()
                    .to_string();
                self.tried_versions.lock().unwrap().push(version.clone());
                if version != self.accept_version {
                    return Ok(HttpResponse::new(
                        200,
                        br#"{"errcode":-2063,"errmsg":"version expired"}"#.to_vec(),
                    ));
                }
                return Ok(HttpResponse::new(200, self.zip.clone())
                    .with_header("encryptkey", self.inner.envelope()));
            }
            self.inner.send(request)
        }
    }

    #[test]
    fn candidate_book_versions_deduplicates_in_priority_order() {
        let toc = ChapterInfos {
            book_id: "100001".into(),
            synckey: Some(30),
            book_version: Some(20),
            chapters: Vec::new(),
        };
        assert_eq!(candidate_book_versions(Some(10), &toc), ["10", "20", "30"]);
        assert_eq!(candidate_book_versions(Some(20), &toc), ["20", "30"]);
        let dup_toc = ChapterInfos {
            book_id: "100001".into(),
            synckey: Some(20),
            book_version: Some(20),
            chapters: Vec::new(),
        };
        assert_eq!(candidate_book_versions(Some(20), &dup_toc), ["20"]);
    }

    #[test]
    fn run_download_recovers_from_2063_via_candidates_and_single_toc_refetch() {
        let temp =
            std::env::temp_dir().join(format!("aristotle-2063-retry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let params = DownloadParams {
            book_id: "100001".to_string(),
            title: "Retry Book".to_string(),
            cache_dir: temp.join("cache"),
            epub_path: temp.join("100001.epub"),
            mapping_path: temp.join("books.json"),
        };
        let toc_calls = Arc::new(AtomicUsize::new(0));
        let tried = Arc::new(Mutex::new(Vec::new()));
        let transport = VersionRetryTransport {
            inner: MockTransport::new(),
            zip: decode_b64(FIXTURE_ZIP_B64),
            accept_version: "40".to_string(),
            toc_calls: toc_calls.clone(),
            refetched_version: 40,
            tried_versions: tried.clone(),
        };
        let creds = Credentials::new("1234567890", "access", "refresh", "dev");
        let cancel = Arc::new(AtomicBool::new(false));
        let downloaded = run_download(creds, &params, transport, cancel, |_, _| {})
            .expect("should recover on refetched TOC version 40");
        assert_eq!(downloaded.positions.map.book_version, "40");
        assert_eq!(toc_calls.load(Ordering::SeqCst), 2);
        assert_eq!(*tried.lock().unwrap(), vec!["10", "20", "30", "40"]);
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn run_download_stops_after_single_toc_refetch_when_2063_persists() {
        let temp =
            std::env::temp_dir().join(format!("aristotle-2063-exhaust-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let params = DownloadParams {
            book_id: "100001".to_string(),
            title: "Retry Book".to_string(),
            cache_dir: temp.join("cache"),
            epub_path: temp.join("100001.epub"),
            mapping_path: temp.join("books.json"),
        };
        let toc_calls = Arc::new(AtomicUsize::new(0));
        let tried = Arc::new(Mutex::new(Vec::new()));
        let transport = VersionRetryTransport {
            inner: MockTransport::new(),
            zip: decode_b64(FIXTURE_ZIP_B64),
            accept_version: "999".to_string(),
            toc_calls: toc_calls.clone(),
            refetched_version: 40,
            tried_versions: tried.clone(),
        };
        let creds = Credentials::new("1234567890", "access", "refresh", "dev");
        let cancel = Arc::new(AtomicBool::new(false));
        let err = match run_download(creds, &params, transport, cancel, |_, _| {}) {
            Ok(_) => panic!("expected -2063 error"),
            Err(err) => err,
        };
        assert!(matches!(
            err,
            DownloadFailure::WeRead(WeReadError::Api { code: -2063, .. })
        ));
        assert_eq!(toc_calls.load(Ordering::SeqCst), 2);
        assert_eq!(*tried.lock().unwrap(), vec!["10", "20", "30", "40"]);
        let _ = fs::remove_dir_all(&temp);
    }
}
