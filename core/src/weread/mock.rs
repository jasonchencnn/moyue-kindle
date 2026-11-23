//! In-memory WeRead E-Ink mock transport with synthetic fixtures only.
//!
//! No real vid, book id, text or token appears here. The mock validates the §14.5 signatures
//! against the request body, rotates refresh tokens once, and can inject §14.7 error codes.

use std::sync::{Arc, Mutex};

use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest;

use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};

use super::auth::Credentials;
use super::client::{inner_signature, outer_signature};

/// Synthetic account id. Never a real WeRead vid.
pub const MOCK_VID: &str = "1234567890";
/// Synthetic device id.
pub const MOCK_DEVICE_ID: &str = "eink-test-device-0001";
/// Synthetic book id.
pub const MOCK_BOOK_ID: &str = "100001";
/// Synthetic ZIP password.
pub const MOCK_ZIP_PASSWORD: &str = "synthetic-zip-pass";
/// Synthetic config token.
pub const MOCK_CONFIG_TOKEN: &str = "config-token-fixture";
/// Synthetic guest token.
pub const MOCK_GUEST_TOKEN: &str = "guest-token-fixture";
/// Synthetic ZIP bytes.
pub const MOCK_CHAPTER_ZIP: &[u8] = b"PK\x03\x04synthetic-chapter-zip";

/// A request captured by the mock.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// The request body as JSON, when it is JSON.
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

struct State {
    vid: String,
    device_id: String,
    access_token: String,
    refresh_token: String,
    access_invalid: bool,
    next_error: Option<(u16, i64, String)>,
    next_write_status: Option<u16>,
    refresh_count: usize,
    requests: Vec<RecordedRequest>,
    logins: Vec<Value>,
    uploads: Vec<Value>,
    reads: Vec<Value>,
    qr_polled: bool,
    config_token: String,
    guest_token: String,
}

/// In-memory `HttpTransport` for the WeRead client.
#[derive(Clone)]
pub struct MockTransport {
    state: Arc<Mutex<State>>,
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTransport {
    /// A mock preloaded with synthetic shelf, progress, TOC and chapter bytes.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                vid: MOCK_VID.to_string(),
                device_id: MOCK_DEVICE_ID.to_string(),
                access_token: "test-access-token".to_string(),
                refresh_token: "test-refresh-token".to_string(),
                access_invalid: false,
                next_error: None,
                next_write_status: None,
                refresh_count: 0,
                requests: Vec::new(),
                logins: Vec::new(),
                uploads: Vec::new(),
                reads: Vec::new(),
                qr_polled: false,
                config_token: MOCK_CONFIG_TOKEN.to_string(),
                guest_token: MOCK_GUEST_TOKEN.to_string(),
            })),
        }
    }

    /// Credentials accepted by the mock.
    pub fn credentials(&self) -> Credentials {
        let state = self.state.lock().unwrap();
        Credentials::new(
            &state.vid,
            &state.access_token,
            &state.refresh_token,
            &state.device_id,
        )
    }

    /// The `encryptkey` envelope derived from the synthetic vid.
    pub fn envelope(&self) -> String {
        make_envelope(MOCK_VID, MOCK_ZIP_PASSWORD)
    }

    /// Every request the mock received, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    /// How many requests targeted `path`.
    pub fn request_count(&self, path: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.url.contains(path))
            .count()
    }

    /// How many refresh-token exchanges happened.
    pub fn refresh_count(&self) -> usize {
        self.state.lock().unwrap().refresh_count
    }

    /// Make every authenticated request fail with HTTP 401 until a refresh succeeds.
    pub fn invalidate_access_token(&self) {
        self.state.lock().unwrap().access_invalid = true;
    }

    /// Inject one API error code into the next authenticated request.
    pub fn set_next_error(&self, status: u16, code: i64, message: &str) {
        self.state.lock().unwrap().next_error = Some((status, code, message.to_string()));
    }

    /// Inject one HTTP status into the next write request (no read refresh can hide it).
    pub fn fail_next_write(&self, status: u16) {
        self.state.lock().unwrap().next_write_status = Some(status);
    }

    /// Captured `batchUploadProgress` bodies.
    pub fn uploads(&self) -> Vec<Value> {
        self.state.lock().unwrap().uploads.clone()
    }

    /// Captured `book/read` bodies.
    pub fn reads(&self) -> Vec<Value> {
        self.state.lock().unwrap().reads.clone()
    }

    /// Captured `/login` bodies.
    pub fn logins(&self) -> Vec<Value> {
        self.state.lock().unwrap().logins.clone()
    }
}

impl HttpTransport for MockTransport {
    fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut state = self.state.lock().unwrap();
        state.requests.push(RecordedRequest {
            method: req.method.clone(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: req.body.clone(),
        });
        let path = path_of(&req.url);
        let query = query_of(&req.url);

        match (req.method.as_str(), path.as_str()) {
            ("GET", "/wxticket") => Ok(json_response(
                200,
                json!({"signature": "mock-signature", "timeStamp": 1_700_000_000i64}),
            )),
            ("GET", "/connect/sdk/qrconnect") => Ok(json_response(
                200,
                json!({"errcode": 0, "uuid": "synthetic-uuid"}),
            )),
            ("GET", "/connect/l/qrconnect") => {
                if query.contains("last=408") || state.qr_polled {
                    Ok(json_response(
                        200,
                        json!({"wx_errcode": 405, "wx_code": "wx-code-fixture"}),
                    ))
                } else {
                    state.qr_polled = true;
                    Ok(json_response(200, json!({"wx_errcode": 408})))
                }
            }
            ("POST", "/login") => handle_login(&mut state, &req),
            _ => {
                if let Err(response) = authorize(&state, &req) {
                    return Ok(response);
                }
                if let Some((status, code, message)) = state.next_error.take() {
                    return Ok(error_response(status, code, &message));
                }
                if is_write(&req.method, path.as_str()) {
                    if let Some(status) = state.next_write_status.take() {
                        return Ok(error_response(status, -2012, "injected write failure"));
                    }
                }
                match (req.method.as_str(), path.as_str()) {
                    ("GET", "/shelf/sync") => Ok(shelf_response()),
                    ("GET", "/book/getProgress") => Ok(progress_response()),
                    ("POST", "/book/chapterInfos") => Ok(chapters_response()),
                    ("GET", "/config") => {
                        Ok(json_response(200, json!({"token": state.config_token})))
                    }
                    ("GET", "/feature") => Ok(json_response(
                        200,
                        json!({"feature": {"guest_token": state.guest_token}}),
                    )),
                    ("GET", "/book/chapterdownload") => Ok(HttpResponse::new(
                        200,
                        MOCK_CHAPTER_ZIP.to_vec(),
                    )
                    .with_header("encryptkey", make_envelope(&state.vid, MOCK_ZIP_PASSWORD))),
                    ("POST", "/book/batchUploadProgress") => {
                        let body = match serde_json::from_slice::<Value>(&req.body) {
                            Ok(body) => body,
                            Err(_) => return Ok(error_response(400, -1, "invalid JSON")),
                        };
                        if let Err(response) = verify_batch(&state, &body) {
                            return Ok(response);
                        }
                        state.uploads.push(body);
                        Ok(json_response(200, json!({"succ": 1})))
                    }
                    ("POST", "/book/read") => {
                        let body = match serde_json::from_slice::<Value>(&req.body) {
                            Ok(body) => body,
                            Err(_) => return Ok(error_response(400, -1, "invalid JSON")),
                        };
                        if let Err(response) = verify_read(&state, &body) {
                            return Ok(response);
                        }
                        state.reads.push(body);
                        Ok(json_response(200, json!({"succ": 1})))
                    }
                    _ => Ok(error_response(404, -1, "unknown endpoint")),
                }
            }
        }
    }
}

fn handle_login(state: &mut State, req: &HttpRequest) -> Result<HttpResponse, TransportError> {
    let body: Value = match serde_json::from_slice(&req.body) {
        Ok(body) => body,
        Err(_) => return Ok(error_response(400, -1, "invalid JSON")),
    };
    state.logins.push(body.clone());
    if body.get("refreshToken").is_some() {
        if body.get("refreshToken").and_then(Value::as_str) != Some(state.refresh_token.as_str()) {
            return Ok(error_response(401, -2012, "refresh token refused"));
        }
        state.refresh_count += 1;
        state.access_token = format!("access-{}", state.refresh_count + 1);
        state.refresh_token = format!("refresh-{}", state.refresh_count + 1);
        state.access_invalid = false;
        return Ok(json_response(
            200,
            json!({
                "vid": state.vid,
                "accessToken": state.access_token,
                "refreshToken": state.refresh_token,
            }),
        ));
    }
    let timestamp = body.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
    let random = body.get("random").and_then(Value::as_u64).unwrap_or(0);
    let device_id = body.get("deviceId").and_then(Value::as_str).unwrap_or("");
    let code = body.get("code").and_then(Value::as_str).unwrap_or("");
    let expected = hex::encode(sha2::Sha256::digest(
        format!("{timestamp}{device_id}{random}").as_bytes(),
    ));
    if code != "wx-code-fixture"
        || device_id != state.device_id
        || body.get("signature").and_then(Value::as_str) != Some(expected.as_str())
    {
        return Ok(error_response(401, -2012, "login signature refused"));
    }
    Ok(json_response(
        200,
        json!({
            "vid": state.vid,
            "accessToken": state.access_token,
            "refreshToken": state.refresh_token,
        }),
    ))
}

fn authorize(state: &State, req: &HttpRequest) -> Result<(), HttpResponse> {
    let header = |name: &str| {
        req.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    if state.access_invalid
        || header("vid") != Some(state.vid.as_str())
        || header("accessToken") != Some(state.access_token.as_str())
    {
        return Err(error_response(401, -2012, "access token refused"));
    }
    Ok(())
}

fn is_write(method: &str, path: &str) -> bool {
    method == "POST" && matches!(path, "/book/batchUploadProgress" | "/book/read")
}

fn verify_batch(state: &State, body: &Value) -> Result<(), HttpResponse> {
    let book = body
        .get("books")
        .and_then(Value::as_array)
        .and_then(|books| books.first())
        .ok_or_else(|| error_response(400, -1, "missing book"))?;
    let text = |key: &str| {
        book.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let number = |key: &str| book.get(key).and_then(Value::as_i64).unwrap_or(0);
    let mut parts = vec![
        state.vid.clone(),
        text("deviceId"),
        text("appId"),
        text("bookId"),
        number("risk").to_string(),
        text("recordCreateTimeZone"),
        number("readingTime").to_string(),
        "0".to_string(),
    ];
    for hour in book
        .get("hours")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        parts.push(format!(
            "{}_{}_{}_{}",
            hour.get("startOfHour").and_then(Value::as_i64).unwrap_or(0),
            hour.get("timeZone").and_then(Value::as_str).unwrap_or(""),
            hour.get("readingTime").and_then(Value::as_i64).unwrap_or(0),
            hour.get("ttsTime").and_then(Value::as_i64).unwrap_or(0),
        ));
    }
    let expected = inner_signature(&[
        state.guest_token.clone(),
        number("random").to_string(),
        number("timestamp").to_string(),
        parts.join("_"),
    ]);
    if text("signature") != expected {
        return Err(error_response(400, -1, "inner signature mismatch"));
    }
    let outer = outer_signature(
        body.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        &state.config_token,
        body.get("random").and_then(Value::as_u64).unwrap_or(0) as u16,
    );
    if body.get("signature").and_then(Value::as_str) != Some(outer.as_str()) {
        return Err(error_response(400, -1, "outer signature mismatch"));
    }
    Ok(())
}

fn verify_read(state: &State, body: &Value) -> Result<(), HttpResponse> {
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let number = |key: &str| body.get(key).and_then(Value::as_i64).unwrap_or(0);
    let parts = [
        state.vid.clone(),
        text("deviceId"),
        text("appId"),
        text("bookId"),
        number("risk").to_string(),
        text("recordCreateTimeZone"),
        number("readingTime").to_string(),
        "0".to_string(),
    ];
    let expected = inner_signature(&[
        state.guest_token.clone(),
        number("random").to_string(),
        number("timestamp").to_string(),
        parts.join("_"),
    ]);
    if text("signature") != expected {
        return Err(error_response(400, -1, "read signature mismatch"));
    }
    Ok(())
}

fn shelf_response() -> HttpResponse {
    json_response(
        200,
        json!({
            "books": [
                {"bookId": MOCK_BOOK_ID, "title": "Synthetic Book One", "format": "epub",
                 "finished": false, "secret": false, "progress": 57, "readingTime": 320},
                {"bookId": "100002", "title": "Synthetic Book Two", "format": "txt",
                 "finished": true, "secret": true, "progress": 100, "readingTime": 60}
            ],
            "archive": [],
            "synckey": 7,
            "bookCount": 2
        }),
    )
}

fn progress_response() -> HttpResponse {
    json_response(
        200,
        json!({
            "bookId": MOCK_BOOK_ID,
            "book": {
                "appId": "synthetic-app",
                "bookVersion": 42,
                "chapterUid": 7,
                "chapterIdx": 7,
                "chapterOffset": 1500,
                "updateTime": 1_795_000_000i64,
                "synckey": 7,
                "summary": "synthetic summary",
                "progress": 57,
                "readingTime": 320
            },
            "tts": {"appId": "synthetic-tts", "chapterUid": 7, "chapterIdx": 7, "chapterOffset": 1400},
            "canFreeRead": 0,
            "timestamp": 1_795_000_000i64
        }),
    )
}

fn chapters_response() -> HttpResponse {
    let chapter = |uid: i64, title: &str, price: i64, paid: i64| {
        json!({
            "chapterUid": uid, "chapterIdx": uid, "title": title, "wordCount": 100,
            "price": price, "paid": paid, "files": [format!("Text/{uid}.xhtml")]
        })
    };
    json_response(
        200,
        json!({
            "data": [{
                "bookId": MOCK_BOOK_ID,
                "synckey": 7,
                "updated": [
                    chapter(0, "Public resources", 0, 0),
                    chapter(7, "Synthetic chapter", 0, 0),
                    chapter(8, "Paid chapter", 100, 0),
                    chapter(9, "Purchased chapter", 100, 1)
                ]
            }]
        }),
    )
}

fn error_response(status: u16, code: i64, message: &str) -> HttpResponse {
    json_response(status, json!({"errcode": code, "errmsg": message}))
}

fn json_response(status: u16, value: Value) -> HttpResponse {
    HttpResponse::new(status, serde_json::to_vec(&value).unwrap_or_default())
        .with_header("Content-Type", "application/json")
}

fn path_of(url: &str) -> String {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let without_query = without_scheme.split('?').next().unwrap_or(without_scheme);
    match without_query.find('/') {
        Some(index) => without_query[index..].to_string(),
        None => "/".to_string(),
    }
}

fn query_of(url: &str) -> String {
    url.split_once('?')
        .map(|(_, query)| query.to_string())
        .unwrap_or_default()
}

/// Build a §14.3 `encryptkey` envelope: AES-CBC with key/IV derived from the vid.
pub fn make_envelope(vid: &str, password: &str) -> String {
    type Aes128Cbc = cbc::Encryptor<aes::Aes128>;
    assert!(!vid.is_empty(), "make_envelope needs a vid");
    let mut material = Vec::with_capacity(32);
    while material.len() < 32 {
        material.extend_from_slice(vid.as_bytes());
    }
    material.truncate(32);
    let mut buffer = password.as_bytes().to_vec();
    let message_len = buffer.len();
    buffer.resize(message_len + 16, 0);
    let cipher = Aes128Cbc::new_from_slices(&material[..16], &material[16..32])
        .expect("synthetic key and iv");
    let ciphertext = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, message_len)
        .expect("synthetic padding");
    base64::engine::general_purpose::STANDARD.encode(ciphertext)
}
