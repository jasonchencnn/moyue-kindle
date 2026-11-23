// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests of the reading-time timer, queue persistence and upload receipts.

use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aristotle_i18n::with_language;
use aristotle_net::Policy;
use inkread_core::sync::transport::{HttpRequest, HttpResponse, TransportError};
use inkread_core::weread::{
    BookMapping, BookMappingStore, Credentials, QueuedSpan, MAX_BATCH_SECS,
};

use super::*;

const HASH: &str = "hash-100001";
const BOOK: &str = "100001";

#[derive(Clone, Default)]
struct MockUpload {
    answers: Arc<Mutex<VecDeque<Result<HttpResponse, TransportError>>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl MockUpload {
    fn new(answers: Vec<Result<HttpResponse, TransportError>>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn writes(&self) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.url.contains("/book/batchUploadProgress"))
            .count()
    }
}

impl HttpTransport for MockUpload {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted answer")
    }
}

fn progress(offset: i64) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(
        200,
        format!(
            r#"{{"bookId":"{BOOK}","book":{{"bookVersion":42,"chapterUid":7,"chapterIdx":2,"chapterOffset":{offset},"progress":42,"readingTime":600}}}}"#
        ),
    ))
}

fn config() -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(
        200,
        r#"{"token":"config-token-fixture"}"#,
    ))
}

fn feature() -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(
        200,
        r#"{"feature":{"guest_token":"guest-token-fixture"}}"#,
    ))
}

fn receipt() -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(200, r#"{"succ":1}"#))
}

fn server_error() -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(500, "{}"))
}

fn rate_limited() -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(
        200,
        r#"{"errcode":-10102,"errmsg":"rate"}"#,
    ))
}

fn timeout() -> Result<HttpResponse, TransportError> {
    Err(TransportError::Timeout)
}

fn offline() -> Result<HttpResponse, TransportError> {
    Err(TransportError::Network("offline".to_string()))
}

fn credentials() -> Credentials {
    Credentials::new(
        "1234567890",
        "access-token-fixture",
        "refresh-token-fixture",
        "eink-test-device-0001",
    )
}

fn state_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aristotle-wr005-{tag}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn write_mapping(dir: &Path) {
    let path = dir.join("weread").join("books.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut store = BookMappingStore::load(&path).unwrap();
    store.insert(
        BOOK,
        BookMapping {
            hash: HASH.to_string(),
            title: "Synthetic Book".to_string(),
            format: "epub".to_string(),
            synckey: None,
        },
    );
    store.save().unwrap();
}

fn enabled(dir: &Path) -> WereadReadtime {
    let mut readtime = WereadReadtime::new();
    readtime.ensure_loaded(dir);
    readtime.set_enabled(true, Instant::now());
    readtime
}

/// One 60-second stay already queued, as if the reader had turned a page.
fn one_stay(dir: &Path) -> WereadReadtime {
    let mut readtime = enabled(dir);
    let t0 = Instant::now();
    readtime.page_shown(HASH, 1_000, t0);
    readtime.page_turned(t0 + Duration::from_secs(60), 1_060);
    assert_eq!(readtime.pending_seconds(), 60);
    assert!(!readtime.upload_due, "a page turn never uploads on its own");
    readtime
}

fn batch(readtime: &mut WereadReadtime) -> Vec<QueuedSpan> {
    readtime
        .queue
        .take_batch(BOOK, MAX_BATCH_SECS)
        .expect("a batch")
}

fn send(
    batch: &[QueuedSpan],
    answers: Vec<Result<HttpResponse, TransportError>>,
) -> (UploadReport, MockUpload) {
    let mock = MockUpload::new(answers);
    let report = upload_batch_with(mock.clone(), credentials(), BOOK, batch, now_secs());
    (report, mock)
}

#[test]
fn the_switch_is_off_and_history_is_never_read_back() {
    let dir = state_dir("off");
    write_mapping(&dir);
    // The local statistics already hold a stay; the queue must not import it.
    std::fs::write(
        dir.join("stats-2026-11.jsonl"),
        r#"{"book_hash":"hash-100001","page":1,"start_time":1000,"duration":300,"total_pages":10}"#,
    )
    .unwrap();
    let mut readtime = WereadReadtime::new();
    readtime.ensure_loaded(&dir);
    assert!(!readtime.enabled);
    let t0 = Instant::now();
    readtime.page_shown(HASH, 2_000, t0);
    readtime.page_turned(t0 + Duration::from_secs(60), 2_060);
    assert_eq!(readtime.pending_seconds(), 0, "off means nothing is kept");

    readtime.set_enabled(true, t0 + Duration::from_secs(120));
    assert_eq!(readtime.pending_seconds(), 0, "no backfill");
    readtime.page_shown(HASH, 3_000, t0 + Duration::from_secs(120));
    readtime.page_turned(t0 + Duration::from_secs(180), 3_060);
    assert_eq!(readtime.pending_seconds(), 60);

    // Turning it off drops the partial stay and stops new ones.
    readtime.set_enabled(false, t0 + Duration::from_secs(200));
    readtime.page_shown(HASH, 4_000, t0 + Duration::from_secs(210));
    readtime.page_turned(t0 + Duration::from_secs(270), 4_060);
    assert_eq!(readtime.pending_seconds(), 60);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_valid_stays_are_counted_and_the_same_stay_is_not_doubled() {
    let dir = state_dir("window");
    write_mapping(&dir);
    let mut readtime = enabled(&dir);
    let t0 = Instant::now();
    readtime.page_shown(HASH, 1_000, t0);
    // A skim: under five seconds.
    readtime.page_turned(t0 + Duration::from_secs(2), 1_002);
    assert_eq!(readtime.pending_seconds(), 0);
    // A normal stay.
    readtime.page_turned(t0 + Duration::from_secs(62), 1_062);
    assert_eq!(readtime.pending_seconds(), 60);
    // A break: the page was open for over ten minutes (600.5 s is a break in
    // `PageStay::new`, which compares the f32 seconds).
    readtime.page_turned(t0 + Duration::from_millis(662_500), 1_663);
    assert_eq!(readtime.pending_seconds(), 60);
    // A book that is not on the WeRead shelf is ignored.
    readtime.page_shown("local-book-hash", 2_000, t0);
    readtime.page_turned(t0 + Duration::from_secs(60), 2_060);
    assert_eq!(readtime.pending_seconds(), 60);
    // Adjacent stays of the same book merge into one span.
    readtime.queue.enqueue(
        BOOK,
        &[StaySpan {
            start_time: 1_062,
            duration: 60,
        }],
    );
    assert_eq!(readtime.queue().spans.len(), 1);
    assert_eq!(readtime.pending_seconds(), 120);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_the_book_persists_the_queue_and_recovers_its_backup() {
    let dir = state_dir("persist");
    write_mapping(&dir);
    let mut readtime = enabled(&dir);
    let t0 = Instant::now();
    readtime.page_shown(HASH, 1_000, t0);
    readtime.close(t0 + Duration::from_secs(60));
    assert_eq!(readtime.pending_seconds(), 60);
    let path = dir.join(READTIME_FILE);
    assert!(path.exists());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // A second save rotates the first generation to `.bak`.
    readtime.close(t0 + Duration::from_secs(61));
    assert!(dir.join("weread-readtime.json.bak").exists());
    assert!(!dir.join("weread-readtime.json.part").exists());
    // A torn main file still loads the previous generation.
    std::fs::write(&path, b"not json").unwrap();
    let mut loaded = WereadReadtime::new();
    loaded.ensure_loaded(&dir);
    assert_eq!(loaded.pending_seconds(), 60);
    assert!(
        dir.join("weread-readtime.json.unread").exists(),
        "the torn file is kept instead of overwritten"
    );
    assert!(!loaded.enabled, "the switch is a preference, not the queue");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_queue_version_this_build_cannot_read_is_isolated_not_cleared() {
    let dir = state_dir("queue-version");
    write_mapping(&dir);
    let path = dir.join(READTIME_FILE);
    let raw = r#"{"version":9,"spans":[{"bookId":"100001","startTime":1000,"duration":60}],"unconfirmed":[],"inflight":null,"paused":null,"cooldownUntil":0}"#;
    std::fs::write(&path, raw).unwrap();
    let mut readtime = WereadReadtime::new();
    readtime.ensure_loaded(&dir);
    assert_eq!(readtime.pending_seconds(), 0);
    assert!(!path.exists(), "the unreadable queue is moved aside");
    let isolated = dir.join("weread-readtime.json.unread-v9");
    assert_eq!(
        std::fs::read_to_string(&isolated).unwrap(),
        raw,
        "the isolated file keeps every byte"
    );
    // The next save starts a fresh generation and leaves the copy alone.
    readtime.save();
    assert_eq!(std::fs::read_to_string(&isolated).unwrap(), raw);
    assert!(path.exists(), "a new queue generation is written");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_receipt_is_unconfirmed_and_never_taken_again() {
    let dir = state_dir("unconfirmed");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, mock) = send(&spans, vec![progress(100), config(), feature(), timeout()]);
    assert!(matches!(report.outcome, UploadOutcome::Unconfirmed(_)));
    assert_eq!(mock.writes(), 1);
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.pending_seconds(), 0);
    assert_eq!(readtime.unconfirmed_seconds(), 60);
    assert!(
        !readtime.start_upload(credentials(), Policy::open(), Instant::now()),
        "an unconfirmed batch is never resent"
    );
    assert_eq!(mock.writes(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_receipt_clears_the_batch() {
    let dir = state_dir("receipt");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, _) = send(
        &spans,
        vec![progress(100), config(), feature(), receipt(), progress(100)],
    );
    assert!(matches!(report.outcome, UploadOutcome::Confirmed));
    assert!(!report.position_changed);
    assert!(report.credentials.is_none(), "no rotation, no write back");
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.pending_seconds(), 0);
    assert_eq!(readtime.unconfirmed_seconds(), 0);
    assert!(readtime.queue.paused.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_moving_cloud_position_pauses_until_the_reader_resumes() {
    let dir = state_dir("paused");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, _) = send(
        &spans,
        vec![progress(100), config(), feature(), receipt(), progress(200)],
    );
    assert!(matches!(report.outcome, UploadOutcome::Confirmed));
    assert!(
        report.position_changed,
        "the other device moved the position"
    );
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.queue.paused, Some(PauseReason::RemoteMoved));
    assert!(!readtime.queue.ready(now_secs()));
    let note = with_language("en", || readtime.settings_note()).expect("the paused row is shown");
    assert!(note.0.contains("Paused"), "{}", note.0);
    assert!(note.1, "the paused row offers resume");

    // The next batch stays pending until the manual resume.
    readtime.queue.enqueue(
        BOOK,
        &[StaySpan {
            start_time: 2_000,
            duration: 60,
        }],
    );
    assert!(!readtime.start_upload(credentials(), Policy::open(), Instant::now()));
    readtime.resume(Instant::now());
    assert!(readtime.queue.paused.is_none());
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, _) = send(
        &spans,
        vec![progress(200), config(), feature(), receipt(), progress(200)],
    );
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.pending_seconds(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rate_limit_cools_down_and_the_batch_is_retried_after_it() {
    let dir = state_dir("rate");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, _) = send(
        &spans,
        vec![progress(100), config(), feature(), rate_limited()],
    );
    assert!(matches!(report.outcome, UploadOutcome::RateLimited));
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.pending_seconds(), 60, "the seconds are kept");
    assert_eq!(readtime.unconfirmed_seconds(), 0);
    assert!(readtime.queue.cooldown_until > now_secs());
    assert!(!readtime.queue.ready(now_secs()));
    // Simulate the cool-down passing: the same seconds are retried.
    readtime.queue.cooldown_until = now_secs() - 1;
    readtime.next_attempt = Some(Instant::now());
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, mock) = send(
        &spans,
        vec![progress(100), config(), feature(), receipt(), progress(100)],
    );
    assert!(matches!(report.outcome, UploadOutcome::Confirmed));
    assert_eq!(mock.writes(), 1);
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.pending_seconds(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_preflight_network_failure_keeps_the_batch_pending() {
    let dir = state_dir("offline");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, mock) = send(&spans, vec![offline()]);
    assert!(matches!(report.outcome, UploadOutcome::NotSent(_)));
    assert_eq!(mock.writes(), 0, "nothing was sent");
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.pending_seconds(), 60, "kept for the next network");
    assert_eq!(readtime.unconfirmed_seconds(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pending_and_unconfirmed_seconds_are_shown_in_settings() {
    let dir = state_dir("note");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let note = with_language("en", || readtime.settings_note()).expect("pending note");
    assert_eq!(note.0, "1 min waiting to upload");
    assert!(!note.1);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, _) = send(&spans, vec![progress(100), config(), feature(), timeout()]);
    readtime.absorb(report, Instant::now());
    let note = with_language("en", || readtime.settings_note()).expect("unconfirmed note");
    assert_eq!(note.0, "1 min sent without a receipt");
    // The switch is off: the row says that nothing is uploaded.
    readtime.set_enabled(false, Instant::now());
    let note = with_language("en", || readtime.settings_note()).expect("off note");
    assert_eq!(note.0, "Off — nothing is uploaded");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_token_rotated_by_a_read_is_handed_back_to_the_account() {
    let dir = state_dir("token");
    write_mapping(&dir);
    let mut readtime = enabled(&dir);
    let rotated = Credentials::new(
        "1234567890",
        "new-access-token",
        "new-refresh-token",
        "eink-test-device-0001",
    );
    readtime.absorb(
        UploadReport {
            outcome: UploadOutcome::NotSent("offline".to_string()),
            position_changed: false,
            credentials: Some(rotated.clone()),
            rotated_from: Some("refresh-token-fixture".to_string()),
        },
        Instant::now(),
    );
    let (from, handed) = readtime
        .take_credentials()
        .expect("the rotated credentials");
    assert_eq!(from, "refresh-token-fixture");
    assert_eq!(handed.access_token, "new-access-token");
    assert_eq!(handed.refresh_token, "new-refresh-token");
    assert!(readtime.take_credentials().is_none(), "taken once");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn page_turns_do_not_upload_until_the_book_closes() {
    let dir = state_dir("trigger");
    write_mapping(&dir);
    let mut readtime = enabled(&dir);
    let t0 = Instant::now();
    readtime.page_shown(HASH, 1_000, t0);
    readtime.page_turned(t0 + Duration::from_secs(60), 1_060);
    assert!(!readtime.upload_due);
    readtime.close(t0 + Duration::from_secs(120));
    assert!(
        readtime.upload_due,
        "closing asks for the queue to be drained"
    );
    let when = readtime.next_attempt.expect("a settled retry time");
    assert!(when >= t0 + Duration::from_secs(120) + Duration::from_secs(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_startup_with_pending_spans_resumes_the_upload() {
    let dir = state_dir("startup");
    write_mapping(&dir);
    let mut readtime = enabled(&dir);
    let t0 = Instant::now();
    readtime.page_shown(HASH, 1_000, t0);
    readtime.close(t0 + Duration::from_secs(60));
    assert_eq!(readtime.pending_seconds(), 60);
    let mut restarted = WereadReadtime::new();
    restarted.ensure_loaded(&dir);
    assert!(restarted.upload_due, "a restart is a wake for the queue");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_token_fetch_failure_before_the_post_is_retried_later() {
    let dir = state_dir("pretoken");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    // `/config` fails: the upload POST is never attempted.
    let (report, mock) = send(&spans, vec![progress(100), server_error()]);
    assert!(
        matches!(report.outcome, UploadOutcome::NotSent(_)),
        "{:?}",
        report.outcome
    );
    assert_eq!(mock.writes(), 0, "nothing was posted");
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.pending_seconds(), 60, "kept for the next network");
    assert_eq!(readtime.unconfirmed_seconds(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_posted_batch_without_a_receipt_is_unconfirmed() {
    let dir = state_dir("posted");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans.clone(), now_secs()));
    let (report, mock) = send(
        &spans,
        vec![progress(100), config(), feature(), server_error()],
    );
    assert!(
        matches!(report.outcome, UploadOutcome::Unconfirmed(_)),
        "{:?}",
        report.outcome
    );
    assert_eq!(mock.writes(), 1, "the POST did leave the device");
    readtime.running = false;
    readtime.absorb(report, Instant::now());
    assert_eq!(readtime.unconfirmed_seconds(), 60);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_interrupted_process_never_resends_the_in_flight_batch() {
    let dir = state_dir("inflight");
    write_mapping(&dir);
    let mut readtime = one_stay(&dir);
    let spans = batch(&mut readtime);
    assert!(readtime.queue.begin_send(spans, now_secs()));
    // The process dies here: the file on disk says "in flight".
    readtime.save();
    let mut restarted = WereadReadtime::new();
    restarted.ensure_loaded(&dir);
    assert_eq!(restarted.pending_seconds(), 0);
    assert_eq!(restarted.unconfirmed_seconds(), 60);
    assert!(
        !restarted.start_upload(credentials(), Policy::open(), Instant::now()),
        "no blind resend after a restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
