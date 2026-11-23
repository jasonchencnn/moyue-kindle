// SPDX-License-Identifier: AGPL-3.0-or-later
//! Web sign-in, progress arbitration and the close/sleep write (WR-004b).
//!
//! The E-Ink channel answers the shelf and the read-only position. Writing a
//! precise position needs the Web channel (`/web/book/read`, contract §14.4),
//! which is authenticated by browser cookies from a second QR code. That is
//! what this module owns:
//!
//! - a sign-in state machine with a deadline, an OTP step and a failure cap
//!   (never an unbounded poll loop), in [`login`];
//! - the `core::weread::arbiter` verdict at book open, which decides whether
//!   the close/sleep write is allowed at all;
//! - one write per close, never retried; an unconfirmed write is recorded and
//!   settled by the next cloud read;
//! - the E-Ink heartbeat, which re-reads `getProgress` and echoes its fields.
//!
//! Cookies live in `weread-web.json` next to the E-Ink account: 0700 folder,
//! 0600 file, and `Debug` never prints a value.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aristotle_i18n::{tr, Msg};
use aristotle_net::store::{private_dir, write_private};
use aristotle_net::{Policy, ReadestTransport};
use inkread_core::sync::transport::HttpTransport;
use inkread_core::weread::{
    arbitrate, enter_payload, read_payload, web_app_id, CookieJar, Credentials, PositionStamp,
    ReadPayloadInput, SyncBaseline, Verdict, WeReadClient, WeReadError, WeReadWebClient,
    WebLoginPoll, WriteFence, DEFAULT_READER_TOKEN, WEB_USER_AGENT,
};

use serde::{Deserialize, Serialize};

use crate::wake;

mod hooks;
mod login;
mod time;

use self::login::{LoginMachine, WebLoginPhase};
use self::time::local_offset;

/// The Web cookies file in the app's private folder.
pub const WEB_STORE_FILE: &str = "weread-web.json";
/// Heartbeat period inside a reading session.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);
/// Timeout of one Web or open request.
const WEB_TIMEOUT_SECS: u64 = 20;
/// Schema version of `weread-web.json`.
const WEB_STORE_VERSION: u32 = 1;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn random_u16() -> u16 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_micros())
        .unwrap_or(0)
        % 1000) as u16
}

fn stamp_array(stamp: PositionStamp) -> [i64; 3] {
    [stamp.chapter_uid, stamp.chapter_idx, stamp.chapter_offset]
}

fn stamp_from_array(value: [i64; 3]) -> PositionStamp {
    PositionStamp::new(value[0], value[1], value[2])
}

// ------------------------------------------------------------------ store

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredBook {
    #[serde(default)]
    last_local: Option<[i64; 3]>,
    #[serde(default)]
    synced_local: Option<[i64; 3]>,
    #[serde(default)]
    synced_remote: Option<[i64; 3]>,
    /// A write was sent but never confirmed; the next cloud read settles it.
    #[serde(default)]
    unconfirmed: bool,
}

impl StoredBook {
    fn baseline(&self) -> Option<SyncBaseline> {
        Some(SyncBaseline::new(
            stamp_from_array(self.synced_local?),
            stamp_from_array(self.synced_remote?),
        ))
    }

    fn last_local(&self) -> Option<PositionStamp> {
        self.last_local.map(stamp_from_array)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredSession {
    vid: String,
    logged_at: i64,
    cookies: CookieJar,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredWeb {
    #[serde(default = "store_version")]
    version: u32,
    #[serde(default)]
    session: Option<StoredSession>,
    #[serde(default)]
    invalid: bool,
    #[serde(default)]
    books: BTreeMap<String, StoredBook>,
}

fn store_version() -> u32 {
    WEB_STORE_VERSION
}

impl Default for StoredWeb {
    fn default() -> Self {
        Self {
            version: WEB_STORE_VERSION,
            session: None,
            invalid: false,
            books: BTreeMap::new(),
        }
    }
}

// ------------------------------------------------------------------- events

#[derive(Clone, Debug)]
pub(super) struct RemoteSnapshot {
    pub coordinate: PositionStamp,
    pub summary: String,
    pub progress: Option<i64>,
}

/// A remote cloud position that should jump the currently open reader.
#[derive(Clone, Debug)]
pub(super) struct RemoteJump {
    pub book_id: String,
    pub coordinate: PositionStamp,
    #[allow(dead_code)]
    pub summary: String,
    pub progress: Option<i64>,
    pub manual: bool,
}

#[derive(Debug)]
pub(super) enum WriteOutcome {
    Confirmed,
    Unconfirmed(String),
    AuthExpired,
    NotSent(String),
}

#[derive(Debug)]
enum WebEvent {
    LoginUid {
        generation: u64,
        uid: String,
        jar: CookieJar,
    },
    LoginFetchFailed {
        generation: u64,
        error: WeReadError,
    },
    LoginPoll {
        generation: u64,
        poll: Result<WebLoginPoll, WeReadError>,
        jar: CookieJar,
    },
    Opened {
        run: u64,
        remote: Option<RemoteSnapshot>,
        verdict: Verdict,
        credentials: Option<Credentials>,
        error: Option<WeReadError>,
        manual: bool,
    },
    WriteDone {
        run: u64,
        book_id: String,
        local: PositionStamp,
        outcome: WriteOutcome,
    },
    Heartbeat {
        run: u64,
        credentials: Option<Credentials>,
        error: Option<WeReadError>,
    },
}

struct Heartbeat {
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
struct PendingWrite {
    run: u64,
    book_id: String,
}

struct BookRun {
    run: u64,
    book_id: String,
    opened_at: Instant,
    credentials: Option<Credentials>,
    remote: Option<RemoteSnapshot>,
    heartbeat: Option<Heartbeat>,
    heartbeat_failed: bool,
    /// The writes of this reading session; `enter` opens it once (§14.4).
    write_session: WriteSession,
}

/// The app's Web state: cookies, the sign-in machine and the current book.
pub struct WereadWeb {
    loaded: bool,
    path: Option<PathBuf>,
    data: StoredWeb,
    login: Option<LoginMachine>,
    book: Option<BookRun>,
    pending: Option<PendingWrite>,
    pending_jump: Option<RemoteJump>,
    tx: wake::Sender<WebEvent>,
    rx: mpsc::Receiver<WebEvent>,
    run_seq: u64,
    message: Option<String>,
    invalid_message: Option<String>,
}

impl Default for WereadWeb {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for WereadWeb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WereadWeb")
            .field("signed_in", &self.signed_in())
            .field("login", &self.login.as_ref().map(|login| login.phase))
            .field(
                "book",
                &self.book.as_ref().map(|book| book.book_id.as_str()),
            )
            .field("pending", &self.pending.is_some())
            .field("message", &self.message)
            .finish()
    }
}

/// What the main loop has to do after draining the Web workers.
#[derive(Default)]
pub(super) struct WebReport {
    pub login_changed: bool,
    pub login_confirmed: bool,
    pub credentials: Option<Credentials>,
    pub message: Option<String>,
    pub remote_jump: Option<RemoteJump>,
    pub remote_progress: Option<(String, Option<i64>)>,
    pub manual_no_change: bool,
}

impl WereadWeb {
    /// An empty state with a live worker channel.
    pub fn new() -> Self {
        let (tx, rx) = wake::channel();
        Self {
            loaded: false,
            path: None,
            data: StoredWeb::default(),
            login: None,
            book: None,
            pending: None,
            pending_jump: None,
            tx,
            rx,
            run_seq: 0,
            message: None,
            invalid_message: None,
        }
    }

    /// Drop the Web session and delete the stored cookie file so the user can
    /// switch accounts cleanly.
    pub fn forget_credentials(&mut self, dir: &Path) {
        self.login = None;
        self.book = None;
        self.pending = None;
        self.pending_jump = None;
        self.message = None;
        self.invalid_message = None;
        self.data = StoredWeb::default();
        self.loaded = true;
        let path = dir.join(WEB_STORE_FILE);
        self.path = Some(path.clone());
        let _ = std::fs::remove_file(path);
    }

    pub fn ensure_loaded(&mut self, dir: &Path) {
        let path = dir.join(WEB_STORE_FILE);
        if self.loaded && self.path.as_deref() == Some(path.as_path()) {
            return;
        }
        self.loaded = true;
        self.path = Some(path.clone());
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoredWeb>(&bytes) {
                Ok(mut data) if data.version == WEB_STORE_VERSION => {
                    data.session = data.session.filter(|session| {
                        session.cookies.get("wr_vid").is_some()
                            && session.cookies.get("wr_skey").is_some()
                    });
                    self.data = data;
                }
                Ok(_) | Err(_) => {
                    eprintln!("aristotle: weread: ignoring {path:?} (bad schema)");
                    self.data = StoredWeb::default();
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.data = StoredWeb::default();
            }
            Err(error) => {
                eprintln!("aristotle: weread: can't read {path:?}: {error}");
                self.data = StoredWeb::default();
            }
        }
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if let Some(dir) = path.parent() {
            if let Err(error) = private_dir(dir) {
                eprintln!("aristotle: weread: can't create {dir:?}: {error}");
                return;
            }
        }
        let Ok(mut bytes) = serde_json::to_vec_pretty(&self.data) else {
            return;
        };
        bytes.push(b'\n');
        if let Err(error) = write_private(&path, &bytes) {
            eprintln!("aristotle: weread: can't save {path:?}: {error}");
        }
    }

    /// Whether a usable Web cookie jar is stored.
    pub fn signed_in(&self) -> bool {
        !self.data.invalid
            && self.data.session.as_ref().is_some_and(|session| {
                session.cookies.get("wr_vid").is_some() && session.cookies.get("wr_skey").is_some()
            })
    }

    /// Whether the close/sleep position write is still in flight. The
    /// reading-time upload waits for it: its own write moves the cloud
    /// position, and comparing before/after across that write would look like
    /// another device.
    pub(crate) fn write_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// A hint for the shelf or the sign-in screen.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Keep a confirmed session and forget the previous expiry flag.
    fn apply_session(&mut self, success: &inkread_core::weread::WebLoginSuccess, jar: CookieJar) {
        self.data.session = Some(StoredSession {
            vid: success.vid.clone(),
            logged_at: now_secs(),
            cookies: jar.persistent(),
        });
        self.data.invalid = false;
        self.invalid_message = None;
        self.save();
    }

    /// The cookies are dead: keep the account but stop every write until a
    /// new scan (§14.7). The caller shows [`WereadWeb::message`] next to the
    /// shelf.
    pub fn invalidate(&mut self, message: String) {
        self.data.invalid = true;
        self.invalid_message = Some(message.clone());
        self.message = Some(message);
        self.save();
    }

    // ---------------------------------------------------------------- login

    pub(super) fn start_login(
        &mut self,
        dir: &Path,
        policy: Policy,
        generation: u64,
        now: Instant,
    ) {
        self.ensure_loaded(dir);
        self.login = Some(LoginMachine::new(generation, now));
        self.message = None;
        let transport = ReadestTransport::with_timeout(policy, WEB_TIMEOUT_SECS);
        let tx = self.tx.clone();
        let spawned = thread::Builder::new()
            .name("weread-web-login".into())
            .spawn(move || {
                let mut client = WeReadWebClient::new(transport);
                let result = client.login_page().and_then(|_| client.login_uid());
                let jar = client.into_jar();
                let event = match result {
                    Ok(uid) => WebEvent::LoginUid {
                        generation,
                        uid,
                        jar,
                    },
                    Err(error) => WebEvent::LoginFetchFailed { generation, error },
                };
                let _ = tx.send(event);
            });
        if let Err(error) = spawned {
            eprintln!("aristotle: weread: can't start the web sign-in: {error}");
            if let Some(login) = self.login.as_mut() {
                login.phase = WebLoginPhase::Failed;
            }
        }
    }

    pub(super) fn cancel_login(&mut self) {
        self.login = None;
    }

    pub(super) fn login_phase(&self) -> Option<WebLoginPhase> {
        self.login.as_ref().map(|login| login.phase)
    }

    pub(super) fn login_otp(&self) -> &str {
        self.login.as_ref().map(LoginMachine::otp).unwrap_or("")
    }

    pub(super) fn confirm_url(&self) -> Option<String> {
        self.login.as_ref().and_then(LoginMachine::confirm_url)
    }

    pub(super) fn push_otp_digit(&mut self, digit: char) -> bool {
        self.login
            .as_mut()
            .is_some_and(|login| login.push_digit(digit))
    }

    pub(super) fn delete_otp_digit(&mut self) -> bool {
        self.login
            .as_mut()
            .is_some_and(|login| login.delete_digit())
    }

    pub(super) fn submit_otp(&mut self, digits: &str, now: Instant) -> bool {
        self.login
            .as_mut()
            .is_some_and(|login| login.submit_otp(digits, now))
    }

    /// Spawn the request the machine wants now, if any.
    pub(super) fn tick_login(&mut self, policy: &Policy, now: Instant) {
        let Some(login) = self.login.as_mut() else {
            return;
        };
        let Some(request) = login.tick(now) else {
            return;
        };
        let generation = login.generation;
        let jar = login.jar.clone();
        let transport = ReadestTransport::with_timeout(policy.clone(), WEB_TIMEOUT_SECS);
        let tx = self.tx.clone();
        let spawned = thread::Builder::new()
            .name("weread-web-poll".into())
            .spawn(move || {
                let mut client = WeReadWebClient::with_jar(transport, jar);
                let result = client.login_poll(&request.uid, &request.otp);
                let jar = client.into_jar();
                let _ = tx.send(WebEvent::LoginPoll {
                    generation,
                    poll: result,
                    jar,
                });
            });
        if let Err(error) = spawned {
            eprintln!("aristotle: weread: can't poll the web sign-in: {error}");
            login.in_flight = false;
            login.phase = WebLoginPhase::Failed;
        }
    }

    // ------------------------------------------------------------ book/sync

    /// Begin a WeRead reading session for the book hash, if it is one.
    pub(super) fn begin_book(
        &mut self,
        dir: &Path,
        hash: &str,
        credentials: Option<Credentials>,
        policy: Policy,
        now: Instant,
    ) {
        let _ = self.begin_book_inner(dir, hash, credentials, policy, now, false);
    }

    /// Manually pull cloud progress for the open WeRead book, updating the sync
    /// baseline and returning a jump target via [`WebReport::remote_jump`].
    pub(super) fn pull_book_manual(
        &mut self,
        dir: &Path,
        hash: &str,
        credentials: Option<Credentials>,
        policy: Policy,
        now: Instant,
    ) -> Result<(), String> {
        self.begin_book_inner(dir, hash, credentials, policy, now, true)
    }

    fn begin_book_inner(
        &mut self,
        dir: &Path,
        hash: &str,
        credentials: Option<Credentials>,
        policy: Policy,
        now: Instant,
        manual: bool,
    ) -> Result<(), String> {
        self.ensure_loaded(dir);
        let Some(credentials) = credentials else {
            return Err(tr(Msg::WereadErrAuth).to_string());
        };
        let Some(book_id) = book_id_for_hash(dir, hash) else {
            return Err(tr(Msg::WereadNotesNotWeread).to_string());
        };
        if self.signed_in() {
            self.message = self.invalid_message.take();
        }
        self.pending_jump = None;
        let run = if manual && self.book.as_ref().is_some_and(|b| b.book_id == book_id) {
            let book = self.book.as_mut().expect("checked above");
            book.credentials = Some(credentials.clone());
            book.run
        } else {
            self.stop_heartbeat();
            self.run_seq = self.run_seq.wrapping_add(1);
            let run = self.run_seq;
            self.book = Some(BookRun {
                run,
                book_id: book_id.clone(),
                opened_at: now,
                credentials: Some(credentials.clone()),
                remote: None,
                heartbeat: None,
                heartbeat_failed: false,
                write_session: WriteSession::default(),
            });
            run
        };
        let stored = self.data.books.get(&book_id).cloned().unwrap_or_default();
        let baseline = stored.baseline();
        let last_local = stored.last_local();
        let tx = self.tx.clone();
        let spawned = thread::Builder::new()
            .name("weread-web-open".into())
            .spawn(move || {
                let transport = ReadestTransport::with_timeout(policy, WEB_TIMEOUT_SECS);
                let mut client = WeReadClient::new(transport, credentials);
                let result = client.get_progress(&book_id);
                let updated = client.credentials().cloned();
                let (remote, error) = match result {
                    Ok(response) => {
                        let remote = response.book.as_ref().and_then(|book| {
                            book.position().map(|position| RemoteSnapshot {
                                coordinate: PositionStamp::new(
                                    position.chapter_uid,
                                    position.chapter_idx,
                                    position.chapter_offset,
                                ),
                                summary: book.summary.clone().unwrap_or_default(),
                                progress: book.progress,
                            })
                        });
                        (remote, None)
                    }
                    Err(error) => (None, Some(error)),
                };
                let verdict = match (&remote, last_local) {
                    (Some(remote), Some(local)) => arbitrate(local, remote.coordinate, baseline),
                    (Some(_), None) => Verdict::RemoteNewer,
                    (None, _) => Verdict::Aligned,
                };
                let _ = tx.send(WebEvent::Opened {
                    run,
                    remote,
                    verdict,
                    credentials: updated,
                    error,
                    manual,
                });
            });
        if let Err(error) = spawned {
            eprintln!("aristotle: weread: can't start the progress read: {error}");
            if !manual {
                self.book = None;
            }
            return Err(tr(Msg::WereadErrUnknown).to_string());
        }
        Ok(())
    }

    pub(super) fn set_pending_jump(&mut self, jump: RemoteJump) {
        self.pending_jump = Some(jump);
    }

    pub(super) fn peek_pending_jump(&self) -> Option<&RemoteJump> {
        self.pending_jump.as_ref()
    }

    pub(super) fn take_pending_jump(&mut self) -> Option<RemoteJump> {
        self.pending_jump.take()
    }

    /// Record a freshly pulled remote coordinate as the synced arbitration baseline.
    pub(super) fn record_synced_remote(&mut self, book_id: &str, coordinate: PositionStamp) {
        let mut stored = self.data.books.get(book_id).cloned().unwrap_or_default();
        stored.synced_local = Some(stamp_array(coordinate));
        stored.synced_remote = Some(stamp_array(coordinate));
        stored.last_local = Some(stamp_array(coordinate));
        stored.unconfirmed = false;
        self.data.books.insert(book_id.to_string(), stored);
        self.save();
    }

    /// Close the reading session: decide the verdict and, only for
    /// [`Verdict::LocalNewer`], send exactly one `period` write.
    pub(super) fn close_book(
        &mut self,
        dir: &Path,
        policy: Policy,
        xpointer: Option<&str>,
        fraction: f32,
    ) -> bool {
        self.close_book_inner(dir, policy, xpointer, fraction, false)
    }

    /// The sleep/exit variant: the write runs before the radio is torn down.
    pub(super) fn close_book_blocking(
        &mut self,
        dir: &Path,
        policy: Policy,
        xpointer: Option<&str>,
        fraction: f32,
    ) -> bool {
        self.close_book_inner(dir, policy, xpointer, fraction, true)
    }

    /// Manually push the open book's current position to WeRead Web, explicitly
    /// overriding the automatic write fence at the user's request.
    pub(super) fn force_push_book(
        &mut self,
        dir: &Path,
        policy: Policy,
        book_id: &str,
        xpointer: Option<&str>,
        fraction: f32,
    ) -> Result<(), String> {
        self.ensure_loaded(dir);
        let Some(cookies) = self
            .data
            .session
            .as_ref()
            .filter(|_| self.signed_in())
            .map(|session| session.cookies.clone())
        else {
            return Err(tr(Msg::ReaderWereadNeedWebLogin).to_string());
        };
        let Some(local) = xpointer.and_then(|xp| local_coordinate(dir, book_id, xp)) else {
            return Err(tr(Msg::WereadPositionUnavailable).to_string());
        };
        let mut stored = self.data.books.get(book_id).cloned().unwrap_or_default();
        stored.last_local = Some(stamp_array(local));
        let (run, elapsed, summary, write_session) = match &self.book {
            Some(book) if book.book_id == book_id => (
                book.run,
                book.opened_at.elapsed().as_secs() as i64,
                book.remote
                    .as_ref()
                    .map(|r| r.summary.clone())
                    .unwrap_or_default(),
                book.write_session.clone(),
            ),
            _ => (0, 0, String::new(), WriteSession::default()),
        };
        let progress = (fraction.clamp(0.0, 1.0) * 100.0).floor() as i64;
        self.spawn_write(
            book_id,
            run,
            local,
            progress,
            elapsed,
            summary,
            cookies,
            policy,
            write_session,
            stored,
        );
        Ok(())
    }

    fn close_book_inner(
        &mut self,
        dir: &Path,
        policy: Policy,
        xpointer: Option<&str>,
        fraction: f32,
        blocking: bool,
    ) -> bool {
        self.stop_heartbeat();
        self.pending_jump = None;
        let Some(book) = self.book.take() else {
            return false;
        };
        let write_session = book.write_session;
        let Some(local) =
            xpointer.and_then(|xpointer| local_coordinate(dir, &book.book_id, xpointer))
        else {
            return false;
        };
        let mut stored = self
            .data
            .books
            .get(&book.book_id)
            .cloned()
            .unwrap_or_default();
        stored.last_local = Some(stamp_array(local));
        let Some(remote) = book.remote.as_ref() else {
            self.data.books.insert(book.book_id, stored);
            self.save();
            return false;
        };
        let baseline = stored.baseline();
        let verdict = arbitrate(local, remote.coordinate, baseline);
        let dirty = baseline.is_none_or(|baseline| local != baseline.local);
        let mut fence = WriteFence::new(verdict, dirty);
        if !fence.allows_write() {
            self.data.books.insert(book.book_id, stored);
            self.save();
            if verdict == Verdict::Conflict {
                self.message = Some(tr(Msg::WereadProgressConflict).to_string());
            }
            return false;
        }
        let Some(cookies) = self
            .data
            .session
            .as_ref()
            .filter(|_| self.signed_in())
            .map(|session| session.cookies.clone())
        else {
            self.data.books.insert(book.book_id, stored);
            self.save();
            return false;
        };
        fence.begin_write();
        let progress = (fraction.clamp(0.0, 1.0) * 100.0).floor() as i64;
        let elapsed = book.opened_at.elapsed().as_secs() as i64;
        if blocking {
            let mut write_session = write_session;
            let outcome = write_once(
                &policy,
                cookies,
                &book.book_id,
                local,
                progress,
                elapsed,
                &remote.summary,
                &mut write_session,
            );
            let mut report = WebReport::default();
            self.absorb_write(&book.book_id, local, outcome, &mut report);
            if let Some(message) = report.message {
                self.message = Some(message);
            }
            true
        } else {
            self.spawn_write(
                &book.book_id,
                book.run,
                local,
                progress,
                elapsed,
                remote.summary.clone(),
                cookies,
                policy,
                write_session,
                stored,
            );
            true
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_write(
        &mut self,
        book_id: &str,
        run: u64,
        local: PositionStamp,
        progress: i64,
        elapsed: i64,
        summary: String,
        jar: CookieJar,
        policy: Policy,
        write_session: WriteSession,
        stored: StoredBook,
    ) {
        self.data.books.insert(book_id.to_string(), stored);
        self.save();
        self.pending = Some(PendingWrite {
            run,
            book_id: book_id.to_string(),
        });
        let tx = self.tx.clone();
        let book_id = book_id.to_string();
        let spawned = thread::Builder::new()
            .name("weread-web-write".into())
            .spawn(move || {
                let mut write_session = write_session;
                let outcome = write_once(
                    &policy,
                    jar,
                    &book_id,
                    local,
                    progress,
                    elapsed,
                    &summary,
                    &mut write_session,
                );
                let _ = tx.send(WebEvent::WriteDone {
                    run,
                    book_id,
                    local,
                    outcome,
                });
            });
        if let Err(error) = spawned {
            eprintln!("aristotle: weread: can't start the progress write: {error}");
            self.pending = None;
        }
    }

    fn stop_heartbeat(&mut self) {
        if let Some(book) = self.book.as_mut() {
            if let Some(heartbeat) = book.heartbeat.take() {
                heartbeat.cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Start the E-Ink heartbeat once the reader really shows this book.
    pub(super) fn start_heartbeat(&mut self, policy: Policy) {
        let tx = self.tx.clone();
        let Some(book) = self.book.as_mut() else {
            return;
        };
        if book.heartbeat.is_some() || book.heartbeat_failed {
            return;
        }
        let Some(credentials) = book.credentials.clone() else {
            return;
        };
        let book_id = book.book_id.clone();
        let run = book.run;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let time_zone = local_offset(now_secs());
        let spawned = thread::Builder::new()
            .name("weread-heartbeat".into())
            .spawn(move || {
                let mut next = Instant::now() + HEARTBEAT_INTERVAL;
                while !worker_cancel.load(Ordering::SeqCst) {
                    let now = Instant::now();
                    if now < next {
                        thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    next = now + HEARTBEAT_INTERVAL;
                    let transport =
                        ReadestTransport::with_timeout(policy.clone(), WEB_TIMEOUT_SECS);
                    let mut client = WeReadClient::new(transport, credentials.clone());
                    let result =
                        client.book_read_echo(&book_id, now_secs(), random_u16(), &time_zone);
                    let updated = client.credentials().cloned();
                    let error = result.err();
                    if tx
                        .send(WebEvent::Heartbeat {
                            run,
                            credentials: updated,
                            error,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        if let Err(error) = spawned {
            eprintln!("aristotle: weread: can't start the heartbeat: {error}");
            return;
        }
        book.heartbeat = Some(Heartbeat { cancel });
    }

    // ------------------------------------------------------------------ poll

    /// Drain the workers and drive the login machine one step.
    pub(super) fn poll(&mut self, now: Instant) -> WebReport {
        let mut report = WebReport::default();
        let before = self.login_phase();
        let mut polled: Vec<(u64, Result<WebLoginPoll, WeReadError>, CookieJar)> = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            match event {
                WebEvent::LoginUid {
                    generation,
                    uid,
                    jar,
                } => {
                    if let Some(login) = self.login.as_mut() {
                        if login.generation == generation {
                            login.on_fetched(uid, jar, now);
                        }
                    }
                }
                WebEvent::LoginFetchFailed { generation, error } => {
                    if let Some(login) = self.login.as_mut() {
                        if login.generation == generation {
                            login.on_fetch_failed(&error);
                            if matches!(login.phase, WebLoginPhase::Failed | WebLoginPhase::Expired)
                            {
                                report.message = Some(super::error_message(&error));
                            }
                        }
                    }
                }
                WebEvent::LoginPoll {
                    generation,
                    poll,
                    jar,
                } => polled.push((generation, poll, jar)),
                WebEvent::Opened {
                    run,
                    remote,
                    verdict,
                    credentials,
                    error,
                    manual,
                } => {
                    let book_id = self
                        .book
                        .as_ref()
                        .filter(|book| book.run == run)
                        .map(|book| book.book_id.clone());
                    if let Some(book) = self.book.as_mut() {
                        if book.run == run {
                            if remote.is_some() || !manual {
                                book.remote = remote.clone();
                            }
                            if credentials.is_some() {
                                book.credentials = credentials.clone();
                            }
                        }
                    }
                    if credentials.is_some() {
                        report.credentials = credentials;
                    }
                    if let Some(error) = error {
                        report.message = Some(super::error_message(&error));
                    } else if let Some(book_id) = book_id {
                        if let Some(remote) = remote {
                            report.remote_progress = Some((book_id.clone(), remote.progress));
                            if manual {
                                self.record_synced_remote(&book_id, remote.coordinate);
                            } else {
                                self.absorb_open(&book_id, remote.clone(), verdict);
                            }
                            if verdict == Verdict::RemoteNewer || manual {
                                report.remote_jump = Some(RemoteJump {
                                    book_id,
                                    coordinate: remote.coordinate,
                                    summary: remote.summary,
                                    progress: remote.progress,
                                    manual,
                                });
                            }
                        } else if manual {
                            report.manual_no_change = true;
                        }
                    }
                }
                WebEvent::WriteDone {
                    run,
                    book_id,
                    local,
                    outcome,
                } => {
                    let expected = self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.run == run && pending.book_id == book_id);
                    if expected {
                        self.pending = None;
                        self.absorb_write(&book_id, local, outcome, &mut report);
                    }
                }
                WebEvent::Heartbeat {
                    run,
                    credentials,
                    error,
                } => {
                    if let Some(book) = self.book.as_mut() {
                        if book.run == run {
                            if credentials.is_some() {
                                book.credentials = credentials.clone();
                            }
                            if let Some(error) = error {
                                book.heartbeat = None;
                                if matches!(
                                    error,
                                    WeReadError::AuthExpired
                                        | WeReadError::AuthRejected(-2011)
                                        | WeReadError::ManualVerification(_)
                                ) {
                                    book.heartbeat_failed = true;
                                    report.message = Some(super::error_message(&error));
                                }
                            }
                        }
                    }
                    if credentials.is_some() {
                        report.credentials = credentials;
                    }
                }
            }
        }
        let mut confirmed: Option<(inkread_core::weread::WebLoginSuccess, CookieJar)> = None;
        let mut failed_message: Option<String> = None;
        if let Some(login) = self.login.as_mut() {
            for (generation, poll, jar) in polled {
                if login.generation != generation {
                    continue;
                }
                match poll {
                    Ok(poll) => {
                        let session_jar = jar.clone();
                        if let Some(success) = login.on_poll(poll, jar, now) {
                            confirmed = Some((success, session_jar));
                        }
                    }
                    Err(error) => {
                        login.on_poll_failed(&error, now);
                        if matches!(login.phase, WebLoginPhase::Failed | WebLoginPhase::Expired) {
                            failed_message = Some(super::error_message(&error));
                        }
                    }
                }
            }
        }
        if let Some(message) = failed_message {
            report.message = Some(message);
        }
        if let Some((success, jar)) = confirmed {
            self.apply_session(&success, jar);
            report.login_confirmed = true;
        }
        report.login_changed = before != self.login_phase();
        report.message = report.message.take().or_else(|| self.message.take());
        report
    }

    fn absorb_open(&mut self, book_id: &str, remote: RemoteSnapshot, verdict: Verdict) {
        let mut stored = self.data.books.get(book_id).cloned().unwrap_or_default();
        if stored.unconfirmed {
            if stored.last_local() == Some(remote.coordinate) {
                stored.synced_local = Some(stamp_array(remote.coordinate));
                stored.synced_remote = Some(stamp_array(remote.coordinate));
            }
            stored.unconfirmed = false;
            self.data.books.insert(book_id.to_string(), stored);
            self.save();
            return;
        }
        if matches!(verdict, Verdict::RemoteNewer | Verdict::Aligned) {
            stored.synced_local = Some(stamp_array(remote.coordinate));
            stored.synced_remote = Some(stamp_array(remote.coordinate));
            stored.last_local = Some(stamp_array(remote.coordinate));
            self.data.books.insert(book_id.to_string(), stored);
            self.save();
        }
    }

    fn absorb_write(
        &mut self,
        book_id: &str,
        local: PositionStamp,
        outcome: WriteOutcome,
        report: &mut WebReport,
    ) {
        let mut stored = self.data.books.get(book_id).cloned().unwrap_or_default();
        match outcome {
            WriteOutcome::Confirmed => {
                stored.synced_local = Some(stamp_array(local));
                stored.synced_remote = Some(stamp_array(local));
                stored.last_local = Some(stamp_array(local));
                stored.unconfirmed = false;
                report.message = Some(tr(Msg::WereadProgressWritten).to_string());
            }
            WriteOutcome::Unconfirmed(detail) => {
                stored.unconfirmed = true;
                report.message = Some(tr(Msg::WereadErrWriteUnconfirmed).to_string());
                eprintln!("aristotle: weread: unconfirmed progress write: {detail}");
            }
            WriteOutcome::AuthExpired => {
                self.invalidate(tr(Msg::WereadErrWebAuth).to_string());
                report.message = self.message.clone();
            }
            WriteOutcome::NotSent(detail) => {
                report.message = Some(tr(Msg::WereadErrNetwork).to_string());
                eprintln!("aristotle: weread: progress write not sent: {detail}");
            }
        }
        self.data.books.insert(book_id.to_string(), stored);
        self.save();
    }
}

fn book_id_for_hash(dir: &Path, hash: &str) -> Option<String> {
    let store =
        inkread_core::weread::BookMappingStore::load(dir.join("weread").join("books.json")).ok()?;
    let found = store
        .iter()
        .find(|(_, mapping)| mapping.hash.eq_ignore_ascii_case(hash))
        .map(|(book_id, _)| book_id.to_string());
    found
}

fn local_coordinate(dir: &Path, book_id: &str, xpointer: &str) -> Option<PositionStamp> {
    let positions = super::download::PositionStore::load_book(dir, book_id)?;
    let normalized = xpointer.strip_prefix("xpointer:").unwrap_or(xpointer);
    let epub_pos = positions.anchors.char_offset_of(normalized).or_else(|| {
        positions
            .map
            .from_xpointer(normalized, Some(&positions.anchors))
            .ok()
            .map(|mapped| mapped.position)
    })?;
    let mapped = positions.map.to_weread(epub_pos).ok()?;
    Some(PositionStamp::new(
        mapped.coordinate.chapter_uid,
        mapped.coordinate.chapter_idx,
        mapped.coordinate.chapter_offset,
    ))
}

/// The write state of one reading session. Contract §14.4: `enter` opens the
/// session, so only the first write that reaches the opening sends it; later
/// writes carry the `period` fields alone. Nothing here retries.
#[derive(Clone, Debug, Default)]
struct WriteSession {
    /// The session opened; a later write must not send another `enter`.
    entered: bool,
}

impl WriteSession {
    /// One write over any transport: the first opens with `enter`, every later
    /// one sends only `period`.
    fn write<T: HttpTransport>(
        &mut self,
        transport: T,
        jar: CookieJar,
        book_id: &str,
        local: PositionStamp,
        progress: i64,
        elapsed: i64,
        summary: &str,
    ) -> WriteOutcome {
        write_with(
            transport, jar, book_id, local, progress, elapsed, summary, self,
        )
    }
}

/// One session write: read the reader page for `psvts` and send `enter` the
/// first time this session reaches the opening, then one `period`. Nothing
/// here retries.
fn write_once(
    policy: &Policy,
    jar: CookieJar,
    book_id: &str,
    local: PositionStamp,
    progress: i64,
    elapsed: i64,
    summary: &str,
    session: &mut WriteSession,
) -> WriteOutcome {
    let transport = ReadestTransport::with_timeout(policy.clone(), WEB_TIMEOUT_SECS);
    session.write(transport, jar, book_id, local, progress, elapsed, summary)
}

/// [`write_once`] over any transport: tests drive it with recorded answers.
fn write_with<T: HttpTransport>(
    transport: T,
    jar: CookieJar,
    book_id: &str,
    local: PositionStamp,
    progress: i64,
    elapsed: i64,
    summary: &str,
    session: &mut WriteSession,
) -> WriteOutcome {
    let mut client = WeReadWebClient::with_jar(transport, jar);
    let state = match client.reader_state(book_id) {
        Ok(state) => state,
        Err(error) => {
            return match error {
                WeReadError::WebAuthExpired => WriteOutcome::AuthExpired,
                other => WriteOutcome::NotSent(super::error_message(&other)),
            }
        }
    };
    let mut input = ReadPayloadInput::new(book_id, local.chapter_uid);
    input.chapter_idx = local.chapter_idx;
    input.chapter_offset = local.chapter_offset;
    input.progress = progress;
    input.summary = summary.to_string();
    input.psvts = state.psvts;
    input.pclts = state.pclts;
    input.token = state
        .token
        .filter(|token| !token.is_empty())
        .unwrap_or_else(|| DEFAULT_READER_TOKEN.to_string());
    input.app_id = web_app_id(WEB_USER_AGENT);
    input.now_s = now_secs();
    input.elapsed_s = elapsed;
    input.ts_ms = now_ms();
    input.rn = random_u16() % 1000;
    // §14.4: only the first write of a reading session opens it with the
    // `enter` payload (no `rt/ts/rn/sg`); later writes carry the time fields
    // in `period` alone. Mark the session before the request leaves: an
    // unconfirmed `enter` still opened it, so a later write must not resend it.
    if !session.entered {
        session.entered = true;
        let enter = enter_payload(&input);
        match client.write_enter(book_id, local.chapter_uid, &enter) {
            Ok(()) => {}
            Err(WeReadError::WebAuthExpired) => return WriteOutcome::AuthExpired,
            Err(WeReadError::UnconfirmedWrite(detail)) => return WriteOutcome::Unconfirmed(detail),
            Err(error) => return WriteOutcome::NotSent(super::error_message(&error)),
        }
    }
    let payload = read_payload(&input);
    match client.write_period(book_id, local.chapter_uid, &payload) {
        Ok(()) => WriteOutcome::Confirmed,
        Err(WeReadError::WebAuthExpired) => WriteOutcome::AuthExpired,
        Err(WeReadError::UnconfirmedWrite(detail)) => WriteOutcome::Unconfirmed(detail),
        Err(error) => WriteOutcome::NotSent(super::error_message(&error)),
    }
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod tests;
