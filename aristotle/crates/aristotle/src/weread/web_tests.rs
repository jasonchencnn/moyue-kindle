// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests of the app's Web sign-in, arbitration fence and one-shot write.

use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use inkread_core::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
use inkread_core::weread::{enter_payload, we, WebLoginPoll, WebLoginSuccess};

use super::login::{LOGIN_DEADLINE, MAX_POLL_FAILURES};
use super::time::{days_from_civil, local_offset};
use super::*;

#[derive(Clone, Default)]
struct MockWeb {
    answers: Arc<Mutex<VecDeque<Result<HttpResponse, TransportError>>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl MockWeb {
    fn new(answers: Vec<Result<HttpResponse, TransportError>>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn writes(&self) -> Vec<HttpRequest> {
        self.requests()
            .into_iter()
            .filter(|request| request.url.ends_with("/web/book/read"))
            .collect()
    }
}

impl HttpTransport for MockWeb {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted answer")
    }
}

fn reader_page(psvts: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(
        200,
        format!(
            r#"<html><script>window.__INITIAL_STATE__={{"reader":{{"psvts":"{psvts}","pclts":"pc-fixture","token":"tok-fixture"}}}};</script></html>"#
        ),
    ))
}

fn receipt() -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(200, r#"{"succ":1}"#))
}

fn jar() -> CookieJar {
    let mut jar = CookieJar::new();
    jar.set("wr_vid", "vid-fixture");
    jar.set("wr_skey", "skey-fixture");
    jar
}

fn local() -> PositionStamp {
    PositionStamp::new(7, 2, 1543)
}

fn success() -> WebLoginSuccess {
    WebLoginSuccess {
        vid: "vid-fixture".to_string(),
        access_token: "skey-fixture".to_string(),
        refresh_token: "rt-fixture".to_string(),
    }
}

#[test]
fn the_login_machine_needs_one_tick_per_poll_and_confirms_once() {
    let now = Instant::now();
    let mut machine = LoginMachine::new(7, now);
    assert_eq!(machine.phase, WebLoginPhase::Fetching);
    assert!(machine.tick(now).is_none(), "no uid yet");
    machine.on_fetched("uid-fixture".to_string(), jar(), now);
    let request = machine.tick(now).expect("first poll");
    assert_eq!(request.otp, "");
    assert!(machine.tick(now).is_none(), "one request in flight");
    let confirmed = machine
        .on_poll(WebLoginPoll::Confirmed(success()), jar(), now)
        .expect("confirmed");
    assert_eq!(confirmed.vid, "vid-fixture");
    assert_eq!(machine.phase, WebLoginPhase::Confirmed);
    assert!(machine.tick(now).is_none());
}

#[test]
fn the_login_machine_stops_at_the_deadline() {
    let now = Instant::now();
    let mut machine = LoginMachine::new(1, now);
    machine.on_fetched("uid".to_string(), jar(), now);
    let late = now + LOGIN_DEADLINE + Duration::from_secs(1);
    assert!(machine.tick(late).is_none());
    assert_eq!(machine.phase, WebLoginPhase::Expired);
    // A worker that never answers must not hide the deadline either.
    let mut stuck = LoginMachine::new(9, now);
    stuck.on_fetched("uid".to_string(), jar(), now);
    let _ = stuck.tick(now);
    assert!(stuck.in_flight);
    assert!(stuck.tick(late).is_none());
    assert_eq!(stuck.phase, WebLoginPhase::Expired);
    assert_eq!(
        machine.confirm_url().as_deref(),
        Some("https://weread.qq.com/web/confirm?uid=uid")
    );
}

#[test]
fn the_login_machine_exits_on_401() {
    let now = Instant::now();
    let mut machine = LoginMachine::new(2, now);
    machine.on_fetched("uid".to_string(), jar(), now);
    let _ = machine.tick(now);
    machine.on_poll_failed(&WeReadError::WebAuthExpired, now);
    assert_eq!(machine.phase, WebLoginPhase::Expired);
}

#[test]
fn the_login_machine_waits_for_otp_and_caps_wrong_codes() {
    let now = Instant::now();
    let mut machine = LoginMachine::new(3, now);
    machine.on_fetched("uid".to_string(), jar(), now);
    let _ = machine.tick(now);
    assert!(machine.on_poll(WebLoginPoll::NeedOtp, jar(), now).is_none());
    assert_eq!(machine.phase, WebLoginPhase::NeedOtp);
    assert!(machine.tick(now).is_none(), "no poll while waiting for OTP");
    for digit in "1234".chars() {
        assert!(machine.push_digit(digit));
    }
    assert!(!machine.push_digit('5'));
    assert!(machine.submit_otp("1234", now));
    let request = machine.tick(now).expect("poll with otp");
    assert_eq!(request.otp, "1234");
    // Five wrong codes end the attempt instead of looping.
    for attempt in 0..5 {
        machine.on_poll(WebLoginPoll::OtpMismatch, jar(), now);
        if attempt < 4 {
            assert_eq!(machine.phase, WebLoginPhase::OtpMismatch);
            let _ = machine.submit_otp("0000", now);
            let _ = machine.tick(now);
        }
    }
    assert_eq!(machine.phase, WebLoginPhase::Failed);
}

#[test]
fn the_login_machine_gives_up_after_repeated_transport_failures() {
    let now = Instant::now();
    let mut machine = LoginMachine::new(4, now);
    machine.on_fetched("uid".to_string(), jar(), now);
    for _ in 0..MAX_POLL_FAILURES {
        let _ = machine.tick(now);
        machine.on_poll_failed(
            &WeReadError::Transport("offline".to_string()),
            now + Duration::from_secs(60),
        );
    }
    assert_eq!(machine.phase, WebLoginPhase::Failed);
}

fn attempt(fence: &mut WriteFence, transport: MockWeb) -> Option<WriteOutcome> {
    if !fence.begin_write() {
        return None;
    }
    let mut session = WriteSession::default();
    Some(write_in(&mut session, transport))
}

fn write_in(session: &mut WriteSession, transport: MockWeb) -> WriteOutcome {
    session.write(transport, jar(), "100001", local(), 42, 30, "cloud summary")
}

/// One write in a fresh reading session, as a book open would do.
fn write_one(transport: MockWeb) -> WriteOutcome {
    write_in(&mut WriteSession::default(), transport)
}

#[test]
fn only_local_newer_reaches_the_transport_and_only_once() {
    let before = SyncBaseline::new(local(), local());
    let local_newer = arbitrate(PositionStamp::new(7, 2, 1700), local(), Some(before));
    let mut fence = WriteFence::new(local_newer, true);
    let transport = MockWeb::new(vec![reader_page("ps-fixture"), receipt(), receipt()]);
    let outcome = attempt(&mut fence, transport.clone()).expect("the one write");
    assert!(matches!(outcome, WriteOutcome::Confirmed));
    // The session's one write is the enter report plus the period.
    assert_eq!(transport.writes().len(), 2);
    // A second close event in the same verdict cannot write again.
    assert!(attempt(&mut fence, transport.clone()).is_none());
    assert_eq!(transport.writes().len(), 2);
}

// WR-005 L1 Sugg-1: `enter` opens a reading session once, even when the first
// write got no receipt; a new session sends it again.
#[test]
fn enter_opens_a_reading_session_once_and_a_new_session_resends_it() {
    let transport = MockWeb::new(vec![
        reader_page("ps-fixture"),
        Ok(HttpResponse::new(200, r#"{"succ":0}"#)),
        reader_page("ps-fixture"),
        receipt(),
        reader_page("ps-fixture"),
        receipt(),
        receipt(),
    ]);
    let mut session = WriteSession::default();
    let outcome = write_in(&mut session, transport.clone());
    assert!(
        matches!(outcome, WriteOutcome::Unconfirmed(_)),
        "{outcome:?}"
    );
    assert_eq!(transport.writes().len(), 1, "the enter report was sent");
    let outcome = write_in(&mut session, transport.clone());
    assert!(matches!(outcome, WriteOutcome::Confirmed), "{outcome:?}");
    assert_eq!(
        transport.writes().len(),
        2,
        "the second write sends no enter"
    );
    let second: serde_json::Value =
        serde_json::from_slice(&transport.writes()[1].body).expect("json");
    assert!(second.get("rt").is_some(), "the second write is a period");

    let mut fresh = WriteSession::default();
    let outcome = write_in(&mut fresh, transport.clone());
    assert!(matches!(outcome, WriteOutcome::Confirmed), "{outcome:?}");
    assert_eq!(transport.writes().len(), 4, "a new session resends enter");
    let reopened: serde_json::Value =
        serde_json::from_slice(&transport.writes()[2].body).expect("json");
    assert!(
        reopened.get("rt").is_none(),
        "the new session opens with enter"
    );
}

#[test]
fn remote_newer_aligned_and_conflict_never_send_http() {
    let baseline = SyncBaseline::new(local(), local());
    let cases = [
        (
            arbitrate(local(), PositionStamp::new(7, 2, 1800), Some(baseline)),
            Verdict::RemoteNewer,
        ),
        (
            arbitrate(local(), local(), Some(baseline)),
            Verdict::Aligned,
        ),
        (
            arbitrate(
                PositionStamp::new(7, 2, 1800),
                PositionStamp::new(8, 3, 10),
                Some(baseline),
            ),
            Verdict::Conflict,
        ),
        (
            arbitrate(local(), PositionStamp::new(8, 3, 10), None),
            Verdict::Conflict,
        ),
    ];
    for (verdict, expected) in cases {
        assert_eq!(verdict, expected);
        let mut fence = WriteFence::new(verdict, true);
        let transport = MockWeb::new(vec![]);
        assert!(
            attempt(&mut fence, transport.clone()).is_none(),
            "{verdict:?}"
        );
        assert!(transport.requests().is_empty(), "{verdict:?}");
    }
}

#[test]
fn a_confirmed_write_echoes_the_reader_page_state() {
    let transport = MockWeb::new(vec![reader_page("ps-fixture"), receipt(), receipt()]);
    let outcome = write_one(transport.clone());
    assert!(matches!(outcome, WriteOutcome::Confirmed));
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].url.contains("/web/reader/"));
    // §14.4: the session opens with `enter`, which has no time fields.
    let enter: serde_json::Value = serde_json::from_slice(&requests[1].body).expect("json");
    for key in ["rt", "ts", "rn", "sg"] {
        assert!(enter.get(key).is_none(), "enter must not carry {key}");
    }
    assert_eq!(enter["b"], we("100001"));
    assert_eq!(enter["pr"], 42);
    let mut enter_input = ReadPayloadInput::new("100001", 7);
    enter_input.chapter_idx = 2;
    enter_input.chapter_offset = 1543;
    enter_input.progress = 42;
    enter_input.summary = "cloud summary".to_string();
    enter_input.psvts = "ps-fixture".to_string();
    enter_input.pclts = Some("pc-fixture".to_string());
    enter_input.now_s = enter["ct"].as_i64().expect("ct");
    enter_input.token = "tok-fixture".to_string();
    assert_eq!(
        enter["s"],
        enter_payload(&enter_input)["s"].js_string(),
        "enter is signed"
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[2].body).expect("json");
    assert_eq!(body["b"], we("100001"));
    assert_eq!(body["c"], we("7"));
    assert_eq!(body["ci"], 2);
    assert_eq!(body["co"], 1543);
    assert_eq!(body["pr"], 42);
    assert_eq!(body["ps"], "ps-fixture");
    assert_eq!(body["pc"], "pc-fixture");
    assert_eq!(body["rt"], 30);
    let mut input = ReadPayloadInput::new("100001", 7);
    input.chapter_idx = 2;
    input.chapter_offset = 1543;
    input.progress = 42;
    input.summary = "cloud summary".to_string();
    input.psvts = "ps-fixture".to_string();
    input.pclts = Some("pc-fixture".to_string());
    input.now_s = body["ct"].as_i64().expect("ct");
    input.elapsed_s = 30;
    input.ts_ms = body["ts"].as_i64().expect("ts");
    input.rn = u16::try_from(body["rn"].as_i64().expect("rn")).expect("rn");
    input.token = "tok-fixture".to_string();
    let expected = read_payload(&input);
    assert_eq!(body["sg"], expected["sg"].js_string());
    assert_eq!(body["s"], expected["s"].js_string());
}

#[test]
fn succ_zero_and_a_timeout_are_unconfirmed_and_never_retried() {
    let failures: [fn() -> Result<HttpResponse, TransportError>; 3] = [
        || Ok(HttpResponse::new(200, r#"{"succ":0}"#)),
        || Err(TransportError::Timeout),
        || Err(TransportError::Network("offline".to_string())),
    ];
    for failure in failures {
        // The enter report itself can go unanswered: the period is not sent.
        let transport = MockWeb::new(vec![reader_page("ps-fixture"), failure()]);
        let outcome = write_one(transport.clone());
        assert!(
            matches!(outcome, WriteOutcome::Unconfirmed(_)),
            "{outcome:?}"
        );
        assert_eq!(transport.writes().len(), 1, "never resent");
        // The period's missing receipt is unconfirmed too, after the enter.
        let transport = MockWeb::new(vec![reader_page("ps-fixture"), receipt(), failure()]);
        let outcome = write_one(transport.clone());
        assert!(
            matches!(outcome, WriteOutcome::Unconfirmed(_)),
            "{outcome:?}"
        );
        assert_eq!(transport.writes().len(), 2, "enter then period, no retry");
    }
}

#[test]
fn a_401_marks_the_web_session_dead() {
    let transport = MockWeb::new(vec![
        reader_page("ps-fixture"),
        Ok(HttpResponse::new(401, "{}")),
    ]);
    let outcome = write_one(transport.clone());
    assert!(matches!(outcome, WriteOutcome::AuthExpired));
    assert_eq!(transport.writes().len(), 1);
}

#[test]
fn a_reader_page_without_psvts_never_sends_a_write() {
    let transport = MockWeb::new(vec![reader_page("")]);
    let outcome = write_one(transport.clone());
    assert!(matches!(outcome, WriteOutcome::NotSent(_)), "{outcome:?}");
    assert!(transport.writes().is_empty());
}

#[test]
fn the_web_store_is_private_redacted_and_survives_a_reload() {
    let dir = std::env::temp_dir().join(format!(
        "aristotle-wr004b-{}-{}",
        std::process::id(),
        now_secs()
    ));
    std::fs::create_dir_all(&dir).expect("state dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("private dir");
    let mut web = WereadWeb::new();
    web.ensure_loaded(&dir);
    web.apply_session(&success(), jar().persistent());
    assert!(web.signed_in());
    let path = dir.join(WEB_STORE_FILE);
    let mode = std::fs::metadata(&path)
        .expect("store")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "credentials must be owner-only");
    let debug = format!("{web:?}");
    for secret in ["vid-fixture", "skey-fixture", "rt-fixture"] {
        assert!(!debug.contains(secret), "debug leaked {secret}: {debug}");
    }

    let mut reloaded = WereadWeb::new();
    reloaded.ensure_loaded(&dir);
    assert!(reloaded.signed_in());
    reloaded.invalidate("expired".to_string());
    assert!(!reloaded.signed_in());
    assert_eq!(reloaded.message(), Some("expired"));
    let mut after = WereadWeb::new();
    after.ensure_loaded(&dir);
    assert!(!after.signed_in(), "the invalid flag is persisted");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_offset_is_a_fixed_width_utc_offset() {
    let text = local_offset(now_secs());
    let bytes = text.as_bytes();
    assert_eq!(bytes.len(), 6, "{text}");
    assert!(matches!(bytes[0], b'+' | b'-'));
    assert!(bytes[1].is_ascii_digit());
    assert!(bytes[2].is_ascii_digit());
    assert_eq!(bytes[3], b':');
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 1, 1), 10_957);
}

// AC-03: skills page -> getLoginUid -> polls through the OTP states ->
// confirmed -> cookies persisted 0600.
#[test]
fn the_web_login_flow_reaches_a_private_cookie_jar() {
    let page = Ok(HttpResponse::new(200, "<html>skills</html>")
        .with_header("Set-Cookie", "ptcz=stable; Path=/"));
    let answers = vec![
        page,
        Ok(HttpResponse::new(200, r#"{"uid":"uid-fixture"}"#)),
        Ok(HttpResponse::new(200, r#"{"logicCode":"NEED_OTP"}"#)),
        Ok(HttpResponse::new(200, r#"{"logicCode":"OTP_NOT_MATCH"}"#)),
        Ok(HttpResponse::new(
            200,
            r#"{"succeed":true,"webLoginVid":"vid-fixture","accessToken":"skey-fixture","refreshToken":"rt-fixture"}"#,
        )),
    ];
    let transport = MockWeb::new(answers);
    let mut client = WeReadWebClient::new(transport.clone());
    client.login_page().expect("skills page");
    assert_eq!(client.jar().get("ptcz"), Some("stable"));
    let uid = client.login_uid().expect("uid");
    assert_eq!(uid, "uid-fixture");

    let now = Instant::now();
    let mut machine = LoginMachine::new(1, now);
    machine.on_fetched(uid.clone(), client.jar().clone(), now);
    let request = machine.tick(now).expect("first poll");
    let poll = client.login_poll(&request.uid, &request.otp).expect("poll");
    assert!(machine.on_poll(poll, client.jar().clone(), now).is_none());
    assert_eq!(machine.phase, WebLoginPhase::NeedOtp);
    assert!(machine.tick(now).is_none(), "OTP waits for the reader");
    for digit in "1234".chars() {
        assert!(machine.push_digit(digit));
    }
    let debug = format!("{machine:?}");
    assert!(!debug.contains("uid-fixture"), "{debug}");
    assert!(!debug.contains("1234"), "{debug}");
    assert!(debug.contains("otp_len"));
    assert!(machine.submit_otp("1234", now));
    let request = machine.tick(now).expect("poll with otp");
    assert_eq!(request.otp, "1234");
    let poll = client.login_poll(&request.uid, &request.otp).expect("poll");
    assert!(machine.on_poll(poll, client.jar().clone(), now).is_none());
    assert_eq!(machine.phase, WebLoginPhase::OtpMismatch);
    assert!(machine.submit_otp("0000", now));
    let request = machine.tick(now).expect("poll after new otp");
    let poll = client.login_poll(&request.uid, &request.otp).expect("poll");
    let success = machine
        .on_poll(poll, client.jar().clone(), now)
        .expect("confirmed");
    assert_eq!(machine.phase, WebLoginPhase::Confirmed);
    assert_eq!(success.vid, "vid-fixture");

    let dir = std::env::temp_dir().join(format!(
        "aristotle-wr004b-login-{}-{}",
        std::process::id(),
        now_secs()
    ));
    std::fs::create_dir_all(&dir).expect("state dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("private");
    let mut web = WereadWeb::new();
    web.ensure_loaded(&dir);
    web.apply_session(&success, client.into_jar().persistent());
    assert!(web.signed_in());
    let store = std::fs::read(dir.join(WEB_STORE_FILE)).expect("store");
    let text = String::from_utf8_lossy(&store);
    assert!(text.contains("wr_vid"));
    assert!(text.contains("wr_skey"));
    assert!(!format!("{web:?}").contains("skey-fixture"));
    let mode = std::fs::metadata(dir.join(WEB_STORE_FILE))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let _ = std::fs::remove_dir_all(&dir);
}
