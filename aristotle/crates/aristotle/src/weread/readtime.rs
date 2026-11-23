// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading-time upload to WeRead (WR-005, contract §14.5/§14.6).
//!
//! The statistics recorder (`stats.rs`) stays untouched: this module listens
//! to the same page-stay signals in `main.rs` and accumulates an independent
//! queue. Nothing is read back from `stats-*.jsonl`, so turning the switch on
//! never backfills history; only stays seen while it is on are queued.
//!
//! # Durability
//!
//! The queue is one JSON file written like `library.json` and
//! `positions.json`: `weread-readtime.json.part` is `fsync`ed, the previous
//! generation is rotated to `.bak`, the part file is renamed into place and
//! the folder is `fsync`ed. A torn main file loads from `.bak`.
//!
//! # Receipts and pauses
//!
//! A batch is written to the queue as in flight before it is sent. Only
//! `succ:1` clears it; no receipt keeps it as unconfirmed and it is never
//! taken again. A transport failure of the pre-flight `getProgress` means the
//! batch never left and is put back. `-10102/-2014` cools down and retries;
//! `-2011/-2041/401` pauses until the reader signs in again. If the cloud
//! position moved between the pre-flight read and the read after the upload,
//! the queue pauses and the settings row offers a manual resume.

use std::fmt;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aristotle_i18n::{tf, tr, Msg};
use aristotle_net::{Policy, ReadestTransport};
use inkread_core::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
use inkread_core::weread::{
    intervals_for, BatchUploadRequest, BookMappingStore, Credentials, PauseReason, QueuedSpan,
    ReadtimeQueue, StaySpan, WeReadClient, WeReadError, MAX_BATCH_SECS, READ_TIME_ZONE,
};

use crate::{wake, App, Screen};

/// The queue file in the app's private folder.
pub const READTIME_FILE: &str = "weread-readtime.json";
/// A page stay shorter than this is a skim or a double tap (`stats.rs`).
const MIN_STAY_SECS: i64 = 5;
/// A page stay longer than this is a break (`stats.rs`).
const MAX_STAY_SECS: i64 = 600;
/// Timeout of one upload request.
const READTIME_TIMEOUT_SECS: u64 = 20;
/// First wait after a network failure that sent nothing.
const BACKOFF_START: Duration = Duration::from_secs(30);
/// Longest wait between attempts.
const BACKOFF_MAX: Duration = Duration::from_secs(600);
/// Cool-down after `-10102/-2014`.
const RATE_LIMIT_COOLDOWN_SECS: i64 = 900;
/// Let the Web channel's close write settle before the reading-time upload
/// reads the cloud position; both run at a close.
const CLOSE_SETTLE: Duration = Duration::from_secs(2);

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
    (now_ms().rem_euclid(1_000)) as u16
}

/// One reading session being timed.
struct StaySession {
    book_id: String,
    book_hash: String,
    /// Monotonic start of the page on the panel.
    shown_at: Instant,
    /// Unix second the page appeared, the `PageStay.start_time` analogue.
    started_at: i64,
}

/// What one upload attempt did.
#[derive(Debug)]
pub(super) enum UploadOutcome {
    Confirmed,
    Unconfirmed(String),
    NotSent(String),
    RateLimited,
    AuthExpired,
}

#[derive(Debug)]
struct UploadReport {
    outcome: UploadOutcome,
    position_changed: bool,
    /// A read request may rotate the refresh token; the account store must see it.
    credentials: Option<Credentials>,
    /// The refresh token the client started with, when `credentials` is newer.
    rotated_from: Option<String>,
}

struct ReadtimeEvent {
    run: u64,
    report: UploadReport,
}

/// The app's reading-time queue, session timer and upload worker.
pub struct WereadReadtime {
    enabled: bool,
    loaded: bool,
    dir: Option<PathBuf>,
    path: Option<PathBuf>,
    queue: ReadtimeQueue,
    session: Option<StaySession>,
    tx: wake::Sender<ReadtimeEvent>,
    rx: mpsc::Receiver<ReadtimeEvent>,
    run: u64,
    running: bool,
    next_attempt: Option<Instant>,
    backoff: Duration,
    /// Stays were added since the last write to the card.
    dirty: bool,
    last_save: Instant,
    message: Option<String>,
    /// Credentials a read refreshed during an upload, with the token it replaced.
    refreshed: Option<(String, Credentials)>,
    /// A close/wake/restart asked for the queue to be drained. Page turns only
    /// add stays; they never upload one page at a time.
    upload_due: bool,
}

impl Default for WereadReadtime {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for WereadReadtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WereadReadtime")
            .field("enabled", &self.enabled)
            .field("pending_seconds", &self.queue.pending_seconds())
            .field("unconfirmed_seconds", &self.queue.unconfirmed_seconds())
            .field("paused", &self.queue.paused)
            .field(
                "session",
                &self.session.as_ref().map(|s| s.book_hash.as_str()),
            )
            .field("running", &self.running)
            .finish()
    }
}

impl WereadReadtime {
    /// An empty state with a live worker channel.
    pub fn new() -> Self {
        let (tx, rx) = wake::channel();
        Self {
            enabled: false,
            loaded: false,
            dir: None,
            path: None,
            queue: ReadtimeQueue::new(),
            session: None,
            tx,
            rx,
            run: 0,
            running: false,
            next_attempt: None,
            backoff: BACKOFF_START,
            dirty: false,
            last_save: Instant::now(),
            message: None,
            refreshed: None,
            upload_due: false,
        }
    }

    /// Load the queue once from the state folder.
    pub fn ensure_loaded(&mut self, dir: &Path) {
        if self.loaded && self.dir.as_deref() == Some(dir) {
            return;
        }
        self.loaded = true;
        self.dir = Some(dir.to_path_buf());
        let path = dir.join(READTIME_FILE);
        self.path = Some(path.clone());
        let backup = sibling(&path, ".bak");
        let mut loaded = None;
        for candidate in [&path, &backup] {
            let Ok(bytes) = std::fs::read(candidate) else {
                continue;
            };
            match serde_json::from_slice::<ReadtimeQueue>(&bytes) {
                Ok(queue) if queue.version == inkread_core::weread::READTIME_QUEUE_VERSION => {
                    loaded = Some(queue);
                    break;
                }
                Ok(queue) => {
                    // A schema this build cannot read: keep the file beside the
                    // live one instead of letting the next save overwrite it.
                    isolate(candidate, &format!(".unread-v{}", queue.version));
                }
                Err(_) => {
                    isolate(candidate, ".unread");
                }
            }
        }
        let was_loaded = loaded.is_some();
        self.queue = loaded.unwrap_or_default().normalized();
        self.upload_due = self.queue.pending_seconds() > 0;
        if was_loaded {
            // Refresh the generation after a load-time repair (`inflight` to
            // unconfirmed, merges, clamps); the file is small.
            self.save();
        }
    }

    /// Follow the settings switch. Turning it on starts with the next stay; no
    /// history is read back.
    pub fn set_enabled(&mut self, enabled: bool, now: Instant) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            self.backoff = BACKOFF_START;
            self.next_attempt = Some(now);
        } else {
            self.finish_session(now, false);
            self.save();
            self.next_attempt = None;
            self.upload_due = false;
        }
    }

    /// The page on the panel appeared: start or restart its stay.
    pub fn page_shown(&mut self, hash: &str, wall: i64, now: Instant) {
        if !self.enabled {
            return;
        }
        if let Some(session) = self.session.as_mut() {
            if session.book_hash == hash {
                session.shown_at = now;
                session.started_at = wall;
                return;
            }
        }
        // Another book: the previous session ended with its last stay.
        self.finish_session(now, true);
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let Some(book_id) = book_id_for_hash(&dir, hash) else {
            return;
        };
        self.session = Some(StaySession {
            book_id,
            book_hash: hash.to_string(),
            shown_at: now,
            started_at: wall,
        });
    }

    /// The reader turned the page: close the stay and start the next one.
    pub fn page_turned(&mut self, now: Instant, wall: i64) {
        if !self.enabled {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let elapsed = now
            .saturating_duration_since(session.shown_at)
            .as_secs_f32();
        let start = session.started_at;
        let book_id = session.book_id.clone();
        session.shown_at = now;
        session.started_at = wall;
        // The same f32 validity window as `PageStay::new`: a 600.5 s stay is a
        // break there, and is not reading here either.
        if (MIN_STAY_SECS as f32..=MAX_STAY_SECS as f32).contains(&elapsed) {
            let elapsed = elapsed as i64;
            self.queue.enqueue(
                &book_id,
                &[StaySpan {
                    start_time: start,
                    duration: elapsed,
                }],
            );
            self.dirty = true;
        }
    }

    /// The reader left the book: close the stay, persist and ask for an upload.
    pub fn close(&mut self, now: Instant) {
        self.finish_session(now, true);
        self.save();
        if self.enabled {
            self.upload_due = true;
            self.next_attempt = Some(now + CLOSE_SETTLE);
        }
    }

    /// Sleep/exit: persist the queue only. The radio goes down next.
    pub fn sleep(&mut self, now: Instant) {
        self.finish_session(now, true);
        self.save();
    }

    /// A wake/network return is a good moment to drain the queue.
    pub fn wake(&mut self, now: Instant) {
        if self.enabled {
            self.upload_due = true;
            self.next_attempt = Some(now);
        }
    }

    /// A session that is no longer on screen (a failed open, a screen switch).
    pub fn clear_session(&mut self) {
        self.session = None;
    }

    /// Manual resume from the settings row.
    pub fn resume(&mut self, now: Instant) {
        self.queue.resume();
        self.backoff = BACKOFF_START;
        self.next_attempt = Some(now);
        if self.enabled {
            self.upload_due = true;
        }
        self.save();
    }

    /// Drain finished uploads and start the next batch when one is due.
    pub fn poll(
        &mut self,
        now: Instant,
        credentials: Option<Credentials>,
        policy: Policy,
        web_write_busy: bool,
    ) -> bool {
        let mut changed = false;
        // A reading session can last for hours; do not wait for its end to put
        // the stays on the card. The stats' own writer flushes on the same
        // order of magnitude.
        if self.dirty && now.duration_since(self.last_save) >= Duration::from_secs(60) {
            self.save();
        }
        while let Ok(event) = self.rx.try_recv() {
            if event.run != self.run {
                continue;
            }
            self.running = false;
            self.absorb(event.report, now);
            changed = true;
        }
        if !self.enabled || self.running || !self.upload_due || web_write_busy {
            return changed;
        }
        if self.queue.pending_seconds() == 0 {
            self.upload_due = false;
            return changed;
        }
        let due = self.next_attempt.is_none_or(|at| now >= at);
        if !due || !self.queue.ready(now_secs()) {
            return changed;
        }
        let Some(credentials) = credentials else {
            return changed;
        };
        self.start_upload(credentials, policy, now) || changed
    }

    /// Everything from the last poll, for the shelf message line.
    pub fn take_message(&mut self) -> Option<String> {
        self.message.take()
    }

    /// Credentials the E-Ink client rotated while reading the progress, with
    /// the refresh token they replace.
    pub fn take_credentials(&mut self) -> Option<(String, Credentials)> {
        self.refreshed.take()
    }

    /// The status line of the settings row, when there is something to say.
    pub fn settings_note(&self) -> Option<(String, bool)> {
        if !self.enabled {
            return Some((tr(Msg::SettingsWereadReadtimeOff).to_string(), false));
        }
        match self.queue.paused {
            Some(PauseReason::RemoteMoved) => {
                return Some((tr(Msg::SettingsWereadReadtimePaused).to_string(), true));
            }
            Some(PauseReason::Auth) => {
                return Some((tr(Msg::SettingsWereadReadtimeAuth).to_string(), true));
            }
            None => {}
        }
        let minutes = |seconds: i64| (seconds.max(1) as u64).div_ceil(60);
        if self.queue.unconfirmed_seconds() > 0 {
            return Some((
                tf!(
                    Msg::SettingsWereadReadtimeUnconfirmed,
                    minutes = minutes(self.queue.unconfirmed_seconds())
                ),
                false,
            ));
        }
        if self.queue.pending_seconds() > 0 {
            return Some((
                tf!(
                    Msg::SettingsWereadReadtimePending,
                    minutes = minutes(self.queue.pending_seconds())
                ),
                false,
            ));
        }
        Some((tr(Msg::SettingsWereadReadtimeOn).to_string(), false))
    }

    fn finish_session(&mut self, now: Instant, enqueue: bool) {
        let Some(session) = self.session.take() else {
            return;
        };
        let elapsed = now
            .saturating_duration_since(session.shown_at)
            .as_secs_f32();
        if enqueue && (MIN_STAY_SECS as f32..=MAX_STAY_SECS as f32).contains(&elapsed) {
            let elapsed = elapsed as i64;
            self.queue.enqueue(
                &session.book_id,
                &[StaySpan {
                    start_time: session.started_at,
                    duration: elapsed,
                }],
            );
            self.dirty = true;
        }
    }

    fn start_upload(&mut self, credentials: Credentials, policy: Policy, now: Instant) -> bool {
        if !self.queue.ready(now_secs()) {
            return false;
        }
        let Some(book_id) = self.queue.pending_books().first().cloned() else {
            return false;
        };
        let Some(batch) = self.queue.take_batch(&book_id, MAX_BATCH_SECS) else {
            return false;
        };
        let sent_at = now_secs();
        if !self.queue.begin_send(batch.clone(), sent_at) {
            // `take_batch` refused while an in-flight batch existed, so this
            // cannot happen; putting the spans back is still safer than
            // dropping them or clearing another batch.
            self.queue.spans.extend(batch);
            return false;
        }
        self.save();
        self.run = self.run.wrapping_add(1);
        let run = self.run;
        self.running = true;
        let tx = self.tx.clone();
        let spawned = thread::Builder::new()
            .name("weread-readtime".into())
            .spawn(move || {
                let transport = ReadestTransport::with_timeout(policy, READTIME_TIMEOUT_SECS);
                let report = upload_batch_with(transport, credentials, &book_id, &batch, sent_at);
                let _ = tx.send(ReadtimeEvent { run, report });
            });
        if let Err(error) = spawned {
            eprintln!("aristotle: weread: can't start the reading-time upload: {error}");
            self.running = false;
            self.queue.return_inflight();
            self.next_attempt = Some(now + self.backoff);
            self.save();
            return false;
        }
        true
    }

    fn absorb(&mut self, report: UploadReport, now: Instant) {
        if let (Some(credentials), Some(from)) = (report.credentials, report.rotated_from) {
            self.refreshed = Some((from, credentials));
        }
        match report.outcome {
            UploadOutcome::Confirmed => {
                self.queue.confirm();
                self.backoff = BACKOFF_START;
                self.next_attempt = Some(now);
                self.upload_due = self.queue.pending_seconds() > 0;
                if report.position_changed {
                    self.queue.pause(PauseReason::RemoteMoved);
                    self.message = Some(tr(Msg::WereadReadtimePausedRemote).to_string());
                } else {
                    self.message = Some(tr(Msg::WereadReadtimeSaved).to_string());
                }
            }
            UploadOutcome::Unconfirmed(detail) => {
                self.queue.unconfirm();
                self.next_attempt = Some(now);
                self.upload_due = self.queue.pending_seconds() > 0;
                self.message = Some(tr(Msg::WereadReadtimeUnconfirmed).to_string());
                eprintln!("aristotle: weread: unconfirmed reading-time batch: {detail}");
            }
            UploadOutcome::NotSent(detail) => {
                self.queue.return_inflight();
                self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                self.next_attempt = Some(now + self.backoff);
                eprintln!("aristotle: weread: reading-time upload not sent: {detail}");
            }
            UploadOutcome::RateLimited => {
                self.queue.return_inflight();
                self.queue.cooldown_until = now_secs() + RATE_LIMIT_COOLDOWN_SECS;
                self.next_attempt =
                    Some(now + Duration::from_secs(RATE_LIMIT_COOLDOWN_SECS as u64));
                self.message = Some(tr(Msg::WereadReadtimeRetryLater).to_string());
            }
            UploadOutcome::AuthExpired => {
                self.queue.return_inflight();
                self.queue.pause(PauseReason::Auth);
                self.upload_due = false;
                self.message = Some(tr(Msg::WereadReadtimePausedAuth).to_string());
            }
        }
        self.save();
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let mut bytes = match serde_json::to_vec_pretty(&self.queue) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("aristotle: weread: can't serialise the reading-time queue: {error}");
                return;
            }
        };
        bytes.push(b'\n');
        if let Err(error) = write_atomic(&path, &bytes) {
            eprintln!("aristotle: weread: can't save {path:?}: {error}");
            return;
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }

    #[cfg(test)]
    fn pending_seconds(&self) -> i64 {
        self.queue.pending_seconds()
    }

    #[cfg(test)]
    fn unconfirmed_seconds(&self) -> i64 {
        self.queue.unconfirmed_seconds()
    }

    #[cfg(test)]
    fn queue(&self) -> &ReadtimeQueue {
        &self.queue
    }
}

/// One batch of one book over any transport, so tests can script the answer.
fn upload_batch_with<T: HttpTransport>(
    transport: T,
    credentials: Credentials,
    book_id: &str,
    spans: &[QueuedSpan],
    now_s: i64,
) -> UploadReport {
    let original = credentials;
    let posted = Arc::new(AtomicBool::new(false));
    let probe = UploadProbe {
        inner: transport,
        posted: posted.clone(),
    };
    let mut client = WeReadClient::new(probe, original.clone());
    let (mut outcome, position_changed) = upload_with_client(&mut client, book_id, spans, now_s);
    // `batch_upload_progress` fetches `/config` and `/feature` before it posts.
    // A failure there means the request never left, so it is safe to try again;
    // only a batch whose POST was attempted is ambiguous.
    if matches!(outcome, UploadOutcome::Unconfirmed(_)) && !posted.load(Ordering::SeqCst) {
        if let UploadOutcome::Unconfirmed(detail) = outcome {
            outcome = UploadOutcome::NotSent(detail);
        }
    }
    let updated = client.credentials().cloned();
    let rotated = updated.filter(|credentials| {
        credentials.access_token != original.access_token
            || credentials.refresh_token != original.refresh_token
    });
    let rotated_from = rotated.as_ref().map(|_| original.refresh_token.clone());
    UploadReport {
        outcome,
        position_changed,
        credentials: rotated,
        rotated_from,
    }
}

/// Marks whether the upload POST was attempted; a token-fetch failure before it
/// must not be treated as an ambiguous write.
struct UploadProbe<T> {
    inner: T,
    posted: Arc<AtomicBool>,
}

impl<T: HttpTransport> HttpTransport for UploadProbe<T> {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.url.contains("/book/batchUploadProgress") {
            self.posted.store(true, Ordering::SeqCst);
        }
        self.inner.send(request)
    }
}

fn upload_with_client<T: HttpTransport>(
    client: &mut WeReadClient<T>,
    book_id: &str,
    spans: &[QueuedSpan],
    now_s: i64,
) -> (UploadOutcome, bool) {
    let before = match client.get_progress(book_id) {
        Ok(response) => response,
        Err(error) => return (preflight_outcome(&error), false),
    };
    let Some(book) = before.book else {
        return (
            UploadOutcome::NotSent(tr(Msg::WereadErrServer).to_string()),
            false,
        );
    };
    let position_before = book.position();
    let book_version = book.book_version.unwrap_or(0);
    let intervals = intervals_for(spans);
    if intervals.is_empty() {
        return (UploadOutcome::NotSent("empty batch".to_string()), false);
    }
    let mut request =
        BatchUploadRequest::new(book_id, book_version, intervals, now_s, READ_TIME_ZONE);
    request.random = random_u16() % 1000;
    request.outer_timestamp_ms = now_ms();
    request.outer_random = random_u16();
    match client.batch_upload_progress(&request) {
        Ok(()) => {
            // Another device may be reading: compare the position after the
            // upload with the one read before it. A failed second read is not
            // a reason to pause.
            let position_after = client
                .get_progress(book_id)
                .ok()
                .and_then(|response| response.book)
                .and_then(|book| book.position());
            let position_changed = match (position_before, position_after) {
                (Some(before), Some(after)) => before != after,
                _ => false,
            };
            (UploadOutcome::Confirmed, position_changed)
        }
        Err(error) => (upload_outcome(&error), false),
    }
}

fn preflight_outcome(error: &WeReadError) -> UploadOutcome {
    match error {
        WeReadError::RateLimited(_) => UploadOutcome::RateLimited,
        WeReadError::AuthExpired
        | WeReadError::AuthRejected(_)
        | WeReadError::ManualVerification(_) => UploadOutcome::AuthExpired,
        _ => UploadOutcome::NotSent(crate::weread::error_message(error)),
    }
}

fn upload_outcome(error: &WeReadError) -> UploadOutcome {
    match error {
        WeReadError::RateLimited(_) => UploadOutcome::RateLimited,
        WeReadError::AuthExpired
        | WeReadError::AuthRejected(_)
        | WeReadError::ManualVerification(_) => UploadOutcome::AuthExpired,
        WeReadError::UnconfirmedUpload => {
            UploadOutcome::Unconfirmed("the server did not confirm the upload".to_string())
        }
        WeReadError::Blocked(_) => UploadOutcome::NotSent(crate::weread::error_message(error)),
        // The request left the device; without `succ:1` it is ambiguous and
        // must never be resent (§14.6).
        other => UploadOutcome::Unconfirmed(crate::weread::error_message(other)),
    }
}

fn book_id_for_hash(dir: &Path, hash: &str) -> Option<String> {
    let store = BookMappingStore::load(dir.join("weread").join("books.json")).ok()?;
    let found = store
        .iter()
        .find(|(_, mapping)| mapping.hash == hash)
        .map(|(book_id, _)| book_id.to_string());
    found
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Move a queue generation this build cannot read out of the live slot, so a
/// later atomic save cannot overwrite it. The suffix names the reason (schema
/// version or a parse failure); a serial keeps repeated loads from clobbering
/// an earlier copy.
fn isolate(path: &Path, suffix: &str) -> Option<PathBuf> {
    let mut target = sibling(path, suffix);
    let mut serial = 1;
    while target.exists() {
        target = sibling(path, &format!("{suffix}.{serial}"));
        serial += 1;
    }
    match std::fs::rename(path, &target) {
        Ok(()) => {
            eprintln!("aristotle: weread: moved unreadable {path:?} to {target:?}");
            Some(target)
        }
        Err(error) => {
            eprintln!("aristotle: weread: can't move {path:?} aside: {error}");
            None
        }
    }
}

/// `fsync` + `.bak` rotation, the FAT discipline of `core::progress`.
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

// ---------------------------------------------------------------- app hooks

impl App {
    /// Follow the setting, loading the queue at most once.
    fn readtime_ready(&mut self, now: Instant) -> bool {
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        let enabled = self.prefs.weread_readtime;
        self.weread.readtime.set_enabled(enabled, now);
        enabled
    }

    /// The page on the panel appeared (book open, wake, back from a page).
    pub(crate) fn weread_readtime_page_shown(&mut self) {
        let now = Instant::now();
        if !self.readtime_ready(now) {
            return;
        }
        match self.stats_page().map(|(hash, _, _)| hash) {
            Some(hash) => {
                self.weread
                    .readtime
                    .page_shown(&hash, aristotle_library::library::now(), now)
            }
            None => self.weread.readtime.clear_session(),
        }
    }

    /// The reading session ended: persist the queue and try to upload it.
    pub(crate) fn weread_readtime_book_close(&mut self) {
        let now = Instant::now();
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        self.weread
            .readtime
            .set_enabled(self.prefs.weread_readtime, now);
        self.weread.readtime.close(now);
    }

    /// Manually flush any pending reading-time spans to WeRead immediately.
    pub(crate) fn weread_readtime_flush_now(&mut self) {
        let now = Instant::now();
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        self.weread
            .readtime
            .set_enabled(self.prefs.weread_readtime, now);
        if self.prefs.weread_readtime {
            self.weread.readtime.wake(now);
        }
    }

    /// Sleep: persist only; the radio is going down.
    pub(crate) fn weread_readtime_sleep(&mut self) {
        let now = Instant::now();
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        self.weread
            .readtime
            .set_enabled(self.prefs.weread_readtime, now);
        self.weread.readtime.sleep(now);
    }

    /// A wake is a chance to drain what a dead network kept.
    pub(crate) fn weread_readtime_wake(&mut self) {
        let now = Instant::now();
        if self.readtime_ready(now) {
            self.weread.readtime.wake(now);
        }
    }

    /// Drain upload results and start the next batch.
    pub(crate) fn poll_weread_readtime(&mut self) {
        let now = Instant::now();
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        self.weread
            .readtime
            .set_enabled(self.prefs.weread_readtime, now);
        let credentials = self.weread.credentials();
        let policy = crate::net::Net::policy();
        let web_write_busy = self.weread.web.write_pending();
        self.weread
            .readtime
            .poll(now, credentials, policy, web_write_busy);
        if let Some((from, credentials)) = self.weread.readtime.take_credentials() {
            // Another worker may have rotated again meanwhile; never roll the
            // account back to the token this upload replaced.
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
        if let Some(message) = self.weread.readtime.take_message() {
            self.weread.message = Some(message);
            if matches!(self.screen, Screen::WereadShelf(..)) {
                let _ = self.show_weread_shelf(aristotle_ui::tree::Refresh::Auto, 0);
            }
        }
    }

    /// The settings row: whether the switch is on and what the queue says.
    pub(crate) fn weread_readtime_note(&mut self) -> Option<(String, bool)> {
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        self.weread
            .readtime
            .set_enabled(self.prefs.weread_readtime, Instant::now());
        self.weread.readtime.settings_note()
    }

    /// The settings switch.
    pub(crate) fn set_weread_readtime(&mut self, enabled: bool) {
        self.prefs.weread_readtime = enabled;
        self.save_prefs();
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        self.weread.readtime.set_enabled(enabled, Instant::now());
    }

    /// Manual resume after a pause or a cooldown.
    pub(crate) fn resume_weread_readtime(&mut self) {
        self.weread.readtime.ensure_loaded(&self.net.paths.state);
        self.weread.readtime.resume(Instant::now());
    }
}

#[cfg(test)]
#[path = "readtime_tests.rs"]
mod tests;
