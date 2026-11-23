//! Unit tests of the Web channel (§14.1/§14.4/§14.7).
//!
//! Split from `web.rs` to keep every file under the repository's 1200-line
//! limit; the module is compiled only for tests.

use std::sync::{Arc, Mutex};

use super::*;

/// A scripted transport: answers in order and records every request.
#[derive(Clone, Default)]
struct Scripted {
    answers: Arc<Mutex<Vec<Result<HttpResponse, TransportError>>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl Scripted {
    fn new(answers: Vec<Result<HttpResponse, TransportError>>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for Scripted {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request);
        self.answers.lock().unwrap().remove(0)
    }
}

fn response(status: u16, body: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::new(status, body))
}

// AC-02: fixed vectors. The expected values were produced independently by
// the reference implementation (weread.koplugin/weread/lib/protocol.lua,
// §14.4's source) and by a from-spec Python implementation; both agree.
#[test]
fn we_matches_the_contract_golden_vectors() {
    // §14.4 `e(v)`, lines 453: h[0:3]+flag+"2"+h[-2:], nine-digit chunks.
    assert_eq!(we("100001"), "e2a324305186a1e2a6a1b3b");
    assert_eq!(we("123456789012"), "341326f0775bcd15g01c007");
    assert_eq!(
        we("MP_WXS_100001"),
        "5f7420d1a4d505f5758535f313030303031206"
    );
    assert_eq!(we("book-αβ"), "a1742f412626f6f6b2dceb1ceb25d1");
    assert_eq!(we("0"), "cfc32da010cfcd208495488");
}

#[test]
fn sign_matches_the_contract_golden_vector() {
    // §14.4 `sign(q)`, line 455: a=b=0x15051505, two bytes from the end.
    let query = "appId=wb000000000000h123456&b=e2a324305186a1e2a6a1b3b&c=8f132430178f14e45fce0f7&ci=2&co=1543&ct=1795006900&pc=b65324607ab2f692g010282&pr=42&ps=psvts-fixture&rn=321&rt=30&sg=5ee6d3c0a2338c35db3a29c3135dc1fa388f4085619e3915640fb64669346863&sm=%E5%B1%B1%E6%9C%89%E6%9C%A8%E5%85%AE%E6%9C%A8%E6%9C%89%E6%9E%9D%EF%BC%8C%E5%BF%83%E6%82%A6%E5%90%9B%E5%85%AE%E5%90%9B%E4%B8%8D%E7%9F%A5%EF%BC%8Cextr&ts=1795006900321";
    assert_eq!(sign(query), "7999bdc4");
    assert_eq!(sign(""), "2a0a2a0a");
}

#[test]
fn web_app_id_matches_the_contract_golden_vector() {
    // §14.4 `web_app_id(UA)`, line 454.
    assert_eq!(web_app_id(WEB_USER_AGENT), "wb115321887466h529830856");
}

#[test]
fn sorted_query_sorts_keys_and_percent_encodes_values() {
    // §14.4 `sorted_query`, line 456: keys sorted, `s` excluded.
    let mut payload = WebPayload::new();
    payload.insert("s".to_string(), QueryValue::Text("ignored".to_string()));
    payload.insert("b".to_string(), QueryValue::Text("a b".to_string()));
    payload.insert("ci".to_string(), QueryValue::Number(2));
    payload.insert("flag".to_string(), QueryValue::Bool(false));
    payload.insert("none".to_string(), QueryValue::Null);
    assert_eq!(sorted_query(&payload), "b=a%20b&ci=2&flag=false&none=null");
}

fn fixed_input() -> ReadPayloadInput {
    ReadPayloadInput {
        book_id: "100001".to_string(),
        chapter_uid: 7,
        chapter_idx: 2,
        chapter_offset: 1543,
        progress: 42,
        summary: "山有木兮木有枝，心悦君兮君不知，extra tail beyond twenty".to_string(),
        psvts: "psvts-fixture".to_string(),
        pclts: None,
        app_id: "wb000000000000h123456".to_string(),
        now_s: 1_795_006_900,
        elapsed_s: 30,
        ts_ms: 1_795_006_900_321,
        rn: 321,
        token: DEFAULT_READER_TOKEN.to_string(),
    }
}

#[test]
fn enter_payload_has_exactly_the_contract_fields() {
    // §14.4 lines 453/457: enter = position fields, no rt/ts/rn/sg.
    let payload = enter_payload(&fixed_input());
    let expected = [
        ("appId", "wb000000000000h123456"),
        ("b", "e2a324305186a1e2a6a1b3b"),
        ("c", "8f132430178f14e45fce0f7"),
        ("ci", "2"),
        ("co", "1543"),
        ("ct", "1795006900"),
        ("pc", "b65324607ab2f692g010282"),
        ("pr", "42"),
        ("ps", "psvts-fixture"),
        ("sm", "山有木兮木有枝，心悦君兮君不知，extr"),
    ];
    assert_eq!(payload.len(), expected.len() + 1);
    for (key, value) in expected {
        assert_eq!(
            payload.get(key).map(QueryValue::js_string).as_deref(),
            Some(value),
            "{key}"
        );
    }
    assert_eq!(
        payload.get("s").map(QueryValue::js_string).as_deref(),
        Some("571e86d8")
    );
    for absent in ["rt", "ts", "rn", "sg"] {
        assert!(!payload.contains_key(absent), "{absent} must be absent");
    }
}

#[test]
fn period_payload_has_every_contract_field() {
    // §14.4 lines 453/457: period = enter + rt/ts/rn/sg.
    let payload = read_payload(&fixed_input());
    for (key, value) in [
        ("appId", "wb000000000000h123456"),
        ("b", "e2a324305186a1e2a6a1b3b"),
        ("c", "8f132430178f14e45fce0f7"),
        ("ci", "2"),
        ("co", "1543"),
        ("ct", "1795006900"),
        ("pc", "b65324607ab2f692g010282"),
        ("pr", "42"),
        ("ps", "psvts-fixture"),
        ("rn", "321"),
        ("rt", "30"),
        (
            "sg",
            "5ee6d3c0a2338c35db3a29c3135dc1fa388f4085619e3915640fb64669346863",
        ),
        ("ts", "1795006900321"),
    ] {
        assert_eq!(
            payload.get(key).map(QueryValue::js_string).as_deref(),
            Some(value),
            "{key}"
        );
    }
    assert_eq!(
        payload.get("s").map(QueryValue::js_string).as_deref(),
        Some("7999bdc4")
    );
}

#[test]
fn payload_clamps_and_recomputes_the_contract_fields() {
    // §14.4 line 453: `pr` 0-100, `pc` empty/0 recomputed as e(ct), `sm`
    // 20 code points, `ps` echoed verbatim.
    let mut input = fixed_input();
    input.progress = 150;
    input.pclts = Some("0".to_string());
    input.summary = "😀".repeat(30);
    let payload = read_payload(&input);
    assert_eq!(
        payload.get("pr").map(QueryValue::js_string).as_deref(),
        Some("100")
    );
    assert_eq!(
        payload.get("pc").map(QueryValue::js_string).as_deref(),
        Some(we("1795006900").as_str())
    );
    assert_eq!(
        payload.get("sm").map(QueryValue::js_string).as_deref(),
        Some("😀".repeat(20).as_str())
    );
    input.pclts = Some("pclts-from-page".to_string());
    let payload = enter_payload(&input);
    assert_eq!(
        payload.get("pc").map(QueryValue::js_string).as_deref(),
        Some("pclts-from-page")
    );
}

#[test]
fn cookies_round_trip_and_keep_only_the_account_boundary() {
    // §14.1: `wr_vid/wr_skey/wr_rt` plus the stable `ptcz/RK` group.
    let mut jar = CookieJar::new();
    jar.absorb(&[
        (
            "set-cookie".to_string(),
            "wr_vid=123; Path=/; HttpOnly, wr_skey=secret; Domain=.qq.com, tmp=1".to_string(),
        ),
        ("Set-Cookie".to_string(), "ptcz=stable; Path=/".to_string()),
    ]);
    assert_eq!(jar.get("wr_vid"), Some("123"));
    assert_eq!(jar.get("wr_skey"), Some("secret"));
    assert_eq!(jar.get("ptcz"), Some("stable"));
    assert_eq!(jar.get("tmp"), Some("1"));
    assert_eq!(
        jar.header(),
        "ptcz=stable; tmp=1; wr_skey=secret; wr_vid=123"
    );
    let persistent = jar.persistent();
    assert_eq!(persistent.len(), 3);
    assert_eq!(persistent.get("tmp"), None);
    let debug = format!("{jar:?}");
    assert!(!debug.contains("secret"), "{debug}");
    assert!(!debug.contains("123"), "{debug}");
    assert!(debug.contains("wr_skey"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn the_sign_in_flow_keeps_the_cookies_and_tokens() {
    let transport = Scripted::new(vec![
        Ok(HttpResponse::new(200, "<html>skills</html>")
            .with_header("Set-Cookie", "ptcz=stable; Path=/")),
        response(200, r#"{"uid":"uid-fixture"}"#),
        response(200, r#"{"succeed":false}"#),
        response(
            200,
            r#"{"succeed":true,"webLoginVid":"vid-fixture","accessToken":"key-fixture","refreshToken":"rt/fixture"}"#,
        )
        .map(|mut value| {
            value.headers.push((
                "Set-Cookie".to_string(),
                "wr_fp=fp-fixture; Path=/".to_string(),
            ));
            value
        }),
    ]);
    let mut client = WeReadWebClient::new(transport.clone());
    client.login_page().expect("page");
    let uid = client.login_uid().expect("uid");
    assert_eq!(uid, "uid-fixture");
    assert_eq!(
        client.confirm_url(&uid),
        "https://weread.qq.com/web/confirm?uid=uid-fixture"
    );
    assert_eq!(
        client.login_poll(&uid, "").expect("poll"),
        WebLoginPoll::Continue
    );
    let poll = client.login_poll(&uid, "1234").expect("poll");
    let WebLoginPoll::Confirmed(success) = &poll else {
        panic!("not confirmed: {poll:?}");
    };
    assert_eq!(success.vid, "vid-fixture");
    let jar = client.jar();
    assert_eq!(jar.get("wr_vid"), Some("vid-fixture"));
    assert_eq!(jar.get("wr_skey"), Some("key-fixture"));
    assert_eq!(jar.get("wr_rt"), Some("rt%2Ffixture"));
    assert_eq!(jar.get("wr_fp"), Some("fp-fixture"));
    let requests = transport.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[2].url,
        "https://weread.qq.com/api/auth/getLoginInfo?uid=uid-fixture&otp="
    );
    assert_eq!(
        requests[3].url,
        "https://weread.qq.com/api/auth/getLoginInfo?uid=uid-fixture&otp=1234"
    );
    assert!(requests.iter().any(|request| request
        .headers
        .iter()
        .any(|(k, v)| k == "Referer" && v.ends_with("/r/weread-skills"))));
}

#[test]
fn login_poll_covers_every_state_of_the_contract() {
    // §14.1 step 3 + MiuRead's logicCode state machine.
    let case = |body: &str| parse_login_poll(&HttpResponse::new(200, body)).expect("poll");
    assert_eq!(case(r#"{"succeed":false}"#), WebLoginPoll::Continue);
    assert_eq!(case(r#"{"logicCode":"NEED_OTP"}"#), WebLoginPoll::NeedOtp);
    assert_eq!(
        case(r#"{"logicCode":"OTP_NOT_MATCH"}"#),
        WebLoginPoll::OtpMismatch
    );
    assert_eq!(
        case(r#"{"logicCode":"LOGIN_DECLINED"}"#),
        WebLoginPoll::Declined
    );
    assert_eq!(
        case(r#"{"logicCode":"LOGIN_TIMEOUT"}"#),
        WebLoginPoll::Expired
    );
    assert_eq!(
        case(r#"{"logicCode":"OTP_EXPIRED"}"#),
        WebLoginPoll::Expired
    );
    assert!(matches!(
        parse_login_poll(&HttpResponse::new(401, "{}")),
        Err(WeReadError::WebAuthExpired)
    ));
    assert!(parse_login_poll(&HttpResponse::new(200, r#"{"succeed":true}"#)).is_err());
}

#[test]
fn the_reader_state_fields_are_read_from_the_page() {
    let html = r#"<html><script>window.__INITIAL_STATE__={"reader":{"psvts":"ps-1","pclts":"pc-2","token":"tok-3"}};</script></html>"#;
    let state = parse_reader_state(html);
    assert_eq!(state.psvts, "ps-1");
    assert_eq!(state.pclts.as_deref(), Some("pc-2"));
    assert_eq!(state.token.as_deref(), Some("tok-3"));
    let numeric = parse_reader_state(r#"{"psvts":1795,"pclts":0}"#);
    assert_eq!(numeric.psvts, "1795");
    assert_eq!(numeric.pclts.as_deref(), Some("0"));
    assert!(parse_reader_state("<html></html>").psvts.is_empty());
}

#[test]
fn a_confirmed_write_needs_succ_one_and_is_sent_once() {
    let transport = Scripted::new(vec![
        response(200, r#"{"succ":1}"#),
        response(200, r#"{"succ":0}"#),
        response(200, "not json"),
        Err(TransportError::Timeout),
        Err(TransportError::Network("offline".to_string())),
        response(401, "{}"),
    ]);
    let mut client = WeReadWebClient::with_jar(transport.clone(), CookieJar::new());
    client.jar.set("wr_vid", "v");
    client.jar.set("wr_skey", "k");
    let payload = read_payload(&fixed_input());
    assert!(client.write_period("100001", 7, &payload).is_ok());
    for error in [
        client.write_period("100001", 7, &payload).unwrap_err(),
        client.write_period("100001", 7, &payload).unwrap_err(),
        client.write_period("100001", 7, &payload).unwrap_err(),
        client.write_period("100001", 7, &payload).unwrap_err(),
    ] {
        assert!(matches!(error, WeReadError::UnconfirmedWrite(_)), "{error}");
    }
    assert!(matches!(
        client.write_period("100001", 7, &payload),
        Err(WeReadError::WebAuthExpired)
    ));
    // AC-05: every attempt is exactly one HTTP request, never a retry.
    let requests = transport.requests();
    assert_eq!(requests.len(), 6);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(
        body.get("b").and_then(Value::as_str),
        Some("e2a324305186a1e2a6a1b3b")
    );
    assert_eq!(
        requests[0]
            .headers
            .iter()
            .find(|(name, _)| name == "Referer")
            .map(|(_, value)| value.as_str()),
        Some("https://weread.qq.com/web/reader/e2a324305186a1e2a6a1b3bk8f132430178f14e45fce0f7")
    );
}

#[test]
fn a_blocked_write_is_not_reported_as_a_possible_commit() {
    let transport = Scripted::new(vec![Err(TransportError::Blocked("no https".to_string()))]);
    let mut client = WeReadWebClient::with_jar(transport, CookieJar::new());
    client.jar.set("wr_vid", "v");
    client.jar.set("wr_skey", "k");
    let payload = read_payload(&fixed_input());
    assert!(matches!(
        client.write_period("100001", 7, &payload),
        Err(WeReadError::Blocked(_))
    ));
}

#[test]
fn a_write_without_cookies_never_reaches_the_transport() {
    let transport = Scripted::new(vec![]);
    let mut client = WeReadWebClient::new(transport.clone());
    let payload = read_payload(&fixed_input());
    assert!(matches!(
        client.write_period("100001", 7, &payload),
        Err(WeReadError::NotAuthenticated)
    ));
    assert!(transport.requests().is_empty());
}

#[test]
fn credentials_never_reach_debug_output() {
    let session = WebSession {
        cookies: {
            let mut jar = CookieJar::new();
            jar.set("wr_vid", "vid-secret");
            jar.set("wr_skey", "skey-secret");
            jar
        },
        vid: "vid-secret".to_string(),
        logged_at: 1,
    };
    let debug = format!("{session:?}");
    for secret in ["vid-secret", "skey-secret"] {
        assert!(!debug.contains(secret), "{debug}");
    }
    let success = WebLoginSuccess {
        vid: "vid-secret".to_string(),
        access_token: "token-secret".to_string(),
        refresh_token: "refresh-secret".to_string(),
    };
    let debug = format!("{success:?}");
    assert!(!debug.contains("secret"), "{debug}");
    assert!(debug.contains("[REDACTED]"));
}

// AC-06: the transport's `Debug` must not print any credential the Web
// channel uses, in a header or in the sign-in poll query (§14.1/§14.7).
#[test]
fn transport_debug_masks_every_web_credential() {
    let request = HttpRequest::new(
        "GET",
        "https://weread.qq.com/api/auth/getLoginInfo?uid=uid-secret&otp=1234",
    )
    .with_header("vid", "vid-secret")
    .with_header("accessToken", "access-secret")
    .with_header(
        "Cookie",
        "wr_vid=wr-vid-secret; wr_skey=wr-skey-secret; wr_rt=wr-rt-secret",
    )
    .with_header("X-Vid", "x-vid-secret")
    .with_header("X-Skey", "x-skey-secret");
    let debug = format!("{request:?}");
    for secret in [
        "vid-secret",
        "access-secret",
        "wr-vid-secret",
        "wr-skey-secret",
        "wr-rt-secret",
        "x-vid-secret",
        "x-skey-secret",
        "1234",
    ] {
        assert!(!debug.contains(secret), "debug leaked {secret}: {debug}");
    }
    assert!(debug.contains("[REDACTED]"));
    assert!(debug.contains("[QUERY_REDACTED]"));

    let response =
        HttpResponse::new(200, "{}").with_header("Set-Cookie", "wr_skey=set-cookie-secret; Path=/");
    let debug = format!("{response:?}");
    assert!(!debug.contains("set-cookie-secret"), "{debug}");
}

// AC-05: the E-Ink heartbeat echoes the cloud fields it just read; it never
// invents a coordinate (contract §14.4 step 1).
#[test]
fn the_heartbeat_echoes_the_cloud_fields() {
    use crate::weread::mock::{MockTransport, MOCK_BOOK_ID};
    use crate::weread::WeReadClient;

    let mock = MockTransport::new();
    let mut client = WeReadClient::new(mock.clone(), mock.credentials());
    client
        .book_read_echo(MOCK_BOOK_ID, 1_795_006_900, 321, "+08:00")
        .expect("heartbeat");
    let reads = mock.reads();
    assert_eq!(reads.len(), 1);
    let body = &reads[0];
    // Every echoed field is the mock cloud's value, not a local guess.
    assert_eq!(body["appId"], "synthetic-app");
    assert_eq!(body["summary"], "synthetic summary");
    assert_eq!(body["chapterUid"], 7);
    assert_eq!(body["chapterIdx"], 7);
    assert_eq!(body["chapterOffset"], 1500);
    assert_eq!(body["progress"], 57);
    // §14.4: the heartbeat is not a position write.
    assert_eq!(body["readingTime"], 0);
    assert_eq!(mock.request_count("/book/read"), 1);
    assert_eq!(mock.request_count("/book/getProgress"), 1);
}
