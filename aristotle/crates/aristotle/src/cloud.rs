// SPDX-License-Identifier: AGPL-3.0-or-later
//! The cloud shelf in the app: the Readest book list, its on-disk cache, and
//! a streaming download into the library folder.
//!
//! The list comes from `core::cloud` (`pull_all_books` then
//! `filter_and_sort_cloud_books`), and is cached in the library's data
//! directory so a reader without a network still sees the last list. The
//! download asks `core::cloud` for a signed ticket, streams the bytes through
//! `aristotle-net`'s downloader into a hidden `.part` file, verifies the
//! partial MD5 with `core::cloud::verify_download_hash`, and only then renames
//! it into `/mnt/us/documents/`. The library scan gives it its index entry.
//!
//! Signed URLs never reach a log or `Debug` output: `DownloadTicket` redacts
//! its URL, and this module keeps the ticket inside the worker.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aristotle_i18n::{tf, tr, Msg};
use aristotle_net::cloud::{download_book, DownloadError};
use aristotle_net::download;
use aristotle_net::http::Error as HttpError;
use aristotle_net::policy::Policy;
use aristotle_net::ReadestTransport;
use inkread_core::cloud::{CloudBook, CloudError};
use inkread_core::sync::{SessionStore, SyncClient, SyncConfig};
use serde::{Deserialize, Serialize};

/// The cache file in the library's data directory.
pub const CACHE_FILE: &str = "cloud-library.json";
/// The longest a downloaded book may be, matching the OPDS downloader.
pub const MAX_BOOK: u64 = download::MAX_BOOK;
/// Free space to leave on the userstore after a download.
pub const KEEP_FREE: u64 = download::KEEP_FREE;

/// What the app remembers between cloud screens and requests.
#[derive(Default)]
pub struct CloudState {
    pub books: Vec<CloudBook>,
    /// The account the list belongs to, used for storage keys.
    pub user_id: Option<String>,
    pub filter: aristotle_ui::screens::cloud::CloudFilter,
    pub loading: bool,
    /// A failure, in the reader's language, shown above the list.
    pub message: Option<String>,
    /// The book whose download is in flight.
    pub downloading: Option<String>,
    /// The flag that stops the download in flight.
    pub cancel: Option<Arc<AtomicBool>>,
    pub download_progress: Option<(u64, Option<u64>)>,
    /// The books the screen drew, in its order: a row action names one.
    pub rows: CloudRows,
    /// The book that failed last, and why.
    pub failed: Option<(String, String)>,
    /// Identifies the current list request.
    pub token: u64,
    /// Identifies the current download request.
    pub download_token: u64,
    /// When the list was fetched or read from the cache.
    pub last_sync: Option<i64>,
    pub cache_time: Option<i64>,
    /// The list on screen came from the cache.
    pub offline: bool,
    /// The cache has been read once this session.
    pub loaded: bool,
}

#[derive(Serialize, Deserialize)]
struct CloudCache {
    cached_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    books: Vec<CloudBook>,
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

/// Read the cached list. A missing or damaged file is no cache.
pub fn load_cache(path: &Path) -> Option<(i64, Option<String>, Vec<CloudBook>)> {
    let bytes = fs::read(path).ok()?;
    let cache: CloudCache = serde_json::from_slice(&bytes).ok()?;
    Some((cache.cached_at, cache.user_id, cache.books))
}

/// Write the list for the next offline visit; a failure only costs the cache.
pub fn save_cache(path: &Path, user_id: Option<&str>, books: &[CloudBook], when: i64) {
    let cache = CloudCache {
        cached_at: when,
        user_id: user_id.map(str::to_string),
        books: books.to_vec(),
    };
    let Ok(json) = serde_json::to_vec(&cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, json).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Wording for a failed list request. `has_cache` says whether the last
/// saved list is still on screen.
pub fn list_error_message(e: &CloudError, has_cache: bool) -> String {
    match e {
        CloudError::Sync(e) => sync_list_error_message(e.kind(), has_cache),
        CloudError::Transport(_) => list_network_message(has_cache),
        CloudError::HttpStatus(status, _) if *status == 401 || *status == 403 => {
            tr(Msg::CloudErrAuth).to_string()
        }
        CloudError::HttpStatus(status, _) if *status == 507 => tr(Msg::CloudErrQuota).to_string(),
        _ => tr(Msg::CloudErrServer).to_string(),
    }
}

fn list_network_message(has_cache: bool) -> String {
    if has_cache {
        tr(Msg::CloudErrNetwork).to_string()
    } else {
        tr(Msg::CloudErrNoNetwork).to_string()
    }
}

fn sync_list_error_message(kind: inkread_core::sync::SyncErrorKind, has_cache: bool) -> String {
    use inkread_core::sync::SyncErrorKind as K;
    match kind {
        K::AuthFailed | K::AuthExpired => tr(Msg::CloudErrAuth).to_string(),
        K::Network => list_network_message(has_cache),
        // The device's own firewall refused the host: saying "network
        // unavailable" here sent readers looking at their Wi-Fi (SYNC-111).
        K::Blocked => tr(Msg::ReadestErrBlocked).to_string(),
        K::TooLarge => tr(Msg::ReadestErrTooLarge).to_string(),
        K::Server | K::Invalid => tr(Msg::CloudErrServer).to_string(),
    }
}

/// Wording for a failed download: its own words, not the list's.
pub fn download_error_message(e: &DownloadError) -> String {
    match e {
        DownloadError::Cloud(e) => cloud_download_error_message(e),
        DownloadError::Http(e) => http_download_error_message(e),
        DownloadError::UnsafeHash => tr(Msg::CloudErrBadHash).to_string(),
        DownloadError::InsecureUrl => tr(Msg::CloudErrInsecure).to_string(),
    }
}

fn sync_download_error_message(kind: inkread_core::sync::SyncErrorKind) -> String {
    use inkread_core::sync::SyncErrorKind as K;
    match kind {
        K::AuthFailed | K::AuthExpired => tr(Msg::CloudErrDlAuth).to_string(),
        K::Network => tr(Msg::CloudErrDlNetwork).to_string(),
        K::Blocked => tr(Msg::ReadestErrBlocked).to_string(),
        K::TooLarge => tr(Msg::ReadestErrTooLarge).to_string(),
        K::Server | K::Invalid => tr(Msg::CloudErrServer).to_string(),
    }
}

fn cloud_download_error_message(e: &CloudError) -> String {
    match e {
        CloudError::HashMismatch { .. } => tr(Msg::CloudErrHash).to_string(),
        CloudError::TicketExpired => tr(Msg::CloudErrExpired).to_string(),
        CloudError::Sync(e) => sync_download_error_message(e.kind()),
        CloudError::Transport(_) => tr(Msg::CloudErrDlNetwork).to_string(),
        CloudError::HttpStatus(status, _) if *status == 401 => tr(Msg::CloudErrDlAuth).to_string(),
        CloudError::HttpStatus(status, _) if *status == 403 => {
            tr(Msg::CloudErrDlForbidden).to_string()
        }
        CloudError::HttpStatus(status, _) if *status == 410 => tr(Msg::CloudErrExpired).to_string(),
        CloudError::HttpStatus(status, _) if *status == 507 => tr(Msg::CloudErrQuota).to_string(),
        CloudError::Io(e) if e.kind() == std::io::ErrorKind::StorageFull => {
            tr(Msg::CloudErrQuota).to_string()
        }
        CloudError::MissingUserId => tr(Msg::CloudErrDlAuth).to_string(),
        _ => tr(Msg::CloudErrUnknown).to_string(),
    }
}

fn http_download_error_message(e: &HttpError) -> String {
    match e {
        HttpError::NotAllowed(_) | HttpError::Connect(_) | HttpError::Timeout => {
            tr(Msg::CloudErrDlNetwork).to_string()
        }
        HttpError::Unauthorized => tr(Msg::CloudErrDlAuth).to_string(),
        HttpError::TooLarge(_) => tr(Msg::CloudErrQuota).to_string(),
        HttpError::Status(status, _) if *status == 401 => tr(Msg::CloudErrDlAuth).to_string(),
        HttpError::Status(status, _) if *status == 403 => tr(Msg::CloudErrDlForbidden).to_string(),
        HttpError::Status(status, _) if *status == 410 => tr(Msg::CloudErrExpired).to_string(),
        HttpError::Status(status, _) if *status == 507 => tr(Msg::CloudErrQuota).to_string(),
        HttpError::Cancelled => tr(Msg::CloudErrCancelled).to_string(),
        _ => tr(Msg::CloudErrUnknown).to_string(),
    }
}

/// What `show_cloud` needs from the app, without borrowing `self`.
pub struct CloudRequest {
    pub local_hashes: HashSet<String>,
    pub config: SyncConfig,
    /// The session of the account, shared with the other requests of it.
    pub session: SessionStore,
    pub policy: Policy,
}

/// Fetch and filter the account's books on a worker thread.
pub type CloudListResult = Result<(Vec<CloudBook>, Option<String>), CloudError>;

pub fn fetch_list(request: CloudRequest) -> (CloudListResult, u64) {
    let epoch = request.session.epoch();
    let client = SyncClient::with_session_store(
        ReadestTransport::new(request.policy),
        request.config,
        request.session,
    );
    let result = (|| -> Result<(Vec<CloudBook>, Option<String>), CloudError> {
        let books = client.pull_all_books(0, 100)?;
        let user_id = books.iter().find_map(|b| {
            b.user_id
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_string)
        });
        let list = inkread_core::cloud::filter_and_sort_cloud_books(
            &books,
            user_id.as_deref(),
            &request.local_hashes,
        )?;
        Ok((list, user_id))
    })();
    (result, epoch)
}

/// The words the account card shows for `when`.
pub fn time_label(when: i64) -> String {
    crate::marks::local_time(when)
}

/// The "downloaded" hint after a successful download.
pub fn downloaded_ready(title: &str) -> String {
    tf!(Msg::CloudDownloadedReady, title = title)
}

// ------------------------------------------------------------------ app

use crate::device::Facts;
use crate::net::{self, Done};
use crate::{App, Flow, Screen};
use aristotle_render::plato_core::anyhow::Error;
use aristotle_ui::screens::cloud::{
    CloudBook as CloudViewBook, CloudBookState, CloudFilter, CloudRows, CloudView,
};
use aristotle_ui::tree::{NodeId, Refresh};
use aristotle_ui::ui::UiEvent;

impl App {
    /// Open the cloud shelf: the cached list first, then a fresh fetch.
    pub(crate) fn open_cloud(&mut self) -> Result<(), Error> {
        self.show_cloud(Refresh::Full, 0)?;
        if self.net.accounts.readest.is_some() && !self.cloud.loading {
            self.refresh_cloud();
            self.show_cloud(Refresh::Auto, 0)?;
        }
        Ok(())
    }

    /// Rebuild the cloud shelf on the current data.
    pub(crate) fn show_cloud(&mut self, refresh: Refresh, page: usize) -> Result<(), Error> {
        let signed_in = self.net.accounts.readest.is_some();
        if signed_in && !self.cloud.loaded {
            self.cloud.loaded = true;
            let path = cache_path(self.lib.data_dir());
            if let Some((cached_at, user_id, books)) = load_cache(&path) {
                self.cloud.books = books;
                self.cloud.user_id = user_id;
                self.cloud.cache_time = Some(cached_at);
                self.cloud.last_sync = Some(cached_at);
                self.cloud.offline = true;
            }
        }
        if !signed_in {
            self.cloud.books.clear();
            self.cloud.user_id = None;
            self.cloud.loading = false;
            self.cloud.message = None;
            self.cloud.offline = false;
            self.cloud.loaded = false;
        }

        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let last_sync = self.cloud.last_sync.or_else(|| {
            self.net
                .accounts
                .readest
                .as_ref()
                .and_then(|r| r.last_sync_time)
        });
        let last_sync_label = last_sync.map(time_label);
        let filter = self.cloud.filter;
        let downloading = self.cloud.downloading.clone();
        let failed = self.cloud.failed.clone();
        let download_progress = self.cloud.download_progress;
        let books = self.cloud.books.clone();

        let mut ui_books = Vec::with_capacity(books.len());
        for b in &books {
            let rec = self.lib.book(&b.book_hash).cloned();
            let downloaded = rec.is_some();
            let state = if downloading.as_deref() == Some(b.book_hash.as_str()) {
                CloudBookState::Downloading
            } else if failed
                .as_ref()
                .is_some_and(|(hash, _)| hash == &b.book_hash)
            {
                CloudBookState::Failed
            } else if downloaded {
                CloudBookState::Downloaded
            } else {
                CloudBookState::NotDownloaded
            };
            let fraction = match (state, download_progress) {
                (CloudBookState::Downloading, Some((bytes, Some(total)))) if total > 0 => {
                    (bytes as f32 / total as f32).clamp(0.0, 1.0)
                }
                _ => 0.0,
            };
            let progress_known = !matches!(
                (state, download_progress),
                (CloudBookState::Downloading, Some((_, None)) | None)
            );
            let cover = rec.as_ref().and_then(|r| self.covers.get(&self.lib, r));
            let mut view_book = CloudViewBook::new(&b.title, &b.author, &b.format);
            view_book.hash = b.book_hash.clone();
            view_book.progress = b.progress;
            view_book.state = state;
            view_book.progress_fraction = fraction;
            view_book.progress_known = progress_known;
            view_book.downloaded = downloaded;
            view_book.cover = cover;
            ui_books.push(view_book);
        }

        let view = CloudView {
            status,
            signed_in,
            book_count: books.len(),
            local_books_count: self.lib.books().len(),
            last_sync: last_sync_label.as_deref(),
            offline: self.cloud.offline,
            filter,
            loading: self.cloud.loading,
            message: self.cloud.message.as_deref(),
            books: &ui_books,
        };
        let width = self.ui.bounds().w;
        let (tree, cloud) =
            CloudView::build_for_width(&mut self.ui.text, &self.styles, &view, width);
        // The row actions of this screen belong to this list, in this order.
        self.cloud.rows = cloud.row_books.clone();
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        if page > 0 {
            self.ui.tree.set_page(cloud.list, page);
            self.ui.layout();
        }
        cloud.sync(&mut self.ui.tree);
        self.screen = Screen::Cloud(Box::new(cloud));
        self.render_ui()
    }

    /// Ask the account for the cloud list on a worker thread.
    pub(crate) fn refresh_cloud(&mut self) {
        if self.cloud.loading {
            return;
        }
        if self.net.accounts.readest.is_none() {
            return;
        }
        self.cloud.loading = true;
        self.cloud.message = None;
        self.cloud.token = self.cloud.token.wrapping_add(1);
        let token = self.cloud.token;
        // The list is the first Readest request of a visit to the shelf: make
        // sure the device's firewall allows the hosts it will dial.
        self.net.reconcile_allow_entries();
        let local_hashes: HashSet<String> = self.lib.books().iter().map(|b| b.id.clone()).collect();
        let request = CloudRequest {
            local_hashes,
            config: self.net.accounts.readest_config(),
            session: self.net.readest_session.clone(),
            policy: net::Net::policy(),
        };
        self.net.spawn("cloud-list", move |_| {
            let (result, session) = fetch_list(request);
            Done::CloudListed {
                token,
                store: session,
                result,
            }
        });
    }

    /// The list arrived: keep it, cache it, or show the last cache on failure.
    pub(crate) fn on_cloud_listed(
        &mut self,
        token: u64,
        store: u64,
        result: Result<(Vec<CloudBook>, Option<String>), CloudError>,
    ) -> Result<(), Error> {
        if token != self.cloud.token {
            return Ok(());
        }
        if !self.net.is_current_store(store) {
            return Ok(());
        }
        self.cloud.loading = false;
        if let Some(sess) = self.net.readest_session.current() {
            if let Some(r) = self.net.accounts.readest.as_mut() {
                r.update_session(&sess);
                self.net.save();
            }
        }
        match result {
            Ok((books, user_id)) => {
                let now = now_secs();
                self.cloud.books = books;
                self.cloud.user_id = user_id;
                self.cloud.last_sync = Some(now);
                self.cloud.offline = false;
                self.cloud.message = None;
                self.cloud.failed = None;
                let path = cache_path(self.lib.data_dir());
                save_cache(&path, self.cloud.user_id.as_deref(), &self.cloud.books, now);
            }
            Err(e) => {
                // "Showing the last saved list" only when there is one.
                let has_cache = !self.cloud.books.is_empty();
                self.cloud.message = Some(list_error_message(&e, has_cache));
                self.cloud.offline = has_cache;
                if self.cloud.last_sync.is_none() {
                    self.cloud.last_sync = self.cloud.cache_time;
                }
            }
        }
        if matches!(self.screen, Screen::Cloud(_)) {
            let page = self.list_page();
            self.show_cloud(Refresh::Auto, page)?;
        }
        Ok(())
    }

    /// Events on the cloud shelf.
    pub(crate) fn on_cloud_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use aristotle_ui::screens::cloud::action as a;
        match ev {
            UiEvent::Swipe(dir) => {
                let old = self.list_page();
                let mut handled = false;
                if let Screen::Cloud(view) = &mut self.screen {
                    if view.handle_swipe(&mut self.ui.tree, dir) {
                        self.ui.layout();
                        view.sync(&mut self.ui.tree);
                        handled = true;
                    }
                }
                if handled && self.list_page() != old {
                    self.ui.request_clean_refresh();
                    self.render_ui()?;
                }
            }
            UiEvent::Action { id, node, .. } => {
                if id == a::LOCAL {
                    self.cancel_cloud_on_leave();
                    self.show_library(Refresh::Full)?;
                } else if id == a::LOGIN {
                    self.cancel_cloud_on_leave();
                    self.show_mine(Refresh::Full)?;
                } else if id == a::SYNC {
                    let page = self.list_page();
                    self.refresh_cloud();
                    self.show_cloud(Refresh::Auto, page)?;
                } else if (a::FILTER..a::FILTER + CloudFilter::ALL.len() as u32).contains(&id) {
                    let i = (id - a::FILTER) as usize;
                    self.cloud.filter = CloudFilter::ALL[i];
                    self.show_cloud(Refresh::Auto, 0)?;
                } else if (a::ITEM..a::CANCEL).contains(&id) {
                    if let Some(hash) = self.cloud.rows.target(a::ITEM, id).map(str::to_string) {
                        self.start_cloud_download(hash, Some(node))?;
                    }
                } else if id >= a::CANCEL {
                    if let Some(hash) = self.cloud.rows.target(a::CANCEL, id).map(str::to_string) {
                        self.cancel_cloud_download(&hash);
                    }
                } else {
                    let mut handled = false;
                    if let Screen::Cloud(view) = &mut self.screen {
                        handled = view.handle(&mut self.ui.tree, id);
                        if handled {
                            self.ui.layout();
                            view.sync(&mut self.ui.tree);
                        }
                    }
                    if handled {
                        self.ui.request_clean_refresh();
                        self.render_ui()?;
                    }
                }
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// Stop the download of `hash`, if it is the one in flight. The worker
    /// removes its `.part` file when it sees the flag.
    fn cancel_cloud_download(&mut self, hash: &str) {
        if self.cloud.downloading.as_deref() != Some(hash) {
            return;
        }
        self.cancel_cloud_on_leave();
    }

    /// Leaving the cloud shelf stops its download: the reader asked to go
    /// somewhere else.
    pub(crate) fn cancel_cloud_on_leave(&self) {
        if let Some(cancel) = &self.cloud.cancel {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Download the cloud book `hash`, or open it if it is local.
    fn start_cloud_download(&mut self, hash: String, node: Option<NodeId>) -> Result<(), Error> {
        let Some(book) = self
            .cloud
            .books
            .iter()
            .find(|b| b.book_hash == hash)
            .cloned()
        else {
            return Ok(());
        };
        if self.lib.book(&hash).is_some() {
            self.cancel_cloud_on_leave();
            return self.open_book(&hash, node);
        }
        if self.cloud.downloading.as_deref() == Some(hash.as_str()) {
            return Ok(());
        }
        if self.net.accounts.readest.is_none() {
            self.show_mine(Refresh::Full)?;
            return Ok(());
        }
        let Some(user_id) = self.cloud.user_id.clone() else {
            self.cloud.message = Some(tr(Msg::CloudErrAuth).to_string());
            return self.show_cloud(Refresh::Auto, self.list_page());
        };

        let free = aristotle_hal::system::storage(&self.root)
            .map(|s| s.free)
            .unwrap_or(MAX_BOOK);
        if free <= KEEP_FREE {
            self.cloud.message = Some(tr(Msg::CloudErrQuota).to_string());
            return self.show_cloud(Refresh::Auto, self.list_page());
        }

        self.cloud.download_token = self.cloud.download_token.wrapping_add(1);
        let download_token = self.cloud.download_token;
        self.cloud.downloading = Some(hash.clone());
        self.cloud.download_progress = Some((0, None));
        self.cloud.failed = None;
        self.cloud.message = None;

        let limit = MAX_BOOK.min(free.saturating_sub(KEEP_FREE));
        let policy = net::Net::policy();
        let config = self.net.accounts.readest_config();
        let api_base = config.api_base.clone();
        let session = self.net.readest_session.clone();
        let epoch = session.epoch();
        let dir = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cloud.cancel = Some(cancel.clone());
        let ui_book = book.clone();

        self.net.spawn("cloud-download", move |tx| {
            let client = SyncClient::with_session_store(
                ReadestTransport::new(policy.clone()),
                config,
                session,
            );
            let result = (|| -> Result<PathBuf, DownloadError> {
                let token = client
                    .ensure_token()
                    .map_err(|e| DownloadError::Cloud(CloudError::Sync(e)))?;
                let mut last = 0u64;
                let mut report = |bytes: u64, total: Option<u64>| {
                    let step = total.map_or(1 << 20, |t| (t / 20).max(1));
                    if bytes >= last + step {
                        last = bytes;
                        let _ = tx.send(Done::CloudProgress {
                            token: download_token,
                            hash: hash.clone(),
                            bytes,
                            total,
                        });
                    }
                };
                download_book(
                    policy,
                    &api_base,
                    &token,
                    &user_id,
                    &ui_book,
                    &dir,
                    limit,
                    &cancel,
                    &mut report,
                )
            })();
            Done::CloudDownloaded {
                token: download_token,
                hash,
                store: epoch,
                result,
            }
        });
        self.show_cloud(Refresh::Auto, self.list_page())
    }

    /// Progress of a cloud download, as a percentage of the whole.
    pub(crate) fn on_cloud_progress(
        &mut self,
        token: u64,
        hash: String,
        bytes: u64,
        total: Option<u64>,
    ) -> Result<(), Error> {
        if token != self.cloud.download_token
            || self.cloud.downloading.as_deref() != Some(hash.as_str())
        {
            return Ok(());
        }
        self.cloud.download_progress = Some((bytes, total));
        if matches!(self.screen, Screen::Cloud(_)) {
            let page = self.list_page();
            self.show_cloud(Refresh::Auto, page)?;
        }
        Ok(())
    }

    /// A cloud download finished: index the new file, or show its failure.
    pub(crate) fn on_cloud_downloaded(
        &mut self,
        token: u64,
        hash: String,
        store: u64,
        result: Result<PathBuf, DownloadError>,
    ) -> Result<(), Error> {
        if token != self.cloud.download_token
            || self.cloud.downloading.as_deref() != Some(hash.as_str())
        {
            return Ok(());
        }
        if !self.net.is_current_store(store) {
            return Ok(());
        }
        self.cloud.downloading = None;
        self.cloud.cancel = None;
        self.cloud.download_progress = None;
        if let Some(sess) = self.net.readest_session.current() {
            if let Some(r) = self.net.accounts.readest.as_mut() {
                r.update_session(&sess);
                self.net.save();
            }
        }
        let page = self.list_page();
        let title = self
            .cloud
            .books
            .iter()
            .find(|b| b.book_hash == hash)
            .map(|b| b.title.clone())
            .unwrap_or_default();
        match result {
            Ok(path) => {
                // Index only the file the reader asked for: the background
                // walk refreshes known books and admits this path alone
                // (UX-112).
                self.admit_explicit_file(&path);
                self.cloud.failed = None;
                self.cloud.message = Some(downloaded_ready(&title));
            }
            Err(e) => {
                let message = download_error_message(&e);
                self.cloud.message = Some(message.clone());
                // A cancel is the reader's own choice: the row goes back to
                // "download", it is not marked as a failure.
                if matches!(e, DownloadError::Http(HttpError::Cancelled)) {
                    self.cloud.failed = None;
                } else {
                    self.cloud.failed = Some((hash, message));
                }
            }
        }
        if matches!(self.screen, Screen::Cloud(_)) {
            self.show_cloud(Refresh::Auto, page)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip_keeps_books_and_user() {
        let dir = std::env::temp_dir().join(format!(
            "aristotle-cloud-cache-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = cache_path(&dir);
        let book = CloudBook {
            book_hash: "abc".into(),
            title: "Book".into(),
            source_title: None,
            author: "Author".into(),
            format: "EPUB".into(),
            progress: Some((1, 10)),
            cover_hash: None,
            uploaded_at: Some("2026-11-09T00:00:00Z".into()),
            deleted_at: None,
            file_key: "u/Readest/Books/abc/Book.epub".into(),
            downloaded: false,
            meta_hash: None,
            updated_at: None,
        };
        save_cache(&path, Some("user-1"), std::slice::from_ref(&book), 42);
        let (when, user, books) = load_cache(&path).unwrap();
        assert_eq!(when, 42);
        assert_eq!(user.as_deref(), Some("user-1"));
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].book_hash, "abc");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_cache_is_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "aristotle-cloud-bad-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = cache_path(&dir);
        fs::write(&path, b"not json").unwrap();
        assert!(load_cache(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quota_and_auth_errors_have_their_own_words() {
        assert_eq!(
            download_error_message(&DownloadError::Cloud(CloudError::HashMismatch {
                expected: "a".into(),
                actual: "b".into(),
            })),
            tr(Msg::CloudErrHash)
        );
        assert_eq!(
            download_error_message(&DownloadError::Cloud(CloudError::TicketExpired)),
            tr(Msg::CloudErrExpired)
        );
    }
}
