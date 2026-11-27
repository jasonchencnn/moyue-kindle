// SPDX-License-Identifier: AGPL-3.0-or-later
//! WeRead in the app: the account, the QR sign-in, and the read-only shelf.
//!
//! The wire work is `core::weread`'s (contract §14); this module owns what
//! the app adds: a QR state machine on a worker thread, the credentials on
//! disk, and the shelf's cursor and cache in memory.
//!
//! Credentials live in `weread.json` next to `accounts.json`, in the same
//! private folder (0700) and with the same owner-only write (0600); the
//! `Debug` output redacts every secret. The shelf is fetched when the screen
//! opens or the reader asks for it: paging the screen never touches this
//! module.
//!
//! Contract §14.7: `-2041` (manual verification) and `-10102` (rate limit)
//! are terminal. The client returns them without a retry, and this module
//! turns them into a sentence the reader can act on.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aristotle_hal::system;
use aristotle_i18n::{tr, Msg};
use aristotle_net::store::{private_dir, write_private};
use aristotle_net::{Policy, ReadestTransport};
use inkread_core::sync::transport::{HttpRequest, HttpTransport, TransportError};
use inkread_core::weread::{
    build_login_body, build_login_request, build_qrconnect_url, parse_login_response,
    parse_qr_ticket, parse_qrconnect_response, poll_status, version_headers, Credentials,
    QrPollStatus, QrTicket, ShelfBook, ShelfSync, WeReadClient, WeReadError, DEFAULT_DEVICE_NAME,
    EINK_BASE,
};
use serde::{Deserialize, Serialize};

use inkread_core::weread::auth::percent_encode;

mod download;
mod notes;
mod readtime;
pub(crate) mod web;

use crate::device::Facts;
use crate::net::{self, Done};
use crate::wake::Sender;
use crate::{App, Flow, Screen};
use aristotle_render::plato_core::anyhow::Error;
use aristotle_ui::screens::weread_login as ui_login;
use aristotle_ui::screens::weread_shelf as ui_shelf;
use aristotle_ui::tree::Refresh;
use aristotle_ui::ui::UiEvent;

/// The credentials file in the app's private folder.
pub const STORE_FILE: &str = "weread.json";
/// One long-poll may take this long before it is retried.
pub const POLL_TIMEOUT_SECS: u64 = 60;
/// How long a sign-in code is followed overall.
pub const POLL_DEADLINE: Duration = Duration::from_secs(600);
/// The first backoff after a transport failure.
const BACKOFF_START: Duration = Duration::from_secs(2);
/// The longest backoff after a transport failure.
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// How many expired codes in a row are replaced automatically.
pub const MAX_QR_ROTATIONS: u32 = 3;

/// The WeChat application id of the WeRead E-Ink client, from the contract.
const WECHAT_SDK: &str = "https://open.weixin.qq.com";
/// The long-poll endpoint of the WeChat QR login.
const WECHAT_LONG_POLL: &str = "https://long.open.weixin.qq.com";

/// `now` in seconds, the same clock as the cloud shelf's cache.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn random_u16() -> u16 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_micros())
        .unwrap_or(0)
        % 1000) as u16
}

// ---------------------------------------------------------------- store

/// The account as stored on disk. Never `Debug`: it holds the tokens.
#[derive(Clone, Default, Serialize, Deserialize)]
struct StoredAccount {
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    install_id: String,
    #[serde(default)]
    credentials: Option<StoredCredentials>,
    #[serde(default)]
    synckey: Option<i64>,
    #[serde(default)]
    last_sync: Option<i64>,
}

/// The three secrets, in the file's own shape.
#[derive(Clone, Serialize, Deserialize)]
struct StoredCredentials {
    vid: String,
    access_token: String,
    refresh_token: String,
    device_id: String,
}

impl From<&Credentials> for StoredCredentials {
    fn from(credentials: &Credentials) -> Self {
        Self {
            vid: credentials.vid.clone(),
            access_token: credentials.access_token.clone(),
            refresh_token: credentials.refresh_token.clone(),
            device_id: credentials.device_id.clone(),
        }
    }
}

impl From<&StoredCredentials> for Credentials {
    fn from(stored: &StoredCredentials) -> Self {
        Credentials::new(
            stored.vid.clone(),
            stored.access_token.clone(),
            stored.refresh_token.clone(),
            stored.device_id.clone(),
        )
    }
}

fn store_path(dir: &Path) -> PathBuf {
    dir.join(STORE_FILE)
}

/// Read the account; a missing or damaged file is "not signed in".
fn load_account(path: &Path) -> StoredAccount {
    let Ok(bytes) = std::fs::read(path) else {
        return StoredAccount::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Write the account owner-only; a failure only costs the next sign-in.
fn save_account(path: &Path, account: &StoredAccount) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let Ok(json) = serde_json::to_vec(account) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "weread.json",
        ));
    };
    write_private(path, &json)
}

/// A device id and install id that stay the same between sign-ins.
///
/// The contract wants `installId = eink31` plus 26 digits. The ids are
/// generated from the clock and the process once, then stored; the real
/// client's device id is opaque to the server, so the shape is ours.
fn fresh_identity() -> (String, String) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seed = nanos
        .saturating_mul(1_000_003)
        .saturating_add(std::process::id() as u128);
    let device_id = format!("moyue-eink-{seed:016x}");
    let install_id = format!(
        "eink31{:026}",
        seed % 100_000_000_000_000_000_000_000_000u128
    );
    (device_id, install_id)
}

// ---------------------------------------------------------------- login

/// The QR sign-in driver over the injected transport.
///
/// It builds the three requests the contract describes (ticket, QR, long
/// poll) and the `/login` exchange with the core builders. Tests drive it
/// with `core::weread::mock::MockTransport`; the app drives it with
/// `aristotle_net::ReadestTransport`.
pub struct LoginDriver<T: HttpTransport> {
    transport: T,
    base: String,
    /// The pause between two `408` polls; the mock sets it to zero.
    pause: Duration,
}

impl<T: HttpTransport> LoginDriver<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            base: EINK_BASE.to_string(),
            pause: Duration::from_millis(800),
        }
    }

    /// Set the pause between polls (tests use zero).
    pub fn with_pause(mut self, pause: Duration) -> Self {
        self.pause = pause;
        self
    }

    /// The pause after a `408`.
    pub fn pause(&self) -> Duration {
        self.pause
    }

    fn get(&self, url: &str) -> Result<inkread_core::sync::transport::HttpResponse, WeReadError> {
        let mut request = HttpRequest::new("GET", url);
        for (name, value) in version_headers() {
            request = request.with_header(name, value);
        }
        Ok(self.transport.send(request)?)
    }

    /// `GET /wxticket?nonceStr=weread` (§14.2 step 1).
    pub fn ticket(&self) -> Result<QrTicket, WeReadError> {
        let response = self.get(&format!("{}/wxticket?nonceStr=weread", self.base))?;
        parse_qr_ticket(&response)
    }

    /// The `uuid` of the QR code (§14.2 step 2).
    pub fn qr_confirm(&self, ticket: &QrTicket) -> Result<String, WeReadError> {
        let response = self.get(&build_qrconnect_url(ticket))?;
        parse_qrconnect_response(&response)
    }

    /// The confirmation URL the QR code carries (§14.2 step 3).
    pub fn confirm_url(&self, uuid: &str) -> String {
        format!("{WECHAT_SDK}/connect/confirm?uuid={}", percent_encode(uuid))
    }

    /// One long poll (§14.2 step 4).
    pub fn poll(&self, uuid: &str, last: i64) -> Result<QrPollStatus, WeReadError> {
        let url = format!(
            "{WECHAT_LONG_POLL}/connect/l/qrconnect?f=json&uuid={}&last={last}",
            percent_encode(uuid)
        );
        let response = self.get(&url)?;
        poll_status(&response)
    }

    /// Exchange `wx_code` for credentials (§14.2 step 5).
    pub fn exchange(
        &self,
        code: &str,
        device_id: &str,
        device_name: &str,
        install_id: &str,
    ) -> Result<Credentials, WeReadError> {
        let body = build_login_body(
            code,
            device_id,
            device_name,
            install_id,
            now_ms(),
            random_u16(),
        );
        let request = build_login_request(&self.base, &body)?;
        let response = self.transport.send(request)?;
        parse_login_response(&response, device_id)
    }
}

/// What the login worker got at the start: the ids the exchange signs with.
#[derive(Clone, Debug)]
pub struct LoginRequest {
    pub generation: u64,
    pub device_id: String,
    pub device_name: String,
    pub install_id: String,
}

/// A transport failure worth another poll; a refusal or a contract error
/// is not.
fn retryable(error: &WeReadError) -> bool {
    match error {
        WeReadError::Transport(_) => true,
        WeReadError::HttpStatus(status, _) => *status >= 500,
        _ => false,
    }
}

fn sleep_until_cancelled(cancel: &AtomicBool, pause: Duration) {
    let deadline = Instant::now() + pause;
    while Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(
            Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn failed(generation: u64, error: &WeReadError) -> Done {
    eprintln!("aristotle: weread login failed: {error}");
    Done::WereadLoginFailed {
        generation,
        message: error_message(error),
    }
}

/// The whole sign-in on one worker: ticket, QR, poll, exchange.
///
/// Progress reaches the main loop through `tx`; the returned `Done` always
/// ends the job, so the app can forget the worker.
pub fn run_login(
    policy: Policy,
    request: LoginRequest,
    cancel: Arc<AtomicBool>,
    tx: &Sender<Done>,
) -> Done {
    let generation = request.generation;
    let transport = ReadestTransport::with_timeout(policy, POLL_TIMEOUT_SECS);
    let driver = LoginDriver::new(transport).with_pause(Duration::from_millis(800));
    let ticket = match driver.ticket() {
        Ok(ticket) => ticket,
        Err(error) => return failed(generation, &error),
    };
    let uuid = match driver.qr_confirm(&ticket) {
        Ok(uuid) => uuid,
        Err(error) => return failed(generation, &error),
    };
    let url = driver.confirm_url(&uuid);
    let _ = tx.send(Done::WereadQr {
        generation,
        url: url.clone(),
        phase: ui_login::WereadLoginPhase::Waiting,
    });

    let mut last = 0i64;
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut backoff = BACKOFF_START;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Done::WereadLoginCancelled { generation };
        }
        if Instant::now() >= deadline {
            return Done::WereadLoginFailed {
                generation,
                message: tr(Msg::WereadErrNetwork).to_string(),
            };
        }
        match driver.poll(&uuid, last) {
            Ok(QrPollStatus::Continue) => {
                last = 408;
                sleep_until_cancelled(&cancel, driver.pause());
            }
            Ok(QrPollStatus::Scanned) => {
                last = 404;
                let _ = tx.send(Done::WereadQr {
                    generation,
                    url: url.clone(),
                    phase: ui_login::WereadLoginPhase::Scanned,
                });
                sleep_until_cancelled(&cancel, driver.pause());
            }
            Ok(QrPollStatus::Confirmed(code)) => {
                let _ = tx.send(Done::WereadQr {
                    generation,
                    url: url.clone(),
                    phase: ui_login::WereadLoginPhase::Confirmed,
                });
                return match driver.exchange(
                    &code,
                    &request.device_id,
                    &request.device_name,
                    &request.install_id,
                ) {
                    Ok(credentials) => Done::WereadLoggedIn {
                        generation,
                        credentials,
                    },
                    Err(error) => failed(generation, &error),
                };
            }
            Ok(QrPollStatus::Expired) => return Done::WereadLoginExpired { generation },
            Ok(QrPollStatus::Declined) => {
                return Done::WereadLoginFailed {
                    generation,
                    message: tr(Msg::WereadStatusDeclined).to_string(),
                }
            }
            Err(error) if retryable(&error) => {
                sleep_until_cancelled(&cancel, backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
            Err(error) => return failed(generation, &error),
        }
    }
}

/// Whether a failed request means the credentials are gone for good.
///
/// §14.7: `-2011` is terminal and the reader signs in again. `-2041` is
/// different (finish the check in the official app), so it keeps them.
fn auth_terminal(error: &WeReadError) -> bool {
    matches!(
        error,
        WeReadError::AuthExpired
            | WeReadError::AuthRejected(_)
            | WeReadError::NotAuthenticated
            | WeReadError::ManualVerification(-2011)
            | WeReadError::HttpStatus(401 | 403, _)
    )
}

// ---------------------------------------------------------------- shelf

/// Add the shelf cursor to the core client's request without touching the
/// core crate (WR-002 found `WeReadClient::shelf_sync` has no cursor
/// argument; reported to the Leader, transparent here rather than copied).
struct CursorTransport<T: HttpTransport> {
    inner: T,
    synckey: Option<i64>,
}

impl<T: HttpTransport> CursorTransport<T> {
    fn new(inner: T, synckey: Option<i64>) -> Self {
        Self { inner, synckey }
    }
}

impl<T: HttpTransport> HttpTransport for CursorTransport<T> {
    fn send(
        &self,
        mut request: HttpRequest,
    ) -> Result<inkread_core::sync::transport::HttpResponse, TransportError> {
        if let Some(synckey) = self.synckey {
            if request.method.eq_ignore_ascii_case("GET")
                && request.url.contains("/shelf/sync")
                && !request.url.contains("synckey=")
            {
                let separator = if request.url.contains('?') { '&' } else { '?' };
                request.url = format!("{}{separator}synckey={synckey}", request.url);
            }
        }
        self.inner.send(request)
    }
}

/// What a shelf request needs from the app.
#[derive(Clone)]
pub struct ShelfRequest {
    pub credentials: Credentials,
    pub synckey: Option<i64>,
    pub policy: Policy,
}

/// Fetch the shelf and return the (possibly refreshed) credentials with it.
pub fn fetch_shelf(request: ShelfRequest) -> (Result<ShelfSync, WeReadError>, Credentials) {
    fetch_shelf_with(
        // A 50k-book shelf can take longer than the 15 s Readest default.
        ReadestTransport::with_timeout(request.policy, POLL_TIMEOUT_SECS),
        request.credentials,
        request.synckey,
    )
}

/// The shelf fetch over an injected transport, for the app and the tests.
pub fn fetch_shelf_with<T: HttpTransport>(
    transport: T,
    credentials: Credentials,
    synckey: Option<i64>,
) -> (Result<ShelfSync, WeReadError>, Credentials) {
    let original = credentials.clone();
    let transport = CursorTransport::new(transport, synckey);
    let mut client = WeReadClient::new(transport, credentials);
    let result = client.shelf_sync();
    let credentials = client.credentials().cloned().unwrap_or(original);
    (result, credentials)
}

/// The words for a failed request, in the reader's language.
pub fn error_message(error: &WeReadError) -> String {
    eprintln!("aristotle: weread: {error}");
    match error {
        // Core groups -2011 (dead credentials: scan again) and -2041
        // (finish the check in the official app) under the same variant;
        // the code tells them apart.
        WeReadError::ManualVerification(-2011) => tr(Msg::WereadErrAuth).to_string(),
        WeReadError::ManualVerification(_) => tr(Msg::WereadErrManual).to_string(),
        WeReadError::RateLimited(_) => tr(Msg::WereadErrRate).to_string(),
        WeReadError::AuthExpired | WeReadError::AuthRejected(_) | WeReadError::NotAuthenticated => {
            tr(Msg::WereadErrAuth).to_string()
        }
        WeReadError::Blocked(_) => tr(Msg::WereadErrBlocked).to_string(),
        WeReadError::TooLarge(_) => tr(Msg::WereadErrTooLarge).to_string(),
        WeReadError::Transport(_) => tr(Msg::WereadErrNetwork).to_string(),
        WeReadError::HttpStatus(status, _) if *status == 401 || *status == 403 => {
            tr(Msg::WereadErrAuth).to_string()
        }
        WeReadError::WebAuthExpired => tr(Msg::WereadErrWebAuth).to_string(),
        WeReadError::UnconfirmedWrite(_) => tr(Msg::WereadErrWriteUnconfirmed).to_string(),
        WeReadError::MissingReaderState(_) => tr(Msg::WereadErrServer).to_string(),
        WeReadError::HttpStatus(status, _) if *status == 429 => tr(Msg::WereadErrRate).to_string(),
        WeReadError::HttpStatus(_, _)
        | WeReadError::Api { .. }
        | WeReadError::InvalidResponse(_) => tr(Msg::WereadErrServer).to_string(),
        _ => tr(Msg::WereadErrUnknown).to_string(),
    }
}

/// The WeRead account and shelf, as the app remembers them.
pub struct WereadState {
    /// The login screen's code and words.
    pub login: ui_login::WereadLoginState,
    pub signed_in: bool,
    pub books: Vec<ui_shelf::WereadBook>,
    pub filter: ui_shelf::WereadFilter,
    pub loading: bool,
    /// A failure or a hint, in the reader's language.
    pub message: Option<String>,
    pub last_sync: Option<i64>,
    /// Identifies the current shelf request.
    pub token: u64,
    /// The server's shelf cursor, persisted with the account.
    pub synckey: Option<i64>,
    /// Downloads and the stored coordinate maps (download.rs).
    pub downloads: download::WereadDownloads,
    /// The Web cookies, sign-in and progress writes (web.rs).
    pub(crate) web: web::WereadWeb,
    /// The reading-time queue and upload timer (readtime.rs).
    pub(crate) readtime: readtime::WereadReadtime,
    /// The cloud highlights and thoughts cache (notes.rs).
    pub(crate) notes: notes::WereadNotes,
    credentials: Option<Credentials>,
    account: StoredAccount,
    loaded: bool,
    path: Option<PathBuf>,
    login_cancel: Option<Arc<AtomicBool>>,
    login_rotations: u32,
    /// The generation of the newest sign-in; older results are dropped.
    pub login_generation: u64,
}

impl Default for WereadState {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for WereadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WereadState")
            .field("signed_in", &self.signed_in)
            .field("books", &self.books.len())
            .field("filter", &self.filter)
            .field("loading", &self.loading)
            .field("message", &self.message)
            .field("synckey", &self.synckey)
            .field("downloads", &self.downloads)
            .field("web", &self.web)
            .field("readtime", &self.readtime)
            .field("notes", &self.notes)
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "[REDACTED]"),
            )
            .field("login", &self.login)
            .field("login_rotations", &self.login_rotations)
            .field("login_generation", &self.login_generation)
            .finish()
    }
}

impl WereadState {
    pub fn new() -> Self {
        Self {
            login: ui_login::WereadLoginState::new(),
            signed_in: false,
            books: Vec::new(),
            filter: ui_shelf::WereadFilter::All,
            loading: false,
            message: None,
            last_sync: None,
            token: 0,
            synckey: None,
            downloads: download::WereadDownloads::default(),
            web: web::WereadWeb::new(),
            readtime: readtime::WereadReadtime::new(),
            notes: notes::WereadNotes::new(),
            credentials: None,
            account: StoredAccount::default(),
            loaded: false,
            path: None,
            login_cancel: None,
            login_rotations: 0,
            login_generation: 0,
        }
    }

    /// Read the account once; later calls are free.
    pub fn ensure_loaded(&mut self, dir: &Path) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let path = store_path(dir);
        self.path = Some(path.clone());
        self.account = load_account(&path);
        if self.account.device_id.is_empty() {
            let (device_id, install_id) = fresh_identity();
            self.account.device_id = device_id;
            self.account.install_id = install_id;
        }
        self.credentials = self.account.credentials.as_ref().map(Credentials::from);
        if let Some(credentials) = self.credentials.as_mut() {
            if credentials.device_id.is_empty() {
                credentials.device_id = self.account.device_id.clone();
            }
        }
        self.synckey = self.account.synckey;
        self.last_sync = self.account.last_sync;
        self.signed_in = self.credentials.is_some();
        self.downloads.ensure_loaded(dir);
        self.web.ensure_loaded(dir);
        self.notes.ensure_loaded(dir);
    }

    /// The ids the next sign-in signs with.
    pub fn device_identity(&mut self, dir: &Path) -> (String, String) {
        self.ensure_loaded(dir);
        if self.account.device_id.is_empty() || self.account.install_id.is_empty() {
            let (device_id, install_id) = fresh_identity();
            if self.account.device_id.is_empty() {
                self.account.device_id = device_id;
            }
            self.account.install_id = install_id;
        }
        (
            self.account.device_id.clone(),
            self.account.install_id.clone(),
        )
    }

    /// Write the account after a sign-in or a shelf refresh.
    pub fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.account.credentials = self.credentials.as_ref().map(StoredCredentials::from);
        self.account.synckey = self.synckey;
        self.account.last_sync = self.last_sync;
        if let Err(error) = save_account(&path, &self.account) {
            eprintln!("aristotle: weread: can't save {path:?}: {error}");
            self.message = Some(tr(Msg::WereadErrStore).to_string());
        }
    }

    /// Keep the credentials the exchange produced.
    pub fn set_credentials(&mut self, credentials: Credentials) {
        if self.account.device_id.is_empty() {
            self.account.device_id = credentials.device_id.clone();
        }
        if self.account.install_id.is_empty() {
            self.account.install_id = fresh_identity().1;
        }
        self.credentials = Some(credentials);
        self.signed_in = true;
        self.login_rotations = 0;
    }

    /// Count one expired code; false when the screen should stop replacing
    /// it automatically.
    pub fn rotate_expired(&mut self) -> bool {
        if self.login_rotations >= MAX_QR_ROTATIONS {
            return false;
        }
        self.login_rotations += 1;
        true
    }

    /// The tokens are dead: drop them and the shelf, so the guide offers a
    /// new scan instead of a shelf that can never load.
    pub fn forget_credentials(&mut self) {
        self.cancel_login();
        self.downloads.cancel_all();
        self.credentials = None;
        self.account.credentials = None;
        self.account.synckey = None;
        // Cloud notes are account data: signing out forgets them too.
        let _ = self.notes.clear();
        self.books.clear();
        self.synckey = None;
        self.signed_in = false;
        self.save();
    }

    /// The account's credentials, for the workers.
    pub fn credentials(&self) -> Option<Credentials> {
        self.credentials.clone()
    }

    /// The generation of the Web stage that follows a successful E-Ink sign-in.
    pub fn begin_web_login(&mut self) -> u64 {
        self.cancel_login();
        let generation = self.login.generation.wrapping_add(1);
        self.login_generation = generation;
        generation
    }

    /// The generation of a new sign-in.
    pub fn begin_login(&mut self) -> u64 {
        self.cancel_login();
        self.login_generation = self.login.generation;
        self.login_generation
    }

    /// Start a new code, forget the old worker.
    pub fn begin_new_code(&mut self) -> u64 {
        let generation = self.login.restart();
        self.login_generation = generation;
        generation
    }

    /// Stop the worker of the code on the screen, if any.
    pub fn cancel_login(&mut self) {
        if let Some(cancel) = self.login_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    /// The worker that owns the current code.
    pub fn set_login_cancel(&mut self, cancel: Arc<AtomicBool>) {
        self.cancel_login();
        self.login_cancel = Some(cancel);
    }

    /// Whether a result belongs to the code on the screen.
    pub fn is_current_login(&self, generation: u64) -> bool {
        generation == self.login_generation
    }

    /// The login ended; nothing to cancel any more.
    pub fn finish_login(&mut self) {
        self.login_cancel = None;
    }

    /// Update the cached shelf card's displayed reading percentage for `book_id`.
    pub fn set_book_progress(&mut self, book_id: &str, progress: i64) {
        let clamped = progress.clamp(0, 100);
        if let Some(book) = self.books.iter_mut().find(|book| book.id == book_id) {
            book.progress = Some(clamped);
            if clamped >= 100 {
                book.finished = true;
            }
        }
    }

    /// The WeRead host names the device's firewall must allow.
    pub fn allow_entries() -> [(&'static str, &'static str); 4] {
        [
            ("i.weread.qq.com:443", "weread"),
            ("open.weixin.qq.com:443", "weread"),
            ("long.open.weixin.qq.com:443", "weread"),
            ("weread.qq.com:443", "weread"),
        ]
    }
}

// ------------------------------------------------------------------ app

impl App {
    /// Open WeRead: the shelf when signed in, the QR code otherwise.
    pub(crate) fn open_weread(&mut self) -> Result<(), Error> {
        self.weread.ensure_loaded(&self.net.paths.state);
        if self.weread.signed_in {
            if !self.weread.web.signed_in() {
                return self.start_weread_web_login();
            }
            self.show_weread_shelf(Refresh::Clean, 0)?;
            if !self.weread.loading {
                self.refresh_weread_shelf();
            }
            self.show_weread_shelf(Refresh::Auto, 0)?;
        } else {
            self.start_weread_login(Refresh::Full)?;
        }
        Ok(())
    }

    /// Ask the device's firewall for the WeRead hosts; a no-op when the
    /// agent is not installed or the network is off.
    fn allow_weread_hosts(&self) {
        let policy = net::Net::policy();
        let wanted: std::collections::BTreeSet<(String, &'static str)> =
            WereadState::allow_entries()
                .into_iter()
                .map(|(entry, feature)| (entry.to_string(), feature))
                .collect();
        for (entry, feature) in aristotle_net::policy::missing_entries(&policy, &wanted) {
            match system::net_allow(true, &entry, feature) {
                Ok(message) => crate::perf!("net: allowlist: {message}"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    crate::perf!("net: no agent, no allowlist to change for {entry}")
                }
                Err(error) => eprintln!("aristotle: allowlist {entry}: {error}"),
            }
        }
    }

    /// The QR sign-in screen, showing the code the worker produced.
    pub(crate) fn show_weread_login(&mut self, refresh: Refresh) -> Result<(), Error> {
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let mut tree = aristotle_ui::tree::Tree::new();
        let (root, _) = ui_login::build_weread_login_screen(
            &mut tree,
            &self.styles,
            &self.weread.login,
            &status,
        );
        tree.set_root(root);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::WereadLogin;
        self.render_ui()
    }

    /// First visit: make the code and show the screen.
    pub(crate) fn start_weread_login(&mut self, refresh: Refresh) -> Result<(), Error> {
        self.weread.begin_new_code();
        self.weread.login_rotations = 0;
        self.spawn_weread_login();
        self.show_weread_login(refresh)
    }

    /// Put the current code's worker on a thread.
    fn spawn_weread_login(&mut self) {
        let generation = self.weread.login_generation;
        let (device_id, install_id) = self.weread.device_identity(&self.net.paths.state);
        self.allow_weread_hosts();
        let policy = net::Net::policy();
        let cancel = Arc::new(AtomicBool::new(false));
        self.weread.set_login_cancel(cancel.clone());
        let request = LoginRequest {
            generation,
            device_id,
            device_name: DEFAULT_DEVICE_NAME.to_string(),
            install_id,
        };
        self.net.spawn("weread-login", move |tx| {
            run_login(policy, request, cancel, tx)
        });
    }

    /// A code or a phase update from the worker.
    pub(crate) fn on_weread_qr(
        &mut self,
        generation: u64,
        url: String,
        phase: ui_login::WereadLoginPhase,
    ) -> Result<(), Error> {
        if !self.weread.is_current_login(generation) {
            return Ok(());
        }
        self.weread.login.set_code(generation, &url);
        self.weread.login.set_phase(generation, phase);
        if matches!(self.screen, Screen::WereadLogin) {
            self.show_weread_login(Refresh::Auto)?;
        }
        Ok(())
    }

    /// The exchange succeeded: keep the credentials, then read the shelf.
    pub(crate) fn on_weread_logged_in(
        &mut self,
        generation: u64,
        credentials: Credentials,
    ) -> Result<(), Error> {
        if !self.weread.is_current_login(generation) {
            return Ok(());
        }
        self.weread.finish_login();
        self.weread.set_credentials(credentials);
        self.weread.save();
        self.weread.message = None;
        if !self.weread.web.signed_in() {
            return self.start_weread_web_login();
        }
        if matches!(self.screen, Screen::WereadLogin) {
            self.show_weread_shelf(Refresh::Clean, 0)?;
        }
        self.refresh_weread_shelf();
        if matches!(self.screen, Screen::WereadShelf(_)) {
            self.show_weread_shelf(Refresh::Auto, self.list_page())?;
        }
        Ok(())
    }

    /// The code expired; make another one, up to the cap.
    pub(crate) fn on_weread_qr_expired(&mut self, generation: u64) -> Result<(), Error> {
        if !self.weread.is_current_login(generation) {
            return Ok(());
        }
        self.weread.finish_login();
        if self.weread.rotate_expired() {
            self.weread
                .login
                .set_phase(generation, ui_login::WereadLoginPhase::Expired);
            if matches!(self.screen, Screen::WereadLogin) {
                self.show_weread_login(Refresh::Auto)?;
            }
            self.weread.begin_login();
            self.spawn_weread_login();
            if matches!(self.screen, Screen::WereadLogin) {
                self.show_weread_login(Refresh::Auto)?;
            }
        } else {
            self.weread
                .login
                .fail(generation, tr(Msg::WereadErrQrExpired).to_string());
            if matches!(self.screen, Screen::WereadLogin) {
                self.show_weread_login(Refresh::Auto)?;
            }
        }
        Ok(())
    }

    /// The sign-in failed (or was declined); show its words.
    pub(crate) fn on_weread_login_failed(
        &mut self,
        generation: u64,
        message: String,
    ) -> Result<(), Error> {
        if !self.weread.is_current_login(generation) {
            return Ok(());
        }
        self.weread.finish_login();
        self.weread.login.fail(generation, message);
        if matches!(self.screen, Screen::WereadLogin) {
            self.show_weread_login(Refresh::Auto)?;
        }
        Ok(())
    }

    /// The login worker stopped because the screen was left.
    pub(crate) fn on_weread_login_cancelled(&mut self, generation: u64) {
        if self.weread.is_current_login(generation) {
            self.weread.finish_login();
        }
    }

    /// Keep WeRead shelf card percentages synchronized with local library progress
    /// when a downloaded book has newer or non-zero local reading progress.
    fn sync_weread_shelf_progress(&mut self) {
        let last_sync = self.weread.last_sync;
        for book in &mut self.weread.books {
            let Some(hash) = self.weread.downloads.hash_of(&book.id) else {
                continue;
            };
            let Some(rec) = self.lib.book(&hash) else {
                continue;
            };
            if let Some(progress) = rec.progress.as_ref() {
                let local_pct = (progress.fraction.clamp(0.0, 1.0) * 100.0).floor() as i64;
                let stale = book.progress.is_none()
                    || book.progress == Some(0)
                    || last_sync.is_some_and(|sync| progress.updated > sync);
                if local_pct > 0 && stale {
                    book.progress = Some(local_pct);
                }
            }
            if rec.status == aristotle_library::index::Status::Finished {
                book.finished = true;
            }
        }
    }

    /// Rebuild the WeRead shelf from the books in memory.
    pub(crate) fn show_weread_shelf(&mut self, refresh: Refresh, page: usize) -> Result<(), Error> {
        self.weread.ensure_loaded(&self.net.paths.state);
        self.sync_weread_shelf_progress();
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let last_sync = self.weread.last_sync.map(crate::marks::local_time);
        let filter = self.weread.filter;
        let message = self
            .weread
            .message
            .as_deref()
            .or_else(|| self.weread.web.message())
            .or_else(|| self.weread.downloads.message());
        let view = ui_shelf::WereadShelfView {
            status,
            signed_in: self.weread.signed_in,
            book_count: self.weread.books.len(),
            local_books_count: self.lib.books().len(),
            last_sync: last_sync.as_deref(),
            filter,
            loading: self.weread.loading,
            message,
            page,
            books: &self.weread.books,
        };
        let width = self.ui.bounds().w;
        let channels = self.channel_visibility();
        let (tree, shelf) = ui_shelf::WereadShelfView::build_for_width_with_channels(
            &mut self.ui.text,
            &self.styles,
            &view,
            width,
            channels,
        );
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        shelf.sync(&mut self.ui.tree);
        self.screen = Screen::WereadShelf(Box::new(shelf));
        self.render_ui()
    }

    /// Ask for the shelf on a worker thread; the cursor goes with it.
    pub(crate) fn refresh_weread_shelf(&mut self) {
        if self.weread.loading {
            return;
        }
        let Some(credentials) = self.weread.credentials() else {
            return;
        };
        self.weread.loading = true;
        self.weread.message = None;
        self.weread.token = self.weread.token.wrapping_add(1);
        let token = self.weread.token;
        // Always request a full shelf snapshot: passing the previous synckey
        // returns an empty delta when nothing changed, which would wipe the
        // displayed shelf.
        let synckey = None;
        self.allow_weread_hosts();
        let request = ShelfRequest {
            credentials,
            synckey,
            policy: net::Net::policy(),
        };
        self.net.spawn("weread-shelf", move |_| {
            let (result, credentials) = fetch_shelf(request);
            Done::WereadShelf {
                token,
                credentials,
                result,
            }
        });
    }

    /// The shelf arrived: keep it, the cursor and the rotated credentials.
    pub(crate) fn on_weread_shelf(
        &mut self,
        token: u64,
        credentials: Credentials,
        result: Result<ShelfSync, inkread_core::weread::WeReadError>,
    ) -> Result<(), Error> {
        if token != self.weread.token {
            return Ok(());
        }
        self.weread.loading = false;
        self.weread.set_credentials(credentials);
        match result {
            Ok(sync) => {
                let unchanged_empty = sync.books.is_empty()
                    && !self.weread.books.is_empty()
                    && sync.synckey.is_some()
                    && sync.synckey == self.weread.synckey;
                if !unchanged_empty {
                    let books = shelf_books(&sync.books, &self.weread.downloads);
                    self.weread.books = books;
                }
                self.weread.synckey = sync.synckey;
                self.weread.last_sync = Some(now_secs());
                self.weread.message = None;
            }
            Err(error) => {
                if auth_terminal(&error) {
                    // The tokens are dead: offer the sign-in guide again.
                    self.weread.forget_credentials();
                }
                self.weread.message = Some(error_message(&error));
            }
        }
        self.weread.save();
        if matches!(self.screen, Screen::WereadShelf(_)) {
            let page = self.list_page();
            // A new list uses a non-flashing GLKW16 refresh; GhostBudget
            // triggers periodic GC16 cleanup when needed.
            self.show_weread_shelf(Refresh::Clean, page)?;
        }
        Ok(())
    }

    /// Events on the sign-in screen.
    pub(crate) fn on_weread_login_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        if self.weread.login.stage == ui_login::WereadLoginStage::Web {
            return self.on_weread_web_login_event(id);
        }
        match id {
            ui_login::action::BACK => {
                self.weread.cancel_login();
                if self.weread.signed_in {
                    self.show_weread_shelf(Refresh::Clean, 0)?;
                } else {
                    self.show_library(Refresh::Clean)?;
                }
            }
            ui_login::action::REFRESH => {
                self.start_weread_login(Refresh::Auto)?;
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// Events on the WeRead shelf.
    pub(crate) fn on_weread_shelf_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_shelf::action as a;
        if let UiEvent::Action { id, .. } = &ev {
            let id = *id;
            let card = if let Screen::WereadShelf(view) = &self.screen {
                view.book_at(id).map(str::to_string)
            } else {
                None
            };
            if let Some(book_id) = card {
                if (a::DOWNLOAD..a::DOWNLOAD + ui_shelf::PER_PAGE as u32).contains(&id) {
                    self.start_weread_download(&book_id)?;
                    return Ok(Flow::Continue);
                }
                if (a::CANCEL..a::CANCEL + ui_shelf::PER_PAGE as u32).contains(&id) {
                    self.cancel_weread_download(&book_id);
                    return Ok(Flow::Continue);
                }
                if (a::OPEN..a::OPEN + ui_shelf::PER_PAGE as u32).contains(&id) {
                    self.open_weread_book(&book_id)?;
                    return Ok(Flow::Continue);
                }
            }
        }
        match ev {
            UiEvent::Swipe(dir) => {
                let mut handled = false;
                if let Screen::WereadShelf(view) = &mut self.screen {
                    handled = view.handle_swipe(&mut self.ui.tree, dir);
                }
                if handled {
                    // Build the next page: three cards, no file, no network.
                    let page = self.list_page();
                    self.show_weread_shelf(Refresh::Auto, page)?;
                }
            }
            UiEvent::Action { id, .. } => match id {
                a::LOCAL => {
                    self.weread.cancel_login();
                    self.weread.downloads.cancel_all();
                    self.show_library(Refresh::Clean)?;
                }
                a::CLOUD => {
                    self.weread.cancel_login();
                    self.weread.downloads.cancel_all();
                    self.open_cloud()?;
                }
                a::JIANGUOYUN => {
                    self.weread.cancel_login();
                    self.weread.downloads.cancel_all();
                    self.open_jianguoyun()?;
                }
                a::LOGIN => {
                    self.weread.cancel_login();
                    self.weread.downloads.cancel_all();
                    self.start_weread_login(Refresh::Clean)?;
                }
                a::LOGOUT => {
                    self.weread.forget_credentials();
                    self.weread.web.forget_credentials(&self.net.paths.state);
                    self.weread.message = None;
                    self.start_weread_login(Refresh::Clean)?;
                }
                a::SYNC => {
                    let page = self.list_page();
                    self.refresh_weread_shelf();
                    self.show_weread_shelf(Refresh::Clean, page)?;
                }
                id if (a::FILTER..a::FILTER + ui_shelf::WereadFilter::ALL.len() as u32)
                    .contains(&id) =>
                {
                    let index = (id - a::FILTER) as usize;
                    self.weread.filter = ui_shelf::WereadFilter::ALL[index];
                    self.show_weread_shelf(Refresh::Clean, 0)?;
                }
                _ => {
                    let mut handled = false;
                    if let Screen::WereadShelf(view) = &mut self.screen {
                        handled = view.handle(&mut self.ui.tree, id);
                    }
                    if handled {
                        let page = self.list_page();
                        self.show_weread_shelf(Refresh::Auto, page)?;
                    }
                }
            },
            _ => {}
        }
        Ok(Flow::Continue)
    }
}

/// The shelf rows as the screen draws them. `archive` is a group or list
/// id: absent, null, zero and an empty string all mean "not in a list".
pub fn shelf_books(
    books: &[ShelfBook],
    downloads: &download::WereadDownloads,
) -> Vec<ui_shelf::WereadBook> {
    books
        .iter()
        .map(|book| ui_shelf::WereadBook {
            id: book.book_id.clone(),
            title: book
                .title
                .clone()
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| book.book_id.clone()),
            // The contract's shelf row has no author; the card's subtitle is
            // the format.
            format: book.format.clone().unwrap_or_default(),
            progress: book.progress,
            finished: book.finished.unwrap_or(false),
            archive: archived(book.archive.as_ref()),
            secret: book.secret.unwrap_or(false),
            download: downloads.state(&book.book_id),
        })
        .collect()
}

fn archived(value: Option<&serde_json::Value>) -> bool {
    use serde_json::Value;
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_i64().is_some_and(|number| number > 0),
        Some(Value::String(text)) => !text.is_empty() && text != "0",
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

#[cfg(test)]
#[path = "weread/tests.rs"]
mod tests;
