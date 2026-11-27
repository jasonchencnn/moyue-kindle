// SPDX-License-Identifier: AGPL-3.0-or-later
//! Nutstore (Jianguoyun) in the app: the account, the WebDAV folder browser,
//! streaming downloads into the library, and the per-book progress and notes
//! channel (contract §15).
//!
//! The wire work is `core::jianguoyun`'s and the sockets are `aristotle-net`'s;
//! this module owns what the app adds:
//!
//! - the account in `accounts.json` ([`JianguoyunSettings`]), written only
//!   after a `PROPFIND` on the endpoint proved the credentials work;
//! - the folder being browsed ([`JianguoyunState::current_dir`]), its listing
//!   and the last listing of every folder visited (`jianguoyun-library.json`
//!   in the library's data folder), so a reader without a network still sees
//!   the shelf they had;
//! - which books came from Nutstore (`jianguoyun-channel.json`), for the
//!   cover badge and for the reader menu's sync row;
//! - the streaming download of one book at a time, into
//!   `<userstore>/jianguoyun/`, cancellable, and admitted to the library by
//!   the hash `partialMD5` gives it;
//! - the two-way progress and notes sync of one book at a time, by hand from
//!   the reader menu, on opening a book and on closing or sleeping.
//!
//! Credentials never reach a log: [`JianguoyunSettings`] and
//! [`ui_login::JianguoyunLoginState`] redact the password in `Debug`, and this
//! module never prints a `JianguoyunConfig` nor an `Authorization` header.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aristotle_i18n::{tf, tr, Msg};
use aristotle_library::index::{Annotation, AnnotationKind, AnnotationTombstone, BookRecord};
use aristotle_library::{Library, Locator, Progress};
use aristotle_net::download;
use aristotle_net::jianguoyun::{
    download_book, DownloadError, DownloadedBook, JianguoyunTransport,
};
use aristotle_net::store::JianguoyunSettings;
use aristotle_net::Url;
use inkread_core::jianguoyun::{
    decide_progress, merge_book_sync, JianguoyunBookSync, JianguoyunClient, JianguoyunConfig,
    JianguoyunError, JianguoyunNote, JianguoyunProgress, NoteKind, ProgressDecision, RemoteEntry,
    SYNC_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};

use crate::device::Facts;
use crate::net::{self, Done};
use crate::{App, Flow, Screen};
use aristotle_render::plato_core::anyhow::Error;
use aristotle_ui::screens::jianguoyun_login as ui_login;
use aristotle_ui::screens::jianguoyun_shelf as ui_shelf;
use aristotle_ui::screens::library::{BookSource, ChannelVisibility};
use aristotle_ui::tree::Refresh;
use aristotle_ui::ui::UiEvent;

/// The last listing of every folder visited, in the library's data folder.
pub const CACHE_FILE: &str = "jianguoyun-library.json";
/// The books that came from Nutstore, and where each one came from.
pub const CHANNEL_HASHES_FILE: &str = "jianguoyun-channel.json";
/// The folder under the userstore the books are downloaded into, which is
/// also what [`crate::shelf::source_from_path`] reads for the badge.
pub const LOCAL_DIR: &str = "jianguoyun";
/// The largest book the channel accepts, matching every other downloader.
pub const MAX_BOOK: u64 = download::MAX_BOOK;
/// Free space to leave on the userstore after a download.
pub const KEEP_FREE: u64 = download::KEEP_FREE;
/// The `Progress::source` of a position that came from the Nutstore record.
pub const PROGRESS_SOURCE: &str = "jianguoyun";

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

fn channel_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CHANNEL_HASHES_FILE)
}

/// Whether the settings name a complete account.
///
/// The endpoint may be left empty (the official one), but the e-mail and the
/// application password are both needed for the Basic header.
pub fn configured(settings: &JianguoyunSettings) -> bool {
    !settings.username.trim().is_empty() && !settings.password.is_empty()
}

/// Percent-decode a URL path; invalid escapes are kept verbatim and `+` is
/// not a space in a path.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push(high << 4 | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// The path of the account root, decoded, without its trailing slash.
fn endpoint_path(config: &JianguoyunConfig) -> Option<String> {
    let url = config.normalized_base_url().ok()?;
    let url = Url::parse(&url).ok()?;
    let path = percent_decode(url.path());
    Some(path.trim_end_matches('/').to_string())
}

/// The path of `abs` relative to the account root, as the shelf keys a row.
///
/// `abs` is the decoded absolute path a `PROPFIND` returned; the root is the
/// endpoint the account is configured with, `remote_dir` included. A path
/// outside the root, or one that tries to leave it, is refused: the shelf only
/// ever shows what belongs to the account.
pub fn rel_of(config: &JianguoyunConfig, abs: &str) -> Option<String> {
    let base = endpoint_path(config)?;
    if base.is_empty() {
        return None;
    }
    let abs = abs.trim().trim_end_matches('/');
    let rest = abs.strip_prefix(&base)?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let rel = rest.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    if rel.split('/').any(|segment| {
        segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\\')
            || has_percent_escape(segment)
    }) {
        return None;
    }
    Some(rel.to_string())
}

fn has_percent_escape(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    let is_hex = |b: u8| (b as char).is_ascii_hexdigit();
    bytes
        .windows(3)
        .any(|w| w[0] == b'%' && is_hex(w[1]) && is_hex(w[2]))
}

/// The `net-allow` entry of an endpoint, when the policy accepts it.
///
/// The rules are the transport's (`sync-contract.md` §15.1): plain HTTP only
/// on loopback and the private IPv4 ranges, anything else over TLS. An
/// endpoint the transport would refuse for leaking the app password is not
/// written to the firewall either.
pub fn allow_entry(config: &JianguoyunConfig) -> Option<(String, &'static str)> {
    let url = config.normalized_base_url().ok()?;
    let url = Url::parse(&url).ok()?;
    if !aristotle_net::endpoint::is_allowed(&url) {
        return None;
    }
    Some((url.host_port(), aristotle_net::store::JIANGUOYUN_FEATURE))
}

// ---------------------------------------------------------------- store

/// The endpoint entries of every book downloaded or tracked, with the remote
/// path each one was downloaded from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRecords {
    /// Lowercase `partialMD5` of every book that came from Nutstore.
    #[serde(default)]
    pub hashes: BTreeSet<String>,
    /// Remote path, relative to the account root, of every downloaded book.
    #[serde(default)]
    pub downloads: BTreeMap<String, String>,
}

impl ChannelRecords {
    /// The hash downloaded from `rel`, if any.
    pub fn hash_of(&self, rel: &str) -> Option<&str> {
        self.downloads.get(rel).map(String::as_str)
    }

    /// The remote path `hash` was downloaded from, if it is known.
    pub fn path_of(&self, hash: &str) -> Option<&str> {
        let hash = hash.to_ascii_lowercase();
        self.downloads
            .iter()
            .find(|(_, value)| value.eq_ignore_ascii_case(&hash))
            .map(|(path, _)| path.as_str())
    }
}

/// Read the channel file, in either shape it has been written in: the current
/// object, or the bare array of hashes an earlier build wrote.
pub fn load_channel(path: &Path) -> ChannelRecords {
    let Ok(bytes) = fs::read(path) else {
        return ChannelRecords::default();
    };
    if let Ok(records) = serde_json::from_slice::<ChannelRecords>(&bytes) {
        return records.normalize();
    }
    match serde_json::from_slice::<Vec<String>>(&bytes) {
        Ok(list) => ChannelRecords {
            hashes: list
                .into_iter()
                .map(|hash| hash.to_ascii_lowercase())
                .collect(),
            downloads: BTreeMap::new(),
        },
        Err(_) => ChannelRecords::default(),
    }
}

impl ChannelRecords {
    fn normalize(mut self) -> Self {
        self.hashes = self
            .hashes
            .into_iter()
            .map(|hash| hash.to_ascii_lowercase())
            .collect();
        self
    }

    /// Write the file; a failure only costs the badge on the next start.
    pub fn save(&self, path: &Path) {
        let Ok(json) = serde_json::to_vec(self) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }
}

/// One entry of a cached listing: what the shelf draws, without the size a
/// folder does not have.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedEntry {
    pub rel: String,
    pub name: String,
    #[serde(default)]
    pub is_dir: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub format: String,
}

/// The last listing of every folder visited.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShelfCache {
    /// When the cached folders were last written.
    #[serde(default)]
    pub cached_at: i64,
    /// The entries of each folder, by its path relative to the account root;
    /// the root itself is the empty string.
    #[serde(default)]
    pub dirs: BTreeMap<String, Vec<CachedEntry>>,
}

/// Read the cache; a missing or damaged file is no cache.
pub fn load_cache(path: &Path) -> ShelfCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ShelfCache>(&bytes).ok())
        .unwrap_or_default()
}

impl ShelfCache {
    /// Write the cache for the next offline visit; a failure only costs it.
    pub fn save(&self, path: &Path) {
        let Ok(json) = serde_json::to_vec(self) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }

    /// The cached entries of `dir`, as the shelf draws them.
    pub fn entries(&self, dir: &str) -> Vec<ui_shelf::JianguoyunEntry> {
        let Some(entries) = self.dirs.get(dir) else {
            return Vec::new();
        };
        entries.iter().map(entry_of).collect()
    }
}

fn entry_of(cached: &CachedEntry) -> ui_shelf::JianguoyunEntry {
    if cached.is_dir {
        ui_shelf::JianguoyunEntry::folder(&cached.rel, &cached.name)
    } else {
        ui_shelf::JianguoyunEntry::book(&cached.rel, &cached.name, &cached.format, cached.size)
    }
}

// ---------------------------------------------------------------- state

/// The Nutstore channel as the app remembers it between screens and requests.
pub struct JianguoyunState {
    /// The folder on the screen, relative to the account root; the root is the
    /// empty string.
    pub current_dir: String,
    /// The entries of [`JianguoyunState::current_dir`], in the order the shelf
    /// draws them.
    pub entries: Vec<ui_shelf::JianguoyunEntry>,
    pub filter: ui_shelf::JianguoyunFilter,
    pub loading: bool,
    /// A failure or a hint, in the reader's language.
    pub message: Option<String>,
    /// When the folder on the screen was fetched.
    pub last_sync: Option<i64>,
    /// Identifies the current listing request.
    pub token: u64,
    /// Identifies the current download request.
    pub download_token: u64,
    /// Identifies the current account test request.
    pub login_token: u64,
    /// Identifies the current book sync request.
    pub sync_token: u64,
    /// Identifies the current book sync request per book, so concurrent
    /// syncs for different books (e.g., pending queue retry vs. open auto-pull)
    /// do not invalidate each other.
    pub book_sync_tokens: HashMap<String, u64>,
    /// The remote path of the download in flight.
    pub downloading: Option<String>,
    /// Bytes so far and the total, if the server sent one.
    pub download_progress: Option<(u64, Option<u64>)>,
    /// The flag that stops the download in flight.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The entry that failed last, and why.
    pub failed: Option<(String, String)>,
    /// Remote paths whose file finished downloading and is waiting for the
    /// background scan to admit it into the library index.
    pub indexing: HashSet<String>,
    /// The setup form.
    pub login: ui_login::JianguoyunLoginState,
    /// Lowercase hashes of the books that came from Nutstore.
    pub channel_hashes: HashSet<String>,
    /// The remote path each of those books was downloaded from.
    pub downloads: BTreeMap<String, String>,
    /// The entries of every folder seen, for the shelf cache.
    pub cache: ShelfCache,
    /// The `PROPFIND` entry behind each row of the current folder.
    remotes: HashMap<String, RemoteEntry>,
    loaded: bool,
}

impl Default for JianguoyunState {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for JianguoyunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JianguoyunState")
            .field("current_dir", &self.current_dir)
            .field("entries", &self.entries.len())
            .field("filter", &self.filter)
            .field("loading", &self.loading)
            .field("message", &self.message)
            .field("last_sync", &self.last_sync)
            .field("downloading", &self.downloading)
            .field("failed", &self.failed)
            .field("channel_hashes", &self.channel_hashes.len())
            .field("downloads", &self.downloads.len())
            .field("login", &self.login)
            .finish_non_exhaustive()
    }
}

impl JianguoyunState {
    pub fn new() -> Self {
        Self {
            current_dir: String::new(),
            entries: Vec::new(),
            filter: ui_shelf::JianguoyunFilter::All,
            loading: false,
            message: None,
            last_sync: None,
            token: 0,
            download_token: 0,
            login_token: 0,
            sync_token: 0,
            book_sync_tokens: HashMap::new(),
            downloading: None,
            download_progress: None,
            cancel: None,
            failed: None,
            indexing: HashSet::new(),
            login: ui_login::JianguoyunLoginState::new(),
            channel_hashes: HashSet::new(),
            downloads: BTreeMap::new(),
            cache: ShelfCache::default(),
            remotes: HashMap::new(),
            loaded: false,
        }
    }

    /// Read the channel file and the folder cache once, so the badges and the
    /// last shelf are there before the screen is opened.
    pub fn preload_cache(&mut self, data_dir: &Path) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.read_channel(data_dir);
        self.cache = load_cache(&cache_path(data_dir));
    }

    /// Read both files again: the account may have changed, or the cache may
    /// have been written by a download.
    pub fn reload_cache(&mut self, data_dir: &Path) {
        self.loaded = true;
        self.read_channel(data_dir);
        self.cache = load_cache(&cache_path(data_dir));
    }

    fn read_channel(&mut self, data_dir: &Path) {
        let records = load_channel(&channel_path(data_dir));
        self.channel_hashes = records.hashes.into_iter().collect();
        self.downloads = records.downloads;
    }

    /// The records as the file on disk keeps them.
    fn records(&self) -> ChannelRecords {
        ChannelRecords {
            hashes: self.channel_hashes.iter().cloned().collect(),
            downloads: self.downloads.clone(),
        }
    }

    /// The hash downloaded from `rel`, if any.
    pub fn hash_of(&self, rel: &str) -> Option<&str> {
        self.downloads.get(rel).map(String::as_str)
    }

    /// The remote path `hash` was downloaded from, if it is known.
    pub fn path_of(&self, hash: &str) -> Option<&str> {
        let hash = hash.to_ascii_lowercase();
        self.downloads
            .iter()
            .find(|(_, value)| value.eq_ignore_ascii_case(&hash))
            .map(|(path, _)| path.as_str())
    }

    /// Remember that `hash` came from Nutstore, downloaded from `rel`.
    pub fn record(&mut self, data_dir: &Path, rel: &str, hash: &str) {
        if hash.is_empty() {
            return;
        }
        let hash = hash.to_ascii_lowercase();
        self.channel_hashes.insert(hash.clone());
        if !rel.is_empty() {
            self.downloads.insert(rel.to_string(), hash);
        }
        self.records().save(&channel_path(data_dir));
    }

    /// Signing out forgets the books of the account with it.
    pub fn forget(&mut self, data_dir: &Path) {
        self.channel_hashes.clear();
        self.downloads.clear();
        self.cache = ShelfCache::default();
        self.entries.clear();
        self.remotes.clear();
        self.current_dir.clear();
        self.downloading = None;
        self.download_progress = None;
        self.failed = None;
        self.indexing.clear();
        self.last_sync = None;
        self.message = None;
        self.loading = false;
        self.book_sync_tokens.clear();
        let _ = fs::remove_file(channel_path(data_dir));
        let _ = fs::remove_file(cache_path(data_dir));
    }

    /// The `PROPFIND` entry behind the row at `rel`.
    pub fn remote(&self, rel: &str) -> Option<&RemoteEntry> {
        self.remotes.get(rel)
    }

    /// Replace the listing of `dir` with `entries`.
    ///
    /// The paths the server reports are absolute; the shelf keys its rows by
    /// the path relative to the account root, so an entry outside it is
    /// dropped rather than shown. The listing is cached as it is written, so
    /// the same folder opens offline.
    pub fn set_listing(
        &mut self,
        config: &JianguoyunConfig,
        dir: &str,
        entries: Vec<RemoteEntry>,
        cached_at: i64,
    ) {
        let mut rows = Vec::with_capacity(entries.len());
        let mut remotes = HashMap::with_capacity(entries.len());
        let mut cached = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(rel) = rel_of(config, &entry.path) else {
                continue;
            };
            let format = match &entry.format {
                Some(format) if !format.is_empty() => format.to_ascii_uppercase(),
                _ => entry
                    .name
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_ascii_uppercase())
                    .unwrap_or_default(),
            };
            let row = if entry.is_dir {
                ui_shelf::JianguoyunEntry::folder(&rel, &entry.name)
            } else {
                ui_shelf::JianguoyunEntry::book(&rel, &entry.name, &format, entry.size)
            };
            cached.push(CachedEntry {
                rel: rel.clone(),
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                size: entry.size,
                format: format.clone(),
            });
            remotes.insert(rel.clone(), entry);
            rows.push(row);
        }
        self.current_dir = dir.to_string();
        self.entries = rows;
        self.remotes = remotes;
        self.cache.cached_at = cached_at;
        self.cache.dirs.insert(dir.to_string(), cached);
    }

    /// Show the cached listing of `dir`; false when it was never fetched.
    pub fn show_cached(&mut self, dir: &str) -> bool {
        let entries = self.cache.entries(dir);
        if entries.is_empty() && !self.cache.dirs.contains_key(dir) {
            return false;
        }
        self.current_dir = dir.to_string();
        self.entries = entries;
        self.remotes.clear();
        true
    }

    /// Write the folder cache; a failure only costs the offline shelf.
    pub fn save_cache(&self, data_dir: &Path) {
        self.cache.save(&cache_path(data_dir));
    }

    /// One step up the tree; false at the root.
    pub fn go_up(&mut self) -> bool {
        let Some((parent, _)) = self.current_dir.rsplit_once('/') else {
            if self.current_dir.is_empty() {
                return false;
            }
            self.current_dir.clear();
            return true;
        };
        self.current_dir = parent.to_string();
        true
    }

    /// The percentage of the download in flight, when the total is known.
    pub fn percent(&self) -> u8 {
        let Some((bytes, Some(total))) = self.download_progress else {
            return 0;
        };
        if total == 0 {
            return 0;
        }
        ((bytes.min(total) * 100) / total).min(100) as u8
    }

    /// What the row at `rel` shows for its download.
    pub fn download_state(&self, rel: &str, lib: &Library) -> ui_shelf::JianguoyunDownloadState {
        if self.downloading.as_deref() == Some(rel) {
            return ui_shelf::JianguoyunDownloadState::Downloading {
                percent: self.percent(),
            };
        }
        if self.failed.as_ref().is_some_and(|(path, _)| path == rel) {
            return ui_shelf::JianguoyunDownloadState::Failed;
        }
        if self.indexing.contains(rel) {
            return ui_shelf::JianguoyunDownloadState::Downloaded;
        }
        if let Some(hash) = self.hash_of(rel) {
            if lib.book(hash).is_some() {
                return ui_shelf::JianguoyunDownloadState::Downloaded;
            }
        }
        ui_shelf::JianguoyunDownloadState::NotDownloaded
    }

    /// Bring every row's download control up to date with the library.
    pub fn sync_states(&mut self, lib: &Library) {
        let states: Vec<_> = self
            .entries
            .iter()
            .map(|entry| self.download_state(&entry.path, lib))
            .collect();
        for (entry, state) in self.entries.iter_mut().zip(states) {
            entry.download = state;
        }
    }

    /// The number of books in the folder on the screen.
    pub fn book_count(&self) -> usize {
        self.entries.iter().filter(|entry| !entry.is_dir).count()
    }
}

// ------------------------------------------------------- progress records

/// The sync record of one library book, for the Nutstore channel.
///
/// The record carries what the contract's §15.4 file holds: the position the
/// local library has, and every highlight, bookmark and tombstone. `device`
/// names the writer in the position so another device can tell the writers
/// apart; it may be empty.
pub fn record_from_book(rec: &BookRecord, remote_path: &str, device: &str) -> JianguoyunBookSync {
    let mut record = JianguoyunBookSync::new(rec.id.to_ascii_lowercase(), rec.meta.title.clone());
    record.author = rec
        .meta
        .authors
        .iter()
        .map(|author| author.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    record.remote_path = remote_path.to_string();
    record.progress = rec.progress.as_ref().map(|progress| {
        let xpointer = match &progress.locator {
            Locator::XPointer { xpointer } => Some(xpointer.clone()),
            _ => None,
        };
        JianguoyunProgress {
            xpointer,
            cfi: None,
            fraction: progress.fraction.clamp(0.0, 1.0) as f64,
            page: None,
            total_pages: None,
            chapter_title: None,
            updated_at_ms: progress.updated.max(0) * 1000,
            device_id: device.to_string(),
        }
    });
    record.notes = wire_notes(rec);
    record
}

/// The notes and highlights of `rec`, tombstones included.
///
/// A tombstone is written as a deleted note, as the contract wants: the
/// deletion has to travel to the other devices.
pub fn wire_notes(rec: &BookRecord) -> Vec<JianguoyunNote> {
    let mut notes: Vec<JianguoyunNote> = rec.annotations.iter().map(wire_note).collect();
    notes.extend(rec.annotation_tombstones.iter().map(|tombstone| {
        let deleted_at_ms = tombstone.deleted_at.max(0) * 1000;
        JianguoyunNote {
            id: tombstone.id.clone(),
            kind: NoteKind::Note,
            xpointer0: None,
            xpointer1: None,
            text: None,
            note: None,
            color: None,
            created_at_ms: deleted_at_ms,
            updated_at_ms: deleted_at_ms,
            deleted_at_ms: Some(deleted_at_ms),
        }
    }));
    notes
}

/// One library annotation on the wire.
fn wire_note(annotation: &Annotation) -> JianguoyunNote {
    let (xpointer0, xpointer1) = match &annotation.position {
        Some((start, end)) => (
            (!start.trim().is_empty()).then(|| start.clone()),
            (annotation.kind != AnnotationKind::Bookmark && !end.trim().is_empty())
                .then(|| end.clone()),
        ),
        None => (None, None),
    };
    let created_at_ms = annotation
        .created
        .as_deref()
        .and_then(crate::marks::parse_local_time)
        .map_or(0, |secs| secs * 1000);
    let updated_at_ms = annotation
        .modified
        .as_deref()
        .or(annotation.created.as_deref())
        .and_then(crate::marks::parse_local_time)
        .map_or(created_at_ms, |secs| secs * 1000);
    JianguoyunNote {
        id: annotation.id.clone(),
        kind: match annotation.kind {
            AnnotationKind::Bookmark => NoteKind::Bookmark,
            AnnotationKind::Highlight => NoteKind::Highlight,
            AnnotationKind::Note => NoteKind::Note,
        },
        xpointer0,
        xpointer1,
        text: annotation.text.clone(),
        note: annotation.note.clone(),
        color: annotation.color.clone(),
        created_at_ms,
        updated_at_ms,
        deleted_at_ms: None,
    }
}

/// The library annotation of a wire note.
pub fn annotation_from_note(note: &JianguoyunNote) -> Annotation {
    let position = match (&note.xpointer0, &note.xpointer1) {
        (Some(start), Some(end)) => Some((start.clone(), end.clone())),
        (Some(start), None) => Some((start.clone(), String::new())),
        _ => None,
    };
    Annotation {
        id: note.id.clone(),
        kind: match note.kind {
            NoteKind::Bookmark => AnnotationKind::Bookmark,
            NoteKind::Highlight => AnnotationKind::Highlight,
            NoteKind::Note => AnnotationKind::Note,
        },
        text: note.text.clone(),
        page: None,
        location: None,
        position,
        created: (note.created_at_ms > 0)
            .then(|| crate::marks::local_time(note.created_at_ms / 1000)),
        modified: (note.updated_at_ms > note.created_at_ms.max(0))
            .then(|| crate::marks::local_time(note.updated_at_ms / 1000)),
        color: note.color.clone(),
        style: None,
        source: PROGRESS_SOURCE.to_string(),
        chapter: None,
        note: note.note.clone(),
    }
}

/// Merge the remote notes into `rec`, under the contract's §15.4 rules.
///
/// The effective timestamp of a note is `max(updatedAtMs, deletedAtMs)`; the
/// newer one wins, a tombstone wins an exact tie, and an exact tie with the
/// same deletion state goes to the remote record so a round converges instead
/// of bouncing the same write between two devices. Returns whether anything
/// changed.
pub fn merge_notes_into(rec: &mut BookRecord, remote: &[JianguoyunNote]) -> bool {
    let mut changed = false;
    for note in remote {
        let remote_ms = note.effective_timestamp_ms();
        let local = rec
            .annotations
            .iter()
            .find(|annotation| annotation.id == note.id);
        let tombstone = rec
            .annotation_tombstones
            .iter()
            .find(|tombstone| tombstone.id == note.id);
        let local_ms = local
            .map(|annotation| {
                annotation
                    .modified
                    .as_deref()
                    .or(annotation.created.as_deref())
                    .and_then(crate::marks::parse_local_time)
                    .map_or(0, |secs| secs * 1000)
            })
            .or_else(|| tombstone.map(|tombstone| tombstone.deleted_at.max(0) * 1000))
            .unwrap_or(i64::MIN);
        let local_deleted = tombstone.is_some();
        let wins = if local.is_none() && tombstone.is_none() {
            true
        } else if remote_ms != local_ms {
            remote_ms > local_ms
        } else if note.is_deleted() != local_deleted {
            note.is_deleted()
        } else {
            true
        };
        if !wins {
            continue;
        }
        if local_deleted && note.is_deleted() {
            // Both sides deleted it: the newer deletion time is the one the
            // other devices merge on, so the record keeps it.
            if remote_ms > local_ms {
                if let Some(tombstone) = rec
                    .annotation_tombstones
                    .iter_mut()
                    .find(|tombstone| tombstone.id == note.id)
                {
                    tombstone.deleted_at = remote_ms / 1000;
                    changed = true;
                }
            }
            continue;
        }
        if note.is_deleted() {
            rec.annotations
                .retain(|annotation| annotation.id != note.id);
            rec.annotation_tombstones
                .retain(|tombstone| tombstone.id != note.id);
            rec.annotation_tombstones.push(AnnotationTombstone {
                id: note.id.clone(),
                deleted_at: note.effective_timestamp_ms() / 1000,
            });
            changed = true;
        } else {
            let had_tombstone = rec
                .annotation_tombstones
                .iter()
                .any(|tombstone| tombstone.id == note.id);
            rec.annotation_tombstones
                .retain(|tombstone| tombstone.id != note.id);
            let annotation = annotation_from_note(note);
            match rec
                .annotations
                .iter_mut()
                .find(|existing| existing.id == note.id)
            {
                Some(existing) => {
                    if *existing != annotation || had_tombstone {
                        *existing = annotation;
                        changed = true;
                    }
                }
                None => {
                    rec.annotations.push(annotation);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// The library progress a pulled record carries, when it has a position.
///
/// The locator of the local record is kept when the record has no XPointer:
/// a fraction is still a position the reader can be placed at, and replacing
/// an exact locator with a page index would lose it.
pub fn progress_from_record(
    record: &JianguoyunBookSync,
    current: Option<&Progress>,
) -> Option<Progress> {
    let progress = record.progress.as_ref()?;
    let locator = match progress.xpointer.as_deref().map(str::trim) {
        Some(xpointer) if !xpointer.is_empty() => Locator::XPointer {
            xpointer: xpointer.to_string(),
        },
        _ => current
            .map(|current| current.locator.clone())
            .unwrap_or(Locator::Page { index: 0 }),
    };
    Some(Progress {
        fraction: progress.fraction.clamp(0.0, 1.0) as f32,
        locator,
        updated: (progress.updated_at_ms / 1000).max(1),
        source: Some(PROGRESS_SOURCE.to_string()),
        page: None,
    })
}

// ------------------------------------------------------------- wording

/// The reader's words for a failed request.
pub fn error_message(error: &JianguoyunError) -> String {
    eprintln!("aristotle: jianguoyun: {error}");
    use inkread_core::jianguoyun::JianguoyunErrorKind as K;
    match error.kind() {
        K::Authentication | K::Permission => tr(Msg::JianguoyunErrAuth).to_string(),
        K::RateLimited => tr(Msg::JianguoyunErrRate).to_string(),
        K::Network => tr(Msg::JianguoyunErrNetwork).to_string(),
        K::NotFound | K::Conflict => tr(Msg::JianguoyunErrServer).to_string(),
        K::Configuration | K::InvalidInput => tr(Msg::JianguoyunErrServerUrl).to_string(),
        K::Protocol | K::Server => tr(Msg::JianguoyunErrServer).to_string(),
    }
}

/// The reader's words for a failed download: its own, not the listing's.
pub fn download_error_message(error: &DownloadError) -> String {
    match error {
        DownloadError::Config(error) => error_message(error),
        DownloadError::UnsupportedFormat | DownloadError::NotAFile => {
            tr(Msg::JianguoyunErrDownloadFormat).to_string()
        }
        DownloadError::Empty => tr(Msg::JianguoyunErrServer).to_string(),
        DownloadError::Cancelled => tr(Msg::JianguoyunDownloadFailed).to_string(),
        DownloadError::UnsafePath | DownloadError::InsecureUrl => {
            tr(Msg::JianguoyunErrServerUrl).to_string()
        }
        DownloadError::Io(io) if io.kind() == std::io::ErrorKind::StorageFull => {
            tr(Msg::JianguoyunErrDownloadStorage).to_string()
        }
        DownloadError::Io(_) => tr(Msg::JianguoyunErrDownloadStorage).to_string(),
        DownloadError::Http(http) => match http {
            aristotle_net::http::Error::Cancelled => tr(Msg::JianguoyunDownloadFailed).to_string(),
            aristotle_net::http::Error::NotAllowed(_) => tr(Msg::JianguoyunErrBlocked).to_string(),
            aristotle_net::http::Error::Connect(_) | aristotle_net::http::Error::Timeout => {
                tr(Msg::JianguoyunErrNetwork).to_string()
            }
            aristotle_net::http::Error::Unauthorized => tr(Msg::JianguoyunErrAuth).to_string(),
            aristotle_net::http::Error::Status(status, _) if *status == 401 || *status == 403 => {
                tr(Msg::JianguoyunErrAuth).to_string()
            }
            aristotle_net::http::Error::Status(status, _) if *status == 429 || *status == 503 => {
                tr(Msg::JianguoyunErrRate).to_string()
            }
            _ => tr(Msg::JianguoyunErrDownloadStorage).to_string(),
        },
    }
}

// ------------------------------------------------------------------ app

impl App {
    /// Whether the reader set a Nutstore account up.
    pub(crate) fn jianguoyun_configured(&self) -> bool {
        self.net
            .accounts
            .jianguoyun
            .as_ref()
            .is_some_and(configured)
    }

    /// The account, its client configuration and the device id the records
    /// name, or `None` when the channel has no usable account.
    fn jianguoyun_target(&self) -> Option<(JianguoyunConfig, String)> {
        let settings = self.net.accounts.jianguoyun.as_ref()?;
        if !configured(settings) {
            return None;
        }
        Some((
            settings.to_core_config(),
            self.net.accounts.device_id.clone(),
        ))
    }

    /// The remote path a book in the library came from, when it is known.
    fn jianguoyun_remote_path(&self, book: &str) -> String {
        self.jianguoyun
            .path_of(book)
            .unwrap_or_default()
            .to_string()
    }

    /// Open Nutstore: the shelf of the last folder when the account is set
    /// up, the setup screen otherwise.
    pub(crate) fn open_jianguoyun(&mut self) -> Result<(), Error> {
        self.jianguoyun.preload_cache(self.lib.data_dir());
        if !self.jianguoyun_configured() {
            return self.show_jianguoyun_login(Refresh::Full);
        }
        // The last folder the reader was in, from the cache, before the
        // network answers: the shelf never opens empty.
        if self.jianguoyun.entries.is_empty() {
            let dir = self.jianguoyun.current_dir.clone();
            self.jianguoyun.show_cached(&dir);
        }
        self.show_jianguoyun_shelf(Refresh::Clean, 0)?;
        if !self.jianguoyun.loading {
            self.refresh_jianguoyun();
            self.show_jianguoyun_shelf(Refresh::Auto, 0)?;
        }
        Ok(())
    }

    /// Rebuild the Nutstore shelf from the state in memory.
    pub(crate) fn show_jianguoyun_shelf(
        &mut self,
        refresh: Refresh,
        page: usize,
    ) -> Result<(), Error> {
        self.jianguoyun.preload_cache(self.lib.data_dir());
        let is_configured = self.jianguoyun_configured();
        if !is_configured {
            self.jianguoyun.entries.clear();
            self.jianguoyun.loading = false;
        }
        self.jianguoyun.sync_states(&self.lib);
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let account = self
            .net
            .accounts
            .jianguoyun
            .as_ref()
            .filter(|settings| configured(settings))
            .map(|settings| settings.username.clone());
        let last_sync = self.jianguoyun.last_sync.map(crate::marks::local_time);
        let current_dir = self.jianguoyun.current_dir.clone();
        let filter = self.jianguoyun.filter;
        let loading = self.jianguoyun.loading;
        let book_count = self.jianguoyun.book_count();
        let message = self.jianguoyun.message.clone();
        let entries = self.jianguoyun.entries.clone();
        let channels = ChannelVisibility {
            readest: self.prefs.enable_readest,
            weread: self.prefs.enable_weread,
            jianguoyun: self.prefs.enable_jianguoyun,
        };
        let view = ui_shelf::JianguoyunShelfView {
            status,
            configured: is_configured,
            account: account.as_deref(),
            current_dir: &current_dir,
            book_count,
            local_books_count: self.lib.books().len(),
            last_sync: last_sync.as_deref(),
            filter,
            loading,
            message: message.as_deref(),
            entries: &entries,
        };
        let width = self.ui.bounds().w;
        let (tree, shelf) = ui_shelf::JianguoyunShelfView::build_for_width_with_channels(
            &mut self.ui.text,
            &self.styles,
            &view,
            width,
            channels,
        );
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        if page > 0 {
            self.ui.tree.set_page(shelf.list, page);
            self.ui.layout();
        }
        shelf.sync(&mut self.ui.tree);
        self.screen = Screen::JianguoyunShelf(Box::new(shelf));
        self.render_ui()
    }

    /// The setup screen, with the account that is saved already filled in.
    ///
    /// Without an account the form is empty: after a sign-out it must not keep
    /// the password of the account that just left.
    fn fill_jianguoyun_login(&mut self) {
        let login = &mut self.jianguoyun.login;
        match self.net.accounts.jianguoyun.clone() {
            Some(settings) => {
                login.server_url = settings.server_url;
                login.email = settings.username;
                login.password = settings.password;
                login.remote_dir = settings.remote_dir;
            }
            None => {
                login.server_url = ui_login::DEFAULT_SERVER_URL.to_string();
                login.email.clear();
                login.password.clear();
                login.remote_dir.clear();
            }
        }
        login.keyboard.committed = login.server_url.clone();
        login.keyboard.pinyin.clear();
        login.keyboard.all_candidates.clear();
        login.keyboard.cand_page = 0;
        login.is_saving = false;
        login.error_message = None;
    }

    /// Show the setup screen.
    pub(crate) fn show_jianguoyun_login(&mut self, refresh: Refresh) -> Result<(), Error> {
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let mut tree = aristotle_ui::tree::Tree::new();
        let root = ui_login::build_jianguoyun_login_screen(
            &mut tree,
            &self.styles,
            &self.jianguoyun.login,
            &status,
        );
        tree.set_root(root);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::JianguoyunLogin;
        self.render_ui()
    }

    /// Open the setup screen, filling the form with the saved account.
    pub(crate) fn open_jianguoyun_login(&mut self) -> Result<(), Error> {
        self.fill_jianguoyun_login();
        self.show_jianguoyun_login(Refresh::Full)
    }

    /// Ask for the folder on the screen on a worker thread.
    pub(crate) fn refresh_jianguoyun(&mut self) {
        if self.jianguoyun.loading {
            return;
        }
        let Some((config, _)) = self.jianguoyun_target() else {
            return;
        };
        self.jianguoyun.loading = true;
        self.jianguoyun.message = None;
        self.jianguoyun.token = self.jianguoyun.token.wrapping_add(1);
        let token = self.jianguoyun.token;
        let dir_path = self.jianguoyun.current_dir.clone();
        // The first request of a visit: make sure the device's firewall
        // allows the endpoint it will dial.
        self.net.reconcile_allow_entries();
        let policy = net::Net::policy();
        self.net.spawn("jianguoyun-list", move |_| {
            let client = JianguoyunClient::new(config, JianguoyunTransport::new(policy));
            let result = client.list_dir(&dir_path);
            Done::JianguoyunListed {
                token,
                dir_path,
                result,
            }
        });
    }

    /// A folder listing arrived: keep it, cache it, or fall back to the cache.
    pub(crate) fn on_jianguoyun_listed(
        &mut self,
        token: u64,
        dir_path: String,
        result: Result<Vec<RemoteEntry>, JianguoyunError>,
    ) -> Result<(), Error> {
        if token != self.jianguoyun.token || dir_path != self.jianguoyun.current_dir {
            return Ok(());
        }
        self.jianguoyun.loading = false;
        let Some((config, _)) = self.jianguoyun_target() else {
            self.jianguoyun.message = Some(tr(Msg::JianguoyunNotConfiguredTitle).to_string());
            return self.show_current_jianguoyun_screen();
        };
        match result {
            Ok(entries) => {
                let now = now_secs();
                self.jianguoyun
                    .set_listing(&config, &dir_path, entries, now);
                self.jianguoyun.last_sync = Some(now);
                self.jianguoyun.message = None;
                self.jianguoyun.failed = None;
                self.jianguoyun.save_cache(self.lib.data_dir());
            }
            Err(error) => {
                self.jianguoyun.message = Some(error_message(&error));
                // Offline: the folder the reader had is still there.
                if self.jianguoyun.show_cached(&dir_path) {
                    self.jianguoyun.last_sync = self.jianguoyun.last_sync.or_else(|| {
                        let cache_time = self.jianguoyun.cache.cached_at;
                        (cache_time > 0).then_some(cache_time)
                    });
                }
            }
        }
        self.show_current_jianguoyun_screen()
    }

    /// Rebuild whichever Nutstore screen is on the display, if any.
    fn show_current_jianguoyun_screen(&mut self) -> Result<(), Error> {
        match &self.screen {
            Screen::JianguoyunShelf(_) => {
                let page = self.list_page();
                self.show_jianguoyun_shelf(Refresh::Auto, page)
            }
            Screen::JianguoyunLogin => self.show_jianguoyun_login(Refresh::Auto),
            _ => Ok(()),
        }
    }

    /// Enter the folder at `rel`.
    pub(crate) fn enter_jianguoyun_dir(&mut self, rel: &str) -> Result<(), Error> {
        if !self.jianguoyun_configured() {
            return self.open_jianguoyun_login();
        }
        let rel = rel.trim_matches('/').to_string();
        if rel.is_empty() {
            return Ok(());
        }
        self.jianguoyun.current_dir = rel.clone();
        self.jianguoyun.failed = None;
        self.jianguoyun.message = None;
        // What this device saw last time, before the network answers.
        if !self.jianguoyun.show_cached(&rel) {
            self.jianguoyun.entries.clear();
        }
        self.jianguoyun.sync_states(&self.lib);
        self.show_jianguoyun_shelf(Refresh::Clean, 0)?;
        self.refresh_jianguoyun();
        Ok(())
    }

    /// Back to the parent folder.
    pub(crate) fn go_up_jianguoyun(&mut self) -> Result<(), Error> {
        if !self.jianguoyun.go_up() {
            return Ok(());
        }
        let dir = self.jianguoyun.current_dir.clone();
        if !self.jianguoyun.show_cached(&dir) {
            self.jianguoyun.entries.clear();
        }
        self.jianguoyun.sync_states(&self.lib);
        self.show_jianguoyun_shelf(Refresh::Clean, 0)?;
        self.refresh_jianguoyun();
        Ok(())
    }

    /// Download the entry at `rel`, or open it when it is already a book of
    /// the library.
    pub(crate) fn open_jianguoyun_entry(&mut self, rel: &str) -> Result<(), Error> {
        if let Some(hash) = self.jianguoyun.hash_of(rel).map(str::to_string) {
            if self.lib.book(&hash).is_some() {
                self.cancel_jianguoyun_on_leave();
                return self.open_book(&hash, None);
            }
        }
        if self.jianguoyun.indexing.contains(rel) {
            return Ok(());
        }
        self.start_jianguoyun_download(rel)
    }

    /// Stream the entry at `rel` into `<userstore>/jianguoyun/`.
    fn start_jianguoyun_download(&mut self, rel: &str) -> Result<(), Error> {
        let Some(entry) = self.jianguoyun.remote(rel).cloned() else {
            return Ok(());
        };
        if entry.is_dir {
            return Ok(());
        }
        if self.jianguoyun.downloading.as_deref() == Some(rel)
            || self.jianguoyun.indexing.contains(rel)
        {
            return Ok(());
        }
        if let Some(hash) = self.jianguoyun.hash_of(rel).map(str::to_string) {
            if self.lib.book(&hash).is_some() {
                self.cancel_jianguoyun_on_leave();
                return self.open_book(&hash, None);
            }
        }
        let Some((config, _)) = self.jianguoyun_target() else {
            return self.open_jianguoyun_login();
        };
        let free = aristotle_hal::system::storage(&self.root)
            .map(|storage| storage.free)
            .unwrap_or(MAX_BOOK);
        if free <= KEEP_FREE {
            self.jianguoyun.message = Some(tr(Msg::JianguoyunErrDownloadStorage).to_string());
            return self.show_jianguoyun_shelf(Refresh::Auto, self.list_page());
        }

        self.jianguoyun.download_token = self.jianguoyun.download_token.wrapping_add(1);
        let download_token = self.jianguoyun.download_token;
        let remote_path = rel.to_string();
        self.jianguoyun.downloading = Some(remote_path.clone());
        self.jianguoyun.download_progress = Some((0, None));
        self.jianguoyun.failed = None;
        self.jianguoyun.message = None;

        let limit = MAX_BOOK.min(free.saturating_sub(KEEP_FREE));
        let policy = net::Net::policy();
        let dir = self.root.join(LOCAL_DIR);
        if let Err(error) = fs::create_dir_all(&dir) {
            self.jianguoyun.downloading = None;
            self.jianguoyun.download_progress = None;
            let message = download_error_message(&DownloadError::Io(error));
            self.jianguoyun.message = Some(message.clone());
            self.jianguoyun.failed = Some((remote_path, message));
            return self.show_jianguoyun_shelf(Refresh::Auto, self.list_page());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.jianguoyun.cancel = Some(cancel.clone());

        self.net.spawn("jianguoyun-download", move |tx| {
            let mut last = 0u64;
            let mut report = |bytes: u64, total: Option<u64>| {
                let step = total.map_or(1 << 20, |total| (total / 20).max(1));
                if bytes >= last + step {
                    last = bytes;
                    let _ = tx.send(Done::JianguoyunProgress {
                        token: download_token,
                        remote_path: remote_path.clone(),
                        bytes,
                        total,
                    });
                }
            };
            let result = download_book(policy, &config, &entry, &dir, limit, &cancel, &mut report);
            Done::JianguoyunDownloaded {
                token: download_token,
                remote_path,
                result,
            }
        });
        self.show_jianguoyun_shelf(Refresh::Auto, self.list_page())
    }

    /// Bytes of the download in flight.
    pub(crate) fn on_jianguoyun_progress(
        &mut self,
        token: u64,
        remote_path: String,
        bytes: u64,
        total: Option<u64>,
    ) -> Result<(), Error> {
        if token != self.jianguoyun.download_token
            || self.jianguoyun.downloading.as_deref() != Some(remote_path.as_str())
        {
            return Ok(());
        }
        let previous = self.jianguoyun.percent();
        self.jianguoyun.download_progress = Some((bytes, total));
        let percent = self.jianguoyun.percent();
        // One redraw per whole percent, as the cloud shelf does: the report
        // arrives every twentieth of the file.
        if percent != previous && matches!(self.screen, Screen::JianguoyunShelf(_)) {
            let page = self.list_page();
            self.show_jianguoyun_shelf(Refresh::Auto, page)?;
        }
        Ok(())
    }

    /// A download finished: index the new file, or show its failure.
    pub(crate) fn on_jianguoyun_downloaded(
        &mut self,
        token: u64,
        remote_path: String,
        result: Result<DownloadedBook, DownloadError>,
    ) -> Result<(), Error> {
        if token != self.jianguoyun.download_token
            || self.jianguoyun.downloading.as_deref() != Some(remote_path.as_str())
        {
            return Ok(());
        }
        self.jianguoyun.downloading = None;
        self.jianguoyun.cancel = None;
        self.jianguoyun.download_progress = None;
        match result {
            Ok(book) => {
                self.jianguoyun
                    .record(self.lib.data_dir(), &remote_path, &book.book_hash);
                let title = book
                    .path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.jianguoyun.failed = None;
                self.jianguoyun.message = Some(tf!(Msg::CloudDownloadedReady, title = title));
                // If an identical book (same partialMD5) is already indexed,
                // discard the duplicate file so the library never ends up with
                // two copies of the same book.
                if let Some(existing) = self.lib.book(&book.book_hash) {
                    let existing_abs = self.lib.absolute_path(existing);
                    if existing_abs.as_deref() != Some(book.path.as_path()) {
                        let _ = fs::remove_file(&book.path);
                    }
                } else {
                    // Mark as indexing immediately so the shelf shows
                    // "Downloaded" and refuses a duplicate download while the
                    // background scan admits the file.
                    self.jianguoyun.indexing.insert(remote_path);
                    self.admit_explicit_file(&book.path);
                }
            }
            Err(error) => {
                let message = download_error_message(&error);
                self.jianguoyun.message = Some(message.clone());
                // A cancel is the reader's own choice: the row goes back to
                // "download", it is not marked as a failure.
                if matches!(
                    error,
                    DownloadError::Cancelled
                        | DownloadError::Http(aristotle_net::http::Error::Cancelled)
                ) {
                    self.jianguoyun.failed = None;
                } else {
                    self.jianguoyun.failed = Some((remote_path, message));
                }
            }
        }
        match &self.screen {
            Screen::JianguoyunShelf(_) => {
                let page = self.list_page();
                self.show_jianguoyun_shelf(Refresh::Auto, page)
            }
            _ => Ok(()),
        }
    }

    /// Stop the download in flight, if it is the one at `rel`.
    pub(crate) fn cancel_jianguoyun_download(&mut self, rel: &str) {
        if self.jianguoyun.downloading.as_deref() != Some(rel) {
            return;
        }
        self.cancel_jianguoyun_on_leave();
    }

    /// Leaving the shelf stops its download: the reader asked to go elsewhere.
    pub(crate) fn cancel_jianguoyun_on_leave(&self) {
        if let Some(cancel) = &self.jianguoyun.cancel {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Events on the setup screen: the keyboard, the fields and the submit.
    pub(crate) fn on_jianguoyun_login_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        let outcome = self.jianguoyun.login.handle_action(id);
        match outcome {
            ui_login::JianguoyunLoginOutcome::None => {}
            ui_login::JianguoyunLoginOutcome::Changed => {
                self.show_jianguoyun_login(Refresh::Auto)?;
            }
            ui_login::JianguoyunLoginOutcome::Cancel => {
                if self.jianguoyun_configured() {
                    self.jianguoyun.message = None;
                    self.show_jianguoyun_shelf(Refresh::Clean, 0)?;
                    self.refresh_jianguoyun();
                } else {
                    self.show_library(Refresh::Full)?;
                }
            }
            ui_login::JianguoyunLoginOutcome::Submit {
                server_url,
                email,
                password,
                remote_dir,
            } => {
                self.submit_jianguoyun_login(server_url, email, password, remote_dir)?;
            }
        }
        Ok(Flow::Continue)
    }

    /// Test the credentials the reader typed before anything is saved.
    pub(crate) fn submit_jianguoyun_login(
        &mut self,
        server_url: String,
        email: String,
        password: String,
        remote_dir: String,
    ) -> Result<(), Error> {
        let settings = JianguoyunSettings::new(server_url, email, password, remote_dir);
        let config = settings.to_core_config();
        let endpoint_ok = config
            .normalized_base_url()
            .ok()
            .and_then(|url| Url::parse(&url).ok())
            .is_some_and(|url| aristotle_net::endpoint::is_allowed(&url));
        if config.normalized_base_url().is_err() || !endpoint_ok {
            self.jianguoyun.login.is_saving = false;
            self.jianguoyun.login.error_message = Some(tr(Msg::JianguoyunErrServerUrl).to_string());
            return self.show_jianguoyun_login(Refresh::Auto);
        }
        // The endpoint has to be reachable before the request that needs it,
        // on a `scoped` device that has never seen this account.
        if let Some(entry) = allow_entry(&config) {
            self.net
                .ensure_allow_entries(std::iter::once(entry).collect());
        }
        self.jianguoyun.login.is_saving = true;
        self.jianguoyun.login.error_message = None;
        self.jianguoyun.login_token = self.jianguoyun.login_token.wrapping_add(1);
        let token = self.jianguoyun.login_token;
        let policy = net::Net::policy();
        self.net.spawn("jianguoyun-login", move |_| {
            let client = JianguoyunClient::new(config, JianguoyunTransport::new(policy));
            let result = client.test_connection();
            Done::JianguoyunTestLogin {
                token,
                settings,
                result,
            }
        });
        self.show_jianguoyun_login(Refresh::Auto)
    }

    /// The credentials worked (or not): save the account and open its shelf.
    pub(crate) fn on_jianguoyun_test_login(
        &mut self,
        token: u64,
        settings: JianguoyunSettings,
        result: Result<(), JianguoyunError>,
    ) -> Result<(), Error> {
        if token != self.jianguoyun.login_token {
            return Ok(());
        }
        self.jianguoyun.login.is_saving = false;
        match result {
            Ok(()) => {
                let before = self.net.accounts.allow_entries();
                self.net.accounts.jianguoyun = Some(settings);
                self.net.save();
                self.net.update_allowlist(&before);
                self.net.reconcile_allow_entries();
                self.jianguoyun.login.error_message = None;
                self.jianguoyun.current_dir.clear();
                self.jianguoyun.entries.clear();
                self.jianguoyun.failed = None;
                self.jianguoyun.message = None;
                self.jianguoyun.reload_cache(self.lib.data_dir());
                self.show_jianguoyun_shelf(Refresh::Clean, 0)?;
                self.refresh_jianguoyun();
                self.show_jianguoyun_shelf(Refresh::Auto, 0)
            }
            Err(error) => {
                self.jianguoyun.login.error_message = Some(error_message(&error));
                self.show_jianguoyun_login(Refresh::Auto)
            }
        }
    }

    /// Sign out: forget the credentials, the endpoint's firewall entry and the
    /// books of the account.
    pub(crate) fn sign_out_jianguoyun(&mut self) -> Result<(), Error> {
        self.cancel_jianguoyun_on_leave();
        let before = self.net.accounts.allow_entries();
        self.net.accounts.jianguoyun = None;
        self.net.save();
        self.net.update_allowlist(&before);
        self.jianguoyun.forget(self.lib.data_dir());
        self.fill_jianguoyun_login();
        self.show_jianguoyun_login(Refresh::Full)
    }

    /// Events on the Nutstore shelf.
    pub(crate) fn on_jianguoyun_shelf_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_shelf::action as a;
        if let UiEvent::Action { id, .. } = &ev {
            let id = *id;
            let row = if let Screen::JianguoyunShelf(view) = &self.screen {
                view.entry_at(id).map(str::to_string)
            } else {
                None
            };
            if let Some(rel) = row {
                if (a::DOWNLOAD..a::DOWNLOAD + a::ROW_STRIDE).contains(&id) {
                    self.start_jianguoyun_download(&rel)?;
                    return Ok(Flow::Continue);
                }
                if (a::CANCEL..a::CANCEL + a::ROW_STRIDE).contains(&id) {
                    self.cancel_jianguoyun_download(&rel);
                    return Ok(Flow::Continue);
                }
                if (a::OPEN..a::OPEN + a::ROW_STRIDE).contains(&id) {
                    self.open_jianguoyun_entry(&rel)?;
                    return Ok(Flow::Continue);
                }
                if (a::ENTER..a::ENTER + a::ROW_STRIDE).contains(&id) {
                    self.enter_jianguoyun_dir(&rel)?;
                    return Ok(Flow::Continue);
                }
            }
        }
        match ev {
            UiEvent::Swipe(dir) => {
                let mut handled = false;
                if let Screen::JianguoyunShelf(view) = &mut self.screen {
                    handled = view.handle_swipe(&mut self.ui.tree, dir);
                }
                if handled {
                    let page = self.list_page();
                    self.show_jianguoyun_shelf(Refresh::Auto, page)?;
                }
            }
            UiEvent::Action { id, .. } => match id {
                a::LOCAL => {
                    self.cancel_jianguoyun_on_leave();
                    self.show_library(Refresh::Clean)?;
                }
                a::CLOUD => {
                    self.cancel_jianguoyun_on_leave();
                    self.open_cloud()?;
                }
                a::WEREAD => {
                    self.cancel_jianguoyun_on_leave();
                    self.open_weread()?;
                }
                a::SYNC => {
                    self.cancel_jianguoyun_on_leave();
                    let page = self.list_page();
                    self.refresh_jianguoyun();
                    self.show_jianguoyun_shelf(Refresh::Clean, page)?;
                }
                a::CONFIGURE => {
                    self.cancel_jianguoyun_on_leave();
                    self.open_jianguoyun_login()?;
                }
                a::LOGIN => {
                    self.cancel_jianguoyun_on_leave();
                    if self.jianguoyun_configured() {
                        self.jianguoyun.message = None;
                        self.show_jianguoyun_shelf(Refresh::Clean, 0)?;
                        self.refresh_jianguoyun();
                    } else {
                        self.open_jianguoyun_login()?;
                    }
                }
                a::LOGOUT => {
                    self.sign_out_jianguoyun()?;
                }
                a::PARENT => {
                    self.cancel_jianguoyun_on_leave();
                    self.go_up_jianguoyun()?;
                }
                id if (a::FILTER..a::FILTER + ui_shelf::JianguoyunFilter::ALL.len() as u32)
                    .contains(&id) =>
                {
                    let index = (id - a::FILTER) as usize;
                    self.jianguoyun.filter = ui_shelf::JianguoyunFilter::ALL[index];
                    self.show_jianguoyun_shelf(Refresh::Auto, 0)?;
                }
                _ => {
                    let mut handled = false;
                    if let Screen::JianguoyunShelf(view) = &mut self.screen {
                        handled = view.handle(&mut self.ui.tree, id);
                    }
                    if handled {
                        let page = self.list_page();
                        self.show_jianguoyun_shelf(Refresh::Auto, page)?;
                    }
                }
            },
            _ => {}
        }
        Ok(Flow::Continue)
    }

    // ------------------------------------------------------- book sync

    /// The sync record of a book in the library.
    fn jianguoyun_record_of(&self, book: &str, device: &str) -> Option<JianguoyunBookSync> {
        let rec = self.lib.book(book)?;
        let remote_path = self.jianguoyun_remote_path(book);
        Some(record_from_book(rec, &remote_path, device))
    }

    /// Manually pull the open book's Nutstore position and notes.
    pub(crate) fn jianguoyun_pull_book_now(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &self.screen else {
            return Ok(());
        };
        let book = r.session.id.clone();
        self.jianguoyun_sync_book(book, true, false)
    }

    /// Manually push the open book's position and notes to Nutstore.
    pub(crate) fn jianguoyun_push_book_now(&mut self) -> Result<(), Error> {
        let (book, progress) = {
            let Screen::Reader(r) = &self.screen else {
                return Ok(());
            };
            let now = aristotle_library::library::now();
            (r.session.id.clone(), r.session.progress(now))
        };
        // Persist the live page position into the library index before
        // building the wire payload so manual push always uploads the page
        // currently on screen.
        self.lib.set_progress(&book, progress);
        self.jianguoyun_sync_book(book, true, true)
    }

    /// Start a book sync on a worker thread.
    ///
    /// An automatic pull merges what the account has with what this device has
    /// and pushes the merge when it differs, so one round converges. A manual
    /// pull fetches the remote record directly so a newer local timestamp
    /// never suppresses jumping to the remote position. A push uploads what
    /// this device has without reading the account's copy: the reader asked
    /// for their position to win.
    fn jianguoyun_sync_book(
        &mut self,
        book: String,
        manual: bool,
        push_only: bool,
    ) -> Result<(), Error> {
        if !self.prefs.enable_jianguoyun || !self.jianguoyun_configured() {
            if manual {
                self.readest.manual_message =
                    Some(tr(Msg::JianguoyunNotConfiguredTitle).to_string());
                return self.refresh_reader_sync_status();
            }
            return Ok(());
        }
        if self.is_weread_book(&book) {
            if manual {
                self.readest.manual_message = Some(tr(Msg::JianguoyunErrAuth).to_string());
                return self.refresh_reader_sync_status();
            }
            return Ok(());
        }
        let device = self.net.accounts.device_id();
        let Some(local) = self.jianguoyun_record_of(&book, &device) else {
            return Ok(());
        };
        if local.book_hash.is_empty() {
            return Ok(());
        }
        self.net.reconcile_allow_entries();
        let Some((config, _)) = self.jianguoyun_target() else {
            return Ok(());
        };
        self.jianguoyun.sync_token = self.jianguoyun.sync_token.wrapping_add(1);
        let token = self.jianguoyun.sync_token;
        self.jianguoyun.book_sync_tokens.insert(book.clone(), token);
        if manual {
            self.readest.manual_message = Some(
                tr(if push_only {
                    Msg::ReaderSyncPushRunning
                } else {
                    Msg::ReaderSyncPullRunning
                })
                .to_string(),
            );
        }
        let policy = net::Net::policy();
        let job_book = book.clone();
        self.net.spawn("jianguoyun-book", move |_| {
            let client = JianguoyunClient::new(config, JianguoyunTransport::new(policy));
            let result = if push_only {
                client.push_book_sync(&local).map(|()| Some(local.clone()))
            } else if manual {
                client.pull_book_sync(&local.book_hash)
            } else {
                client.sync_book(&local).map(|(merged, _)| Some(merged))
            };
            Done::JianguoyunBookSynced {
                book: job_book,
                token,
                manual,
                push_only,
                result,
            }
        });
        if manual {
            self.refresh_reader_sync_status()?;
        }
        Ok(())
    }

    /// A book sync finished: merge it in, or queue the push for later.
    pub(crate) fn on_jianguoyun_book_synced(
        &mut self,
        book: String,
        token: u64,
        manual: bool,
        push_only: bool,
        result: Result<Option<JianguoyunBookSync>, JianguoyunError>,
    ) -> Result<(), Error> {
        let active_for_book = self
            .jianguoyun
            .book_sync_tokens
            .get(&book)
            .copied()
            .unwrap_or(self.jianguoyun.sync_token);
        if token != active_for_book {
            return Ok(());
        }
        match result {
            Ok(record) => {
                if let Some(settings) = self.net.accounts.jianguoyun.as_mut() {
                    settings.last_sync_time = Some(now_secs() * 1000);
                    settings.dequeue_push(&book);
                }
                self.net.save();
                let applied = if let Some(record) = record {
                    self.apply_jianguoyun_record(&book, &record, !push_only, manual)?
                } else {
                    false
                };
                if manual {
                    self.readest.manual_message = Some(
                        tr(if push_only {
                            Msg::ReaderSyncPushDone
                        } else if applied {
                            Msg::ReaderSyncPullDone
                        } else {
                            Msg::ReaderSyncSame
                        })
                        .to_string(),
                    );
                }
            }
            Err(error) => {
                let message = error_message(&error);
                if push_only {
                    if let Some(settings) = self.net.accounts.jianguoyun.as_mut() {
                        settings.enqueue_push(&book);
                    }
                    self.net.save();
                }
                if manual {
                    self.readest.manual_message = Some(message);
                } else if push_only {
                    eprintln!("aristotle: jianguoyun: automatic push of {book} queued");
                }
            }
        }
        if manual {
            self.refresh_reader_sync_status()?;
        }
        Ok(())
    }

    /// Merge a sync record into the library and jump the open reader when
    /// appropriate. Returns whether notes or the reading position changed.
    fn apply_jianguoyun_record(
        &mut self,
        book: &str,
        record: &JianguoyunBookSync,
        with_progress: bool,
        manual: bool,
    ) -> Result<bool, Error> {
        let Some((mut changed, before)) = self
            .lib
            .book_mut(book)
            .map(|rec| (merge_notes_into(rec, &record.notes), rec.progress.clone()))
        else {
            return Ok(false);
        };
        let mut target_progress = None;
        if with_progress {
            if let Some(remote_progress) = record.progress.as_ref() {
                if let Some(progress) = progress_from_record(record, before.as_ref()) {
                    if manual || before.as_ref() != Some(&progress) {
                        target_progress = Some(remote_progress.clone());
                    }
                    if before.as_ref() != Some(&progress) {
                        self.lib.set_progress(book, progress);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.saver.save(&self.lib);
            self.reload_marks();
        }
        let mut jumped = false;
        if let Some(remote_progress) = target_progress {
            jumped = self.jianguoyun_jump_to_progress(book, &remote_progress, manual)?;
        }
        Ok(changed || jumped)
    }

    /// Move the open book to `remote_progress`.
    ///
    /// Resolves the remote record's own `xpointer` first and falls back to
    /// `fraction` so books without an XPointer (or cross-device records
    /// carrying fraction only) never get stuck on the local page's old
    /// XPointer. Manual pulls may jump either forward or backward; automatic
    /// pulls on open only jump forward so a reader is never pulled back
    /// silently.
    fn jianguoyun_jump_to_progress(
        &mut self,
        book: &str,
        remote_progress: &JianguoyunProgress,
        allow_backward: bool,
    ) -> Result<bool, Error> {
        if self.asleep.is_some() {
            return Ok(false);
        }
        if !matches!(&self.screen, Screen::Reader(r) if r.session.id == book) {
            return Ok(false);
        }
        self.settle_restyle()?;
        let (location, current) = {
            let Some(r) = self.reading() else {
                return Ok(false);
            };
            let by_xpointer = remote_progress
                .xpointer
                .as_deref()
                .map(str::trim)
                .filter(|xpointer| !xpointer.is_empty())
                .and_then(|xpointer| {
                    let pos = aristotle_render::book::Position::XPointer {
                        xpointer: xpointer.to_string(),
                    };
                    r.session.page_of(&pos)
                });
            let by_fraction = || {
                let pos = aristotle_render::book::Position::Fraction(
                    remote_progress.fraction.clamp(0.0, 1.0) as f32,
                );
                r.session.page_of(&pos)
            };
            let Some(location) = by_xpointer.or_else(by_fraction) else {
                return Ok(false);
            };
            (location, r.session.current)
        };
        if location == current {
            return Ok(false);
        }
        if !allow_backward && location < current {
            return Ok(false);
        }
        // `jump_to` updates `session.current`, closes the menu/overlay, renders
        // the target page to the e-ink panel, updates progress, and prerenders
        // neighbours.
        self.jump_to(location)?;
        if let Screen::Reader(r) = &mut self.screen {
            r.turned = true;
        }
        Ok(true)
    }

    /// Pull when a book opens, so this device starts where the account says.
    pub(crate) fn jianguoyun_auto_pull_on_open(&mut self, book_id: &str) {
        if !self.prefs.enable_jianguoyun
            || !self.jianguoyun_configured()
            || self.book_source(book_id) != BookSource::Jianguoyun
        {
            return;
        }
        let _ = self.jianguoyun_sync_book(book_id.to_string(), false, false);
    }

    /// Push when a book closes or the device sleeps.
    ///
    /// A push that cannot run now (another request is using the network) is
    /// queued for the next one, as a failed push is.
    pub(crate) fn jianguoyun_auto_push_on_leave(&mut self, book_id: &str) {
        self.jianguoyun_push_when_leaving(book_id, false);
    }

    /// Push when the device goes to sleep.
    ///
    /// The push is queued first: the radio goes down moments after this, and
    /// a request cut in half must be retried when the book is opened again.
    pub(crate) fn jianguoyun_auto_push_on_sleep(&mut self, book_id: &str) {
        self.jianguoyun_push_when_leaving(book_id, true);
    }

    fn jianguoyun_push_when_leaving(&mut self, book_id: &str, before_sleep: bool) {
        if !self.prefs.enable_jianguoyun {
            return;
        }
        let Some(settings) = self.net.accounts.jianguoyun.as_ref() else {
            return;
        };
        if !configured(settings) || !settings.auto_push {
            return;
        }
        if before_sleep {
            if let Some(settings) = self.net.accounts.jianguoyun.as_mut() {
                settings.enqueue_push(book_id);
            }
            self.net.save();
        }
        if before_sleep && self.net.busy() {
            return;
        }
        let _ = self.jianguoyun_sync_book(book_id.to_string(), false, true);
    }

    /// Retry the books whose push failed earlier, when the network works.
    pub(crate) fn retry_pending_jianguoyun(&mut self) {
        let Some(settings) = self.net.accounts.jianguoyun.as_ref() else {
            return;
        };
        if !configured(settings) || !settings.auto_push {
            return;
        }
        let open_book = match &self.screen {
            Screen::Reader(r) => Some(r.session.id.clone()),
            _ => None,
        };
        let pending = settings.pending_queue.clone();
        for book in pending {
            // The book being opened right now already runs `sync_book` via
            // `jianguoyun_auto_pull_on_open` (which merges and pushes back if
            // local is newer), so don't race a blind push against it.
            if open_book.as_deref() == Some(book.as_str()) {
                continue;
            }
            if self.lib.book(&book).is_some() {
                let _ = self.jianguoyun_sync_book(book, false, true);
            } else if let Some(settings) = self.net.accounts.jianguoyun.as_mut() {
                settings.dequeue_push(&book);
            }
        }
        self.net.save();
    }
}

/// The decision a worker's merge made, for the tests and for the next device
/// that reads the record: whether the position on the account is the one to
/// show.
pub fn remote_is_newer(
    local: Option<&JianguoyunProgress>,
    remote: Option<&JianguoyunProgress>,
) -> bool {
    matches!(decide_progress(local, remote), ProgressDecision::PullRemote)
}

/// The merged record of `local` and `remote`, without a transport.
pub fn merged_record(
    local: &JianguoyunBookSync,
    remote: Option<&JianguoyunBookSync>,
) -> JianguoyunBookSync {
    match remote {
        Some(remote) => merge_book_sync(local, remote),
        None => local.clone(),
    }
}

/// The version a record written by this build carries.
pub fn schema_version() -> u32 {
    SYNC_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use aristotle_library::index::FileStamp;

    fn endpoint(base: &str, remote_dir: &str) -> JianguoyunConfig {
        JianguoyunConfig::new(base, "reader@example.com", "app-password", remote_dir)
    }

    fn record(id: &str, path: &str) -> BookRecord {
        BookRecord {
            id: id.into(),
            file: FileStamp {
                path: path.into(),
                size: 1,
                mtime: 0,
                mtime_ns: 0,
            },
            format: aristotle_library::Format::Epub,
            meta: Default::default(),
            cover: None,
            added: 0,
            last_opened: None,
            status: Default::default(),
            progress: None,
            settings: Default::default(),
            annotations: Vec::new(),
            annotation_tombstones: Vec::new(),
        }
    }

    #[test]
    fn relative_paths_stay_inside_the_account() {
        let cfg = endpoint("https://dav.jianguoyun.com/dav/", "");
        assert_eq!(
            rel_of(&cfg, "/dav/Books/三体.epub").as_deref(),
            Some("Books/三体.epub")
        );
        assert_eq!(rel_of(&cfg, "/dav/Books").as_deref(), Some("Books"));
        assert_eq!(rel_of(&cfg, "/dav/"), None, "the root is no entry");
        assert_eq!(rel_of(&cfg, "/davish/x.epub"), None);
        assert_eq!(rel_of(&cfg, "/other/x.epub"), None);
        assert_eq!(rel_of(&cfg, "/dav/../etc/passwd"), None);
        assert_eq!(rel_of(&cfg, "/dav/Books\\x.epub"), None);

        let nested = endpoint("https://dav.jianguoyun.com/dav/", "书库");
        assert_eq!(
            rel_of(&nested, "/dav/书库/我的 书.epub").as_deref(),
            Some("我的 书.epub")
        );
        assert_eq!(rel_of(&nested, "/dav/Books/x.epub"), None);
        assert_eq!(
            rel_of(&cfg, "/dav/%E4%B8%89%E4%BD%93.epub"),
            None,
            "the PROPFIND path is already decoded"
        );
    }

    #[test]
    fn percent_decoding_keeps_invalid_escapes_and_plus() {
        assert_eq!(
            percent_decode("/dav/%E4%B8%89%E4%BD%93.epub"),
            "/dav/三体.epub"
        );
        assert_eq!(percent_decode("/a+b/%zz"), "/a+b/%zz");
    }

    #[test]
    fn the_allowlist_entry_follows_the_transport_rules() {
        let official = endpoint("https://dav.jianguoyun.com/dav/", "");
        assert_eq!(
            allow_entry(&official),
            Some((
                "dav.jianguoyun.com:443".to_string(),
                aristotle_net::store::JIANGUOYUN_FEATURE
            ))
        );
        // Plain HTTP is only for a loopback or a private address.
        assert!(allow_entry(&endpoint("http://127.0.0.1:8080/dav/", "")).is_some());
        assert!(allow_entry(&endpoint("http://dav.jianguoyun.com/dav/", "")).is_none());
        assert!(allow_entry(&endpoint("not a url", "")).is_none());
    }

    #[test]
    fn the_channel_file_reads_both_shapes_and_survives_damage() {
        let dir = crate::testdir("jgy-channel");
        let path = channel_path(&dir);
        assert_eq!(load_channel(&path), ChannelRecords::default());

        // The bare array an earlier build wrote.
        fs::write(&path, br#"["ABC123", "def456"]"#).unwrap();
        let records = load_channel(&path);
        assert!(records.hashes.contains("abc123"));
        assert!(records.hashes.contains("def456"));
        assert!(records.downloads.is_empty());

        // The object this build writes.
        let mut records = ChannelRecords::default();
        records.hashes.insert("abc123".into());
        records
            .downloads
            .insert("Books/dune.epub".into(), "abc123".into());
        records.save(&path);
        let loaded = load_channel(&path);
        assert_eq!(loaded, records);
        assert_eq!(loaded.hash_of("Books/dune.epub"), Some("abc123"));
        assert_eq!(loaded.path_of("ABC123"), Some("Books/dune.epub"));
        assert_eq!(loaded.path_of("nope"), None);

        fs::write(&path, b"not json").unwrap();
        assert_eq!(load_channel(&path), ChannelRecords::default());
    }

    #[test]
    fn the_shelf_cache_round_trips_and_lists_a_folder() {
        let dir = crate::testdir("jgy-cache");
        let path = cache_path(&dir);
        assert!(ShelfCache::default().dirs.is_empty());

        let mut cache = ShelfCache::default();
        cache.cached_at = 42;
        cache.dirs.insert(
            "Books".into(),
            vec![
                CachedEntry {
                    rel: "Books/Novels".into(),
                    name: "Novels".into(),
                    is_dir: true,
                    size: 0,
                    format: String::new(),
                },
                CachedEntry {
                    rel: "Books/dune.epub".into(),
                    name: "dune.epub".into(),
                    is_dir: false,
                    size: 1024,
                    format: "EPUB".into(),
                },
            ],
        );
        cache.save(&path);
        let loaded = load_cache(&path);
        assert_eq!(loaded.cached_at, 42);
        let entries = loaded.entries("Books");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].format, "EPUB");
        assert_eq!(entries[1].size, 1024);
        assert!(loaded.entries("Elsewhere").is_empty());
        // A damaged file is no cache, not a failure.
        fs::write(&path, b"{").unwrap();
        assert!(load_cache(&path).dirs.is_empty());
    }

    #[test]
    fn a_listing_keeps_only_what_belongs_to_the_account() {
        let mut state = JianguoyunState::new();
        let config = endpoint("https://dav.jianguoyun.com/dav/", "");
        let entry = |path: &str, name: &str, is_dir: bool| RemoteEntry {
            path: path.into(),
            name: name.into(),
            is_dir,
            size: if is_dir { 0 } else { 2048 },
            etag: None,
            last_modified: None,
            content_type: None,
            format: if is_dir { None } else { Some("epub".into()) },
        };
        let entries = vec![
            entry("/dav/Books", "Books", true),
            entry("/dav/三体.epub", "三体.epub", false),
            entry("/other/escape.epub", "escape.epub", false),
        ];
        state.set_listing(&config, "", entries, 7);
        assert_eq!(state.entries.len(), 2);
        assert!(state.entries[0].is_dir);
        assert_eq!(state.entries[1].path, "三体.epub");
        assert_eq!(state.entries[1].format, "EPUB");
        assert_eq!(state.entries[1].size, 2048);
        assert_eq!(state.book_count(), 1);
        assert!(state.remote("三体.epub").is_some());
        assert!(state.remote("Books").is_some_and(|entry| entry.is_dir));
        assert!(state.remote("escape.epub").is_none());
        assert_eq!(state.cache.cached_at, 7);

        // The cached folder opens again without a network.
        let mut offline = JianguoyunState::new();
        offline.cache = state.cache.clone();
        assert!(offline.show_cached(""));
        assert_eq!(offline.entries.len(), 2);
        assert!(!offline.show_cached("Books/Nothing"));
    }

    #[test]
    fn going_up_stops_at_the_root() {
        let mut state = JianguoyunState::new();
        assert!(!state.go_up(), "the root has no parent");
        state.current_dir = "Books".into();
        assert!(state.go_up());
        assert_eq!(state.current_dir, "");
        state.current_dir = "Books/Novels/Dune".into();
        assert!(state.go_up());
        assert_eq!(state.current_dir, "Books/Novels");
        assert!(state.go_up());
        assert_eq!(state.current_dir, "Books");
        assert!(state.go_up());
        assert_eq!(state.current_dir, "");
        assert!(!state.go_up());
    }

    #[test]
    fn the_download_control_follows_the_library() {
        let dir = crate::testdir("jgy-states");
        let mut state = JianguoyunState::new();
        let cfg = endpoint("https://dav.jianguoyun.com/dav/", "");
        state.set_listing(
            &cfg,
            "",
            vec![RemoteEntry {
                path: "/dav/dune.epub".into(),
                name: "dune.epub".into(),
                is_dir: false,
                size: 10,
                etag: None,
                last_modified: None,
                content_type: None,
                format: Some("epub".into()),
            }],
            0,
        );
        let lib = Library::open(&dir).unwrap();
        assert_eq!(
            state.download_state("dune.epub", &lib),
            ui_shelf::JianguoyunDownloadState::NotDownloaded
        );

        state.downloading = Some("dune.epub".into());
        state.download_progress = Some((50, Some(200)));
        assert_eq!(state.percent(), 25);
        assert_eq!(
            state.download_state("dune.epub", &lib),
            ui_shelf::JianguoyunDownloadState::Downloading { percent: 25 }
        );
        // A total the server did not send leaves the bar unpainted.
        state.download_progress = Some((50, None));
        assert_eq!(state.percent(), 0);

        state.downloading = None;
        state.download_progress = None;
        state.failed = Some(("dune.epub".into(), "no".into()));
        assert_eq!(
            state.download_state("dune.epub", &lib),
            ui_shelf::JianguoyunDownloadState::Failed
        );

        // The book is on the device: the row opens it.
        state.failed = None;
        state.downloads.insert(
            "dune.epub".into(),
            "0123456789abcdef0123456789abcdef".into(),
        );
        assert_eq!(
            state.download_state("dune.epub", &lib),
            ui_shelf::JianguoyunDownloadState::NotDownloaded,
            "downloaded, but not in the library"
        );
        let mut index = lib.index().clone();
        let mut book = record("0123456789abcdef0123456789abcdef", "jianguoyun/dune.epub");
        book.meta.title = "Dune".into();
        index.books.push(book);
        let mut lib = Library::open(&dir).unwrap();
        lib.restore_index(index);
        assert_eq!(
            state.download_state("dune.epub", &lib),
            ui_shelf::JianguoyunDownloadState::Downloaded
        );
        state.sync_states(&lib);
        assert_eq!(
            state.entries[0].download,
            ui_shelf::JianguoyunDownloadState::Downloaded
        );
    }

    #[test]
    fn a_record_carries_the_position_and_the_notes() {
        let mut rec = record("0123456789abcdef0123456789abcdef", "jianguoyun/dune.epub");
        rec.meta.title = "Dune".into();
        rec.meta.authors = vec![aristotle_library::metadata::Author::new(
            "Frank Herbert",
            Some("Herbert, Frank"),
        )];
        rec.progress = Some(Progress {
            fraction: 0.25,
            locator: Locator::XPointer {
                xpointer: "/body/DocFragment[2]/body/p[3]/text().0".into(),
            },
            updated: 1_700_000_000,
            source: Some("aristotle".into()),
            page: None,
        });
        rec.annotations.push(Annotation {
            id: "note-1".into(),
            kind: AnnotationKind::Highlight,
            text: Some("Fear is the mind-killer".into()),
            page: None,
            location: None,
            position: Some(("xp0".into(), "xp1".into())),
            created: Some(crate::marks::local_time(1_700_000_000)),
            modified: None,
            color: Some("yellow".into()),
            style: None,
            source: "aristotle".into(),
            chapter: None,
            note: Some("litany".into()),
        });
        rec.annotation_tombstones.push(AnnotationTombstone {
            id: "note-2".into(),
            deleted_at: 1_700_000_100,
        });

        let sync = record_from_book(&rec, "Books/dune.epub", "KINDLE-OASIS-2");
        assert_eq!(sync.version, SYNC_SCHEMA_VERSION);
        assert_eq!(sync.book_hash, "0123456789abcdef0123456789abcdef");
        assert_eq!(sync.title, "Dune");
        assert_eq!(sync.author, "Frank Herbert");
        assert_eq!(sync.remote_path, "Books/dune.epub");
        let progress = sync.progress.as_ref().unwrap();
        assert_eq!(progress.fraction, 0.25);
        assert_eq!(progress.updated_at_ms, 1_700_000_000_000);
        assert_eq!(progress.device_id, "KINDLE-OASIS-2");
        assert_eq!(
            progress.xpointer.as_deref(),
            Some("/body/DocFragment[2]/body/p[3]/text().0")
        );

        let highlight = sync.notes.iter().find(|note| note.id == "note-1").unwrap();
        assert_eq!(highlight.kind, NoteKind::Highlight);
        assert_eq!(highlight.xpointer0.as_deref(), Some("xp0"));
        assert_eq!(highlight.xpointer1.as_deref(), Some("xp1"));
        assert_eq!(highlight.text.as_deref(), Some("Fear is the mind-killer"));
        assert_eq!(highlight.note.as_deref(), Some("litany"));
        assert!(!highlight.is_deleted());
        let tombstone = sync.notes.iter().find(|note| note.id == "note-2").unwrap();
        assert!(tombstone.is_deleted());
        assert_eq!(tombstone.deleted_at_ms, Some(1_700_000_100_000));
        assert_eq!(tombstone.effective_timestamp_ms(), 1_700_000_100_000);

        // The record round-trips back into the library's shape.
        let mut empty = record("0123456789abcdef0123456789abcdef", "x");
        empty.progress = None;
        assert!(merge_notes_into(&mut empty, &sync.notes));
        assert_eq!(empty.annotations.len(), 1);
        assert_eq!(empty.annotation_tombstones.len(), 1);
        let annotation = &empty.annotations[0];
        assert_eq!(annotation.kind, AnnotationKind::Highlight);
        assert_eq!(annotation.position, Some(("xp0".into(), "xp1".into())));
        assert_eq!(annotation.color.as_deref(), Some("yellow"));
        assert_eq!(annotation.source, PROGRESS_SOURCE);

        // The progress becomes a library position with its locator.
        let progress = progress_from_record(&sync, None).unwrap();
        assert!((progress.fraction - 0.25).abs() < 1e-6);
        assert_eq!(progress.updated, 1_700_000_000);
        assert_eq!(progress.source.as_deref(), Some(PROGRESS_SOURCE));
        assert!(matches!(
            progress.locator,
            Locator::XPointer { ref xpointer } if xpointer.starts_with("/body/DocFragment[2]")
        ));
        // Without an XPointer the local locator is kept.
        let mut fraction_only = sync.clone();
        fraction_only.progress.as_mut().unwrap().xpointer = None;
        let kept = progress_from_record(
            &fraction_only,
            Some(&Progress {
                fraction: 0.1,
                locator: Locator::Page { index: 12 },
                updated: 1,
                source: None,
                page: None,
            }),
        )
        .unwrap();
        assert_eq!(kept.locator, Locator::Page { index: 12 });
    }

    #[test]
    fn notes_merge_by_timestamp_and_keep_the_tombstone() {
        let mut rec = record("0123456789abcdef0123456789abcdef", "x");
        rec.annotations.push(Annotation {
            id: "a".into(),
            kind: AnnotationKind::Highlight,
            text: Some("local".into()),
            page: None,
            location: None,
            position: None,
            created: Some(crate::marks::local_time(1_000)),
            modified: None,
            color: None,
            style: None,
            source: "aristotle".into(),
            chapter: None,
            note: None,
        });

        let mut newer = JianguoyunNote::highlight("a", "xp0", "xp1", "remote", 2_000_000);
        newer.updated_at_ms = 2_000_000;
        assert!(merge_notes_into(&mut rec, &[newer.clone()]));
        assert_eq!(rec.annotations.len(), 1);
        assert_eq!(rec.annotations[0].text.as_deref(), Some("remote"));
        // The same note again changes nothing.
        assert!(!merge_notes_into(&mut rec, &[newer]));

        // An older remote note loses.
        let older = JianguoyunNote::highlight("a", "xp0", "xp1", "stale", 1);
        assert!(!merge_notes_into(&mut rec, &[older]));
        assert_eq!(rec.annotations[0].text.as_deref(), Some("remote"));

        // A tombstone at the same millisecond wins over the note.
        let effective = rec
            .annotations
            .iter()
            .find(|annotation| annotation.id == "a")
            .and_then(|annotation| {
                annotation
                    .modified
                    .as_deref()
                    .or(annotation.created.as_deref())
                    .and_then(crate::marks::parse_local_time)
                    .map(|secs| secs * 1000)
            })
            .unwrap();
        let tombstone = JianguoyunNote {
            id: "a".into(),
            kind: NoteKind::Highlight,
            xpointer0: None,
            xpointer1: None,
            text: None,
            note: None,
            color: None,
            created_at_ms: effective,
            updated_at_ms: effective,
            deleted_at_ms: Some(effective),
        };
        assert!(merge_notes_into(&mut rec, &[tombstone.clone()]));
        assert!(rec.annotations.is_empty());
        assert_eq!(rec.annotation_tombstones.len(), 1);
        assert_eq!(rec.annotation_tombstones[0].id, "a");
        // The tombstone stays a tombstone, and does not grow a duplicate.
        assert!(!merge_notes_into(&mut rec, &[tombstone]));
        assert_eq!(rec.annotation_tombstones.len(), 1);
    }

    #[test]
    fn the_progress_decision_follows_the_contract() {
        let local = JianguoyunProgress {
            xpointer: Some("local".into()),
            cfi: None,
            fraction: 0.2,
            page: None,
            total_pages: None,
            chapter_title: None,
            updated_at_ms: 1_000,
            device_id: "a".into(),
        };
        let mut remote = local.clone();
        remote.updated_at_ms = 2_000;
        remote.xpointer = Some("remote".into());
        assert!(remote_is_newer(Some(&local), Some(&remote)));
        assert!(!remote_is_newer(Some(&remote), Some(&local)));
        // A tie is not "newer": the merge keeps the remote record.
        assert!(!remote_is_newer(Some(&local), Some(&local)));
        // The larger fraction wins a tie.
        let mut farther = local.clone();
        farther.fraction = 0.5;
        assert!(remote_is_newer(Some(&local), Some(&farther)));
        assert!(!remote_is_newer(None, None));

        // The merge keeps the newer position and the union of the notes.
        let mut left = JianguoyunBookSync::new("0123456789abcdef0123456789abcdef", "Dune");
        left.progress = Some(local.clone());
        left.notes
            .push(JianguoyunNote::highlight("a", "x0", "x1", "one", 1_000));
        let mut right = left.clone();
        right.progress = Some(remote.clone());
        right.notes = vec![
            JianguoyunNote::highlight("a", "x0", "x1", "one", 1_000),
            JianguoyunNote::highlight("b", "x2", "x3", "two", 2_000),
        ];
        let merged = merged_record(&left, Some(&right));
        assert_eq!(
            merged.progress.as_ref().unwrap().xpointer.as_deref(),
            Some("remote")
        );
        assert_eq!(merged.notes.len(), 2);
        assert_eq!(schema_version(), SYNC_SCHEMA_VERSION);
    }

    #[test]
    fn errors_speak_the_readers_language() {
        assert_eq!(
            error_message(&JianguoyunError::AuthFailed),
            tr(Msg::JianguoyunErrAuth)
        );
        assert_eq!(
            error_message(&JianguoyunError::RateLimited {
                retry_after_secs: Some(60)
            }),
            tr(Msg::JianguoyunErrRate)
        );
        assert_eq!(
            error_message(&JianguoyunError::Transport("reset".into())),
            tr(Msg::JianguoyunErrNetwork)
        );
        assert_eq!(
            error_message(&JianguoyunError::InvalidHash("..".into())),
            tr(Msg::JianguoyunErrServerUrl)
        );
        assert_eq!(
            download_error_message(&DownloadError::UnsupportedFormat),
            tr(Msg::JianguoyunErrDownloadFormat)
        );
        assert_eq!(
            download_error_message(&DownloadError::Io(std::io::Error::other("disk"))),
            tr(Msg::JianguoyunErrDownloadStorage)
        );
        assert_eq!(
            download_error_message(&DownloadError::Http(
                aristotle_net::http::Error::NotAllowed("evil.example:443".into())
            )),
            tr(Msg::JianguoyunErrBlocked)
        );
        assert_eq!(
            download_error_message(&DownloadError::Config(JianguoyunError::AuthFailed)),
            tr(Msg::JianguoyunErrAuth)
        );
    }

    #[test]
    fn signing_out_forgets_the_channel_files() {
        let dir = crate::testdir("jgy-forget");
        fs::create_dir_all(&dir).unwrap();
        let mut state = JianguoyunState::new();
        state.record(&dir, "Books/dune.epub", "0123456789abcdef0123456789abcdef");
        state.cache.cached_at = 1;
        state.save_cache(&dir);
        assert!(channel_path(&dir).exists());
        assert!(cache_path(&dir).exists());

        state.current_dir = "Books".into();
        state.forget(&dir);
        assert!(state.channel_hashes.is_empty());
        assert!(state.downloads.is_empty());
        assert!(state.cache.dirs.is_empty());
        assert_eq!(state.current_dir, "");
        assert!(!channel_path(&dir).exists());
        assert!(!cache_path(&dir).exists());
        assert_eq!(load_channel(&channel_path(&dir)), ChannelRecords::default());
    }

    #[test]
    fn a_complete_account_is_required() {
        let mut settings = JianguoyunSettings::official("anna@example.com", "");
        assert!(!configured(&settings));
        settings.password = "app-password".into();
        assert!(configured(&settings));
        settings.username.clear();
        assert!(!configured(&settings));
        // The debug output never carries the application password.
        settings.username = "anna@example.com".into();
        let debug = format!("{settings:?}");
        assert!(!debug.contains("app-password"), "{debug}");
        assert!(debug.contains("[REDACTED]"));
        // And the state's own debug output keeps it out too.
        let mut state = JianguoyunState::new();
        state.login.password = "hunter2".into();
        let debug = format!("{state:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }

    #[test]
    fn a_filter_keeps_the_folders_and_picks_the_books() {
        use ui_shelf::JianguoyunDownloadState as D;
        use ui_shelf::JianguoyunFilter as F;
        let mut folder = ui_shelf::JianguoyunEntry::folder("Books", "Books");
        folder.download = D::NotDownloaded;
        let mut downloaded = ui_shelf::JianguoyunEntry::book("Books/a.epub", "A", "EPUB", 1);
        downloaded.download = D::Downloaded;
        let pending = ui_shelf::JianguoyunEntry::book("Books/b.epub", "B", "EPUB", 1);
        assert!(F::All.accepts(&folder));
        assert!(F::All.accepts(&downloaded));
        assert!(F::All.accepts(&pending));
        assert!(F::Downloaded.accepts(&folder), "folders are the way in");
        assert!(F::Downloaded.accepts(&downloaded));
        assert!(!F::Downloaded.accepts(&pending));
        assert!(F::NotDownloaded.accepts(&pending));
        assert!(!F::NotDownloaded.accepts(&downloaded));
        assert_eq!(F::ALL.len(), 3);
    }

    #[test]
    fn indexing_entry_shows_as_downloaded_before_scan_completes() {
        let dir = crate::testdir("jgy-indexing-state");
        fs::create_dir_all(&dir).unwrap();
        let lib = Library::open(&dir).unwrap();
        let mut state = JianguoyunState::new();
        let rel = "Books/三体.epub";
        state.record(&dir, rel, "0123456789abcdef0123456789abcdef");
        // Before `indexing` is marked and before scan finishes, the library
        // does not have the book yet.
        assert_eq!(
            state.download_state(rel, &lib),
            ui_shelf::JianguoyunDownloadState::NotDownloaded
        );
        // Once download finishes and awaits background scan admission, the
        // shelf must immediately display Downloaded so the user cannot trigger
        // a duplicate download.
        state.indexing.insert(rel.to_string());
        assert_eq!(
            state.download_state(rel, &lib),
            ui_shelf::JianguoyunDownloadState::Downloaded
        );
        state.forget(&dir);
        assert!(state.indexing.is_empty());
    }
}
