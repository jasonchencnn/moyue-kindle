//! WeRead Web channel: sign-in cookies, precise progress writes and the heartbeat payloads.
//!
//! The wire contract is `docs/sync-contract.md` §14.1 (Web sign-in), §14.4 (the
//! `e()`/`sign()`/`sorted_query()` protocol, `enter`/`period` payloads, the
//! heartbeat echo) and §14.7 (error codes). HTTP is injected through
//! [`crate::sync::transport`], so this module has no network stack and no
//! global state.
//!
//! Two rules shape the write path:
//!
//! - A write is sent exactly once. Only HTTP 200 with `succ:1` is a success;
//!   a timeout, a dropped connection, `succ:0` or an unreadable body all mean
//!   the server may or may not have committed, which is
//!   [`WeReadError::UnconfirmedWrite`]. The caller must never resend it.
//! - Credentials are cookies (`wr_vid`, `wr_skey`, `wr_rt`). They never appear
//!   in `Debug` output; the jar only prints cookie names.

use std::collections::BTreeMap;
use std::fmt;

use md5::Md5;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};

use super::auth::percent_encode;
use super::{api_error_code, api_message, WeReadError};

/// The Web gateway. The whole §14.4 write protocol lives under it.
pub const WEB_BASE: &str = "https://weread.qq.com";
/// The page whose cookies seed a Web sign-in (§14.1 step 1).
pub const SKILLS_PATH: &str = "/r/weread-skills";
/// Anonymous `uid` used to build the confirmation QR code (§14.1 step 2).
pub const LOGIN_UID_PATH: &str = "/api/auth/getLoginUid";
/// Long-poll of the Web sign-in (§14.1 step 3).
pub const LOGIN_INFO_PATH: &str = "/api/auth/getLoginInfo";
/// Precise progress write (§14.4 step 2).
pub const READ_PATH: &str = "/web/book/read";
/// Reader page that carries `psvts`/`pclts`/`token` (weread.koplugin `protocol.lua`).
pub const READER_PATH: &str = "/web/reader/";
/// Public reader token of the Web client when the reader page has none (§14.4).
pub const DEFAULT_READER_TOKEN: &str = "3c5c8717f3daf09iop3423zafeqoi";
/// User agent of the Web client; `appId` is derived from it (§14.4).
pub const WEB_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36 Edg/135.0.0.0";

/// Cookies a Web account must keep between runs. Browser-only session cookies
/// are dropped: a second sign-in may rotate them (§14.1, MiuRead boundary).
const PERSISTENT_COOKIES: [&str; 3] = ["ptcz", "RK", "pgv_pvid"];

/// One value of a §14.4 payload. The query string spells booleans and null the
/// way JavaScript's `String(value)` does; the JSON body keeps their type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryValue {
    /// A string or a number rendered as a string (`ct`, `rn`, `b`, …).
    Text(String),
    /// A JSON number (`ci`, `co`, `pr`, `rt`, `ts`).
    Number(i64),
    /// A JSON boolean (`prevChapter`).
    Bool(bool),
    /// JSON null.
    Null,
}

impl QueryValue {
    /// The value as `String(value)` in JavaScript, used by `sorted_query()`.
    pub fn js_string(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(number) => number.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Null => "null".to_string(),
        }
    }

    /// The value as a JSON scalar for the request body.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Number(number) => Value::from(*number),
            Self::Bool(value) => Value::Bool(*value),
            Self::Null => Value::Null,
        }
    }
}

impl From<&str> for QueryValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<String> for QueryValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<i64> for QueryValue {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<bool> for QueryValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

/// A §14.4 payload, keyed the way the signature sorts it.
pub type WebPayload = BTreeMap<String, QueryValue>;

fn md5_hex(bytes: &[u8]) -> String {
    hex::encode(Md5::digest(bytes))
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The `e()` obfuscation of §14.4, transcribed from weread.koplugin `protocol.lua`.
///
/// Digits are chunked nine at a time and each chunk is its lowercase hex
/// value; anything else is the hex of its UTF-8 bytes. The chunk list is
/// prefixed by its own hex length and joined with `g`.
pub fn we(value: &str) -> String {
    let digest = md5_hex(value.as_bytes());
    let numeric = !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    let flag = if numeric { '3' } else { '4' };
    let mut chunks: Vec<String> = Vec::new();
    if numeric {
        let bytes = value.as_bytes();
        for start in (0..bytes.len()).step_by(9) {
            let end = (start + 9).min(bytes.len());
            let part = std::str::from_utf8(&bytes[start..end]).unwrap_or("0");
            let number = part.parse::<u64>().unwrap_or(0);
            chunks.push(format!("{number:x}"));
        }
    } else {
        chunks.push(
            value
                .bytes()
                .map(|byte| format!("{byte:x}"))
                .collect::<String>(),
        );
    }
    let mut out = format!("{}{flag}2{}", &digest[..3], &digest[30..]);
    for (index, chunk) in chunks.iter().enumerate() {
        out.push_str(&format!("{:02x}", chunk.len()));
        out.push_str(chunk);
        if index + 1 < chunks.len() {
            out.push('g');
        }
    }
    if out.len() < 20 {
        out.push_str(&digest[..20 - out.len()]);
    }
    let tail = md5_hex(out.as_bytes());
    out.push_str(&tail[..3]);
    out
}

/// The `sign()` hash of §14.4: two accumulators seeded with `0x15051505`,
/// mixed from the end of the query string two bytes at a time.
pub fn sign(query: &str) -> String {
    let bytes = query.as_bytes();
    let length = bytes.len();
    let mut a: u32 = 0x1505_1505;
    let mut b: u32 = 0x1505_1505;
    let mut i = length;
    while i > 1 {
        let shift_a = ((length - i + 1) % 30) as u32;
        let shift_b = ((i - 1) % 30) as u32;
        a = (a ^ ((bytes[i - 1] as u32) << shift_a)) & 0x7fff_ffff;
        b = (b ^ ((bytes[i - 2] as u32) << shift_b)) & 0x7fff_ffff;
        i -= 2;
    }
    format!("{:x}", a + b)
}

/// Sort the payload keys (except `s`) and percent-encode each value; `s` is
/// always added after signing.
pub fn sorted_query(params: &WebPayload) -> String {
    let mut out = String::new();
    for (key, value) in params {
        if key == "s" {
            continue;
        }
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(key);
        out.push('=');
        out.push_str(&percent_encode(&value.js_string()));
    }
    out
}

/// The browser `appId` of §14.4: the last digit of each of the first twelve
/// user-agent words, then `wb{prefix}h{hash}` with `hash = 131*hash + byte`.
pub fn web_app_id(user_agent: &str) -> String {
    let mut prefix = String::new();
    for word in user_agent.split_whitespace().take(12) {
        prefix.push(char::from(b'0' + (word.len() % 10) as u8));
    }
    let mut hash: u32 = 0;
    for byte in user_agent.bytes() {
        hash = 0x83u32.wrapping_mul(hash).wrapping_add(u32::from(byte)) & 0x7fff_ffff;
    }
    format!("wb{prefix}h{hash}")
}

/// The confirmation page the phone opens after scanning (§14.1 step 2).
pub fn web_confirm_url(uid: &str) -> String {
    format!("{WEB_BASE}/web/confirm?uid={}", percent_encode(uid))
}

/// The reader URL used as `Referer` on writes (§14.4 step 2).
pub fn reader_url(book_id: &str, chapter_uid: Option<i64>) -> String {
    let mut url = format!("{WEB_BASE}{READER_PATH}{}", we(book_id));
    if let Some(uid) = chapter_uid {
        url.push('k');
        url.push_str(&we(&uid.to_string()));
    }
    url
}

/// `sm`: at most twenty Unicode scalar values, never cut inside a character.
pub fn summary_prefix(summary: &str) -> String {
    summary.chars().take(20).collect()
}

/// Everything one `enter` or `period` payload needs.
///
/// `psvts`, `pclts` and `token` come from the reader page
/// (`window.__INITIAL_STATE__.reader`), not from local arithmetic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadPayloadInput {
    /// WeRead cloud book id.
    pub book_id: String,
    /// `chapterUid` of the position.
    pub chapter_uid: i64,
    /// `chapterIdx` of the position.
    pub chapter_idx: i64,
    /// Character offset inside the chapter.
    pub chapter_offset: i64,
    /// Whole-book progress, clamped to 0..=100.
    pub progress: i64,
    /// Text near the position, truncated to twenty code points.
    pub summary: String,
    /// Reader page `psvts`.
    pub psvts: String,
    /// Reader page `pclts`; absent, empty or `"0"` means `e(ct)`.
    pub pclts: Option<String>,
    /// `appId`; callers pass [`web_app_id`] of the user agent.
    pub app_id: String,
    /// `ct`, the current Unix second.
    pub now_s: i64,
    /// `rt`, active seconds of this period.
    pub elapsed_s: i64,
    /// `ts`, the current Unix millisecond.
    pub ts_ms: i64,
    /// `rn`, the 0..=999 random nonce.
    pub rn: u16,
    /// `sg` token, [`DEFAULT_READER_TOKEN`] when the page has none.
    pub token: String,
}

impl ReadPayloadInput {
    /// A payload input with the public token and the computed `appId`.
    pub fn new(book_id: impl Into<String>, chapter_uid: i64) -> Self {
        Self {
            book_id: book_id.into(),
            chapter_uid,
            app_id: web_app_id(WEB_USER_AGENT),
            token: DEFAULT_READER_TOKEN.to_string(),
            ..Self::default()
        }
    }
}

fn base_payload(input: &ReadPayloadInput) -> WebPayload {
    let progress = input.progress.clamp(0, 100);
    let pc = match input.pclts.as_deref() {
        Some(value) if !value.is_empty() && value != "0" => QueryValue::Text(value.to_string()),
        _ => QueryValue::Text(we(&input.now_s.to_string())),
    };
    let mut payload: WebPayload = BTreeMap::new();
    payload.insert("appId".to_string(), QueryValue::Text(input.app_id.clone()));
    payload.insert("b".to_string(), QueryValue::Text(we(&input.book_id)));
    payload.insert(
        "c".to_string(),
        QueryValue::Text(we(&input.chapter_uid.to_string())),
    );
    payload.insert("ci".to_string(), QueryValue::Number(input.chapter_idx));
    payload.insert("co".to_string(), QueryValue::Number(input.chapter_offset));
    payload.insert(
        "sm".to_string(),
        QueryValue::Text(summary_prefix(&input.summary)),
    );
    payload.insert("pr".to_string(), QueryValue::Number(progress));
    payload.insert("ct".to_string(), QueryValue::Number(input.now_s));
    payload.insert("ps".to_string(), QueryValue::Text(input.psvts.clone()));
    payload.insert("pc".to_string(), pc);
    payload
}

fn signed(mut payload: WebPayload) -> WebPayload {
    let query = sorted_query(&payload);
    payload.insert("s".to_string(), QueryValue::Text(sign(&query)));
    payload
}

/// The `enter` payload of §14.4: the position fields, no `rt/ts/rn/sg`.
pub fn enter_payload(input: &ReadPayloadInput) -> WebPayload {
    signed(base_payload(input))
}

/// The `period` payload of §14.4: the position fields plus `rt/ts/rn/sg`.
pub fn read_payload(input: &ReadPayloadInput) -> WebPayload {
    let mut payload = base_payload(input);
    payload.insert("rt".to_string(), QueryValue::Number(input.elapsed_s.max(0)));
    payload.insert("ts".to_string(), QueryValue::Number(input.ts_ms));
    payload.insert(
        "rn".to_string(),
        QueryValue::Number(i64::from(input.rn % 1000)),
    );
    let token = if input.token.is_empty() {
        DEFAULT_READER_TOKEN
    } else {
        &input.token
    };
    let signature = sha256_hex(&format!("{}{}{}", input.ts_ms, input.rn % 1000, token));
    payload.insert("sg".to_string(), QueryValue::Text(signature));
    signed(payload)
}

/// The payload as the JSON object of the request body.
pub fn payload_json(payload: &WebPayload) -> Value {
    let mut object = Map::new();
    for (key, value) in payload {
        object.insert(key.clone(), value.to_json());
    }
    Value::Object(object)
}

// ---------------------------------------------------------------- cookies

/// The Web sign-in cookie jar.
///
/// `Debug` lists names only: the values are credentials (§14.1).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CookieJar(BTreeMap<String, String>);

impl fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.0.keys().map(String::as_str).collect();
        f.debug_struct("CookieJar")
            .field("names", &names)
            .field("values", &"[REDACTED]")
            .finish()
    }
}

fn reserved_attribute(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "path" | "domain" | "expires" | "max-age" | "samesite" | "secure" | "httponly"
    )
}

/// Split one `Set-Cookie` line into its cookies, keeping commas inside
/// `Expires=...` (§14.1: one response may carry several cookies).
fn split_set_cookie(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_expires = false;
    for (index, ch) in line.char_indices() {
        let tail = line[index..].to_ascii_lowercase();
        if tail.starts_with("expires=") {
            in_expires = true;
        }
        if ch == ';' {
            in_expires = false;
        }
        if ch == ',' && !in_expires {
            let rest = line[index + 1..].trim_start();
            let name = rest.split('=').next().unwrap_or_default();
            if !name.is_empty() && !name.contains(';') && !name.contains(',') {
                out.push(&line[start..index]);
                start = index + 1;
            }
        }
    }
    out.push(&line[start..]);
    out
}

impl CookieJar {
    /// An empty jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cookie names, sorted, for logs that must not carry values.
    pub fn names(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }

    /// One cookie value.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// Set one cookie; an empty value removes it.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        if value.is_empty() {
            self.0.remove(&name);
        } else {
            self.0.insert(name, value);
        }
    }

    /// The `Cookie` request header: names sorted, `; ` separated.
    pub fn header(&self) -> String {
        self.0
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Merge the `Set-Cookie` headers of one response.
    pub fn absorb(&mut self, headers: &[(String, String)]) {
        for (name, value) in headers {
            if !name.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            for cookie in split_set_cookie(value) {
                let mut parts = cookie.split(';');
                let Some(pair) = parts.next() else { continue };
                let Some((key, value)) = pair.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                let value = value.trim();
                if key.is_empty() || reserved_attribute(key) {
                    continue;
                }
                self.set(key.to_string(), value.to_string());
            }
        }
    }

    /// The cookies that belong in the long-lived account (§14.1).
    pub fn persistent(&self) -> Self {
        let mut out = Self::new();
        for (name, value) in &self.0 {
            if name.starts_with("wr_") || PERSISTENT_COOKIES.contains(&name.as_str()) {
                out.set(name.clone(), value.clone());
            }
        }
        out
    }

    /// Number of cookies; a jar of zero names cannot authenticate.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the jar has no cookie at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The Web account's cookies and the vid they belong to.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WebSession {
    /// The long-lived cookie jar.
    pub cookies: CookieJar,
    /// `wr_vid`, kept next to the jar for the account line.
    pub vid: String,
    /// Unix seconds of the last successful sign-in.
    pub logged_at: i64,
}

impl fmt::Debug for WebSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSession")
            .field("cookies", &self.cookies)
            .field("vid", &"[REDACTED]")
            .field("logged_at", &self.logged_at)
            .finish()
    }
}

impl WebSession {
    /// Whether the jar has the two cookies a write needs.
    pub fn is_authenticated(&self) -> bool {
        self.cookies.get("wr_vid").is_some() && self.cookies.get("wr_skey").is_some()
    }
}

// ---------------------------------------------------------------- sign-in

/// One answer of `GET /api/auth/getLoginInfo` (§14.1 step 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebLoginPoll {
    /// Nobody confirmed yet (or the server returned no state).
    Continue,
    /// The phone shows a four-digit code: ask for it.
    NeedOtp,
    /// The typed code was wrong; ask again.
    OtpMismatch,
    /// The reader declined on the phone.
    Declined,
    /// The QR code or the OTP window expired.
    Expired,
    /// `succeed:true`; the cookies and tokens are in [`WebLoginSuccess`].
    Confirmed(WebLoginSuccess),
}

/// The tokens of a successful Web sign-in. Never `Debug`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WebLoginSuccess {
    /// `webLoginVid`.
    pub vid: String,
    /// `accessToken` (`wr_skey`).
    pub access_token: String,
    /// `refreshToken` (`wr_rt`), when the server sends one.
    pub refresh_token: String,
}

impl fmt::Debug for WebLoginSuccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebLoginSuccess")
            .field("vid", &"[REDACTED]")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .finish()
    }
}

/// Parse one `getLoginInfo` answer (§14.1 step 3).
pub fn parse_login_poll(response: &HttpResponse) -> Result<WebLoginPoll, WeReadError> {
    if let Some(error) = web_response_error(response.status, &response.body) {
        return Err(error);
    }
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("getLoginInfo: {error}")))?;
    if let Some(succeed) = value.get("succeed") {
        let done = match succeed {
            Value::Bool(flag) => *flag,
            Value::Number(number) => number.as_i64() == Some(1),
            Value::String(text) => text == "true" || text == "1",
            _ => false,
        };
        if done {
            let vid = scalar_string(value.get("webLoginVid"))
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    WeReadError::InvalidResponse("getLoginInfo has no webLoginVid".to_string())
                })?;
            let access_token = scalar_string(value.get("accessToken"))
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    WeReadError::InvalidResponse("getLoginInfo has no accessToken".to_string())
                })?;
            let refresh_token = scalar_string(value.get("refreshToken")).unwrap_or_default();
            return Ok(WebLoginPoll::Confirmed(WebLoginSuccess {
                vid,
                access_token,
                refresh_token,
            }));
        }
    }
    match value.get("logicCode").and_then(Value::as_str) {
        Some("NEED_OTP") => Ok(WebLoginPoll::NeedOtp),
        Some("OTP_NOT_MATCH") => Ok(WebLoginPoll::OtpMismatch),
        Some("LOGIN_DECLINED") => Ok(WebLoginPoll::Declined),
        Some("LOGIN_TIMEOUT") | Some("OTP_EXPIRED") => Ok(WebLoginPoll::Expired),
        Some("SUCCEED") => Err(WeReadError::InvalidResponse(
            "getLoginInfo said SUCCEED without succeed:true".to_string(),
        )),
        _ => Ok(WebLoginPoll::Continue),
    }
}

fn scalar_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

// --------------------------------------------------------------- reader page

/// The `reader.psvts/pclts/token` fields of the Web reader page (§14.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReaderState {
    /// `reader.psvts`; a write without it must not be sent.
    pub psvts: String,
    /// `reader.pclts`, when the page has one.
    pub pclts: Option<String>,
    /// `reader.token` (`sg` token), when the page has one.
    pub token: Option<String>,
}

fn js_field(html: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(offset) = html[from..].find(&needle) {
        let mut rest = &html[from + offset + needle.len()..];
        from += offset + needle.len();
        rest = rest.trim_start();
        let rest = rest.strip_prefix(':')?.trim_start();
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut out = String::new();
            let mut escaped = false;
            for ch in quoted.chars() {
                if escaped {
                    out.push(ch);
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    break;
                } else {
                    out.push(ch);
                }
            }
            if !out.is_empty() {
                return Some(out);
            }
        } else {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                return Some(digits);
            }
        }
    }
    None
}

/// Extract `psvts`, `pclts` and `token` from the reader page.
///
/// The page embeds `window.__INITIAL_STATE__` as JavaScript, not JSON, so the
/// three scalars are scanned by name, the way weread.koplugin does.
pub fn parse_reader_state(html: &str) -> ReaderState {
    ReaderState {
        psvts: js_field(html, "psvts").unwrap_or_default(),
        pclts: js_field(html, "pclts"),
        token: js_field(html, "token"),
    }
}

// ------------------------------------------------------------------ client

fn web_response_error(status: u16, body: &[u8]) -> Option<WeReadError> {
    if let Some(code) = api_error_code(body) {
        if code != 0 {
            return Some(match code {
                -2011 | -2012 | -2041 => WeReadError::WebAuthExpired,
                -10102 | -2014 => WeReadError::RateLimited(code),
                other => WeReadError::Api {
                    code: other,
                    message: api_message(
                        &serde_json::from_slice::<Value>(body).unwrap_or(Value::Null),
                    ),
                },
            });
        }
    }
    if status == 401 {
        return Some(WeReadError::WebAuthExpired);
    }
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(body);
        let text: String = text.chars().take(200).collect();
        return Some(WeReadError::HttpStatus(status, text));
    }
    None
}

fn receipt_error(response: &HttpResponse) -> Option<WeReadError> {
    if response.status == 401 {
        return Some(WeReadError::WebAuthExpired);
    }
    if let Some(code) = api_error_code(&response.body) {
        if matches!(code, -2011 | -2012 | -2041) {
            return Some(WeReadError::WebAuthExpired);
        }
    }
    None
}

/// A confirmed write: HTTP 200 and `succ:1` (§14.4).
fn parse_write_receipt(response: &HttpResponse) -> Result<(), WeReadError> {
    if let Some(error) = receipt_error(response) {
        return Err(error);
    }
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|_| WeReadError::UnconfirmedWrite(format!("http {}", response.status)))?;
    let success = match value.get("succ").or_else(|| value.get("succeed")) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_i64() == Some(1),
        Some(Value::String(text)) => text == "1" || text == "true",
        _ => false,
    };
    if success {
        Ok(())
    } else {
        let detail = value
            .get("msg")
            .or_else(|| value.get("errmsg"))
            .and_then(Value::as_str)
            .unwrap_or("succ was not 1");
        Err(WeReadError::UnconfirmedWrite(detail.to_string()))
    }
}

/// The Web client: one cookie jar, one injected transport, no retries on writes.
pub struct WeReadWebClient<T: HttpTransport> {
    transport: T,
    base: String,
    user_agent: String,
    jar: CookieJar,
}

impl<T: HttpTransport> WeReadWebClient<T> {
    /// A client with an empty jar.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            base: WEB_BASE.to_string(),
            user_agent: WEB_USER_AGENT.to_string(),
            jar: CookieJar::new(),
        }
    }

    /// A client that continues an existing sign-in.
    pub fn with_jar(transport: T, jar: CookieJar) -> Self {
        Self {
            jar,
            ..Self::new(transport)
        }
    }

    /// Override the gateway (tests and mocks).
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// The current jar.
    pub fn jar(&self) -> &CookieJar {
        &self.jar
    }

    /// The jar after the last response.
    pub fn into_jar(self) -> CookieJar {
        self.jar
    }

    /// The current `wr_vid`, when signed in.
    pub fn vid(&self) -> Option<&str> {
        self.jar.get("wr_vid")
    }

    fn request(&self, method: &str, url: &str, referer: &str) -> HttpRequest {
        let mut request = HttpRequest::new(method, url)
            .with_header("Accept", "application/json, text/plain, */*")
            .with_header("User-Agent", self.user_agent.clone())
            .with_header("Referer", referer);
        if !self.jar.is_empty() {
            request = request.with_header("Cookie", self.jar.header());
        }
        request
    }

    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, WeReadError> {
        let response = self.transport.send(request)?;
        self.jar.absorb(&response.headers);
        if let Some(error) = web_response_error(response.status, &response.body) {
            return Err(error);
        }
        Ok(response)
    }

    fn get(&mut self, path: &str, referer: &str) -> Result<HttpResponse, WeReadError> {
        let url = format!("{}{path}", self.base);
        let request = self.request("GET", &url, referer);
        self.send(request)
    }

    /// `GET /r/weread-skills`: the page whose `Set-Cookie` opens a sign-in.
    pub fn login_page(&mut self) -> Result<(), WeReadError> {
        let referer = format!("{}/", self.base);
        self.get(SKILLS_PATH, &referer)?;
        Ok(())
    }

    /// `GET /api/auth/getLoginUid`: the anonymous id behind the QR code.
    pub fn login_uid(&mut self) -> Result<String, WeReadError> {
        let referer = format!("{}{SKILLS_PATH}", self.base);
        let response = self.get(LOGIN_UID_PATH, &referer)?;
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|error| WeReadError::InvalidResponse(format!("getLoginUid: {error}")))?;
        scalar_string(value.get("uid"))
            .filter(|uid| !uid.is_empty())
            .ok_or_else(|| WeReadError::InvalidResponse("getLoginUid has no uid".to_string()))
    }

    /// The URL the phone opens after scanning.
    pub fn confirm_url(&self, uid: &str) -> String {
        let base = self.base.trim_end_matches('/');
        format!("{base}/web/confirm?uid={}", percent_encode(uid))
    }

    /// `GET /api/auth/getLoginInfo`: one poll of the sign-in state machine.
    ///
    /// `otp` is the four-digit code shown on the phone, empty before it is
    /// asked for. Cookies of every answer are kept: the final answer sets
    /// `wr_vid`/`wr_skey`/`wr_rt`.
    pub fn login_poll(&mut self, uid: &str, otp: &str) -> Result<WebLoginPoll, WeReadError> {
        let referer = format!("{}{SKILLS_PATH}", self.base);
        let mut url = format!(
            "{}{LOGIN_INFO_PATH}?uid={}&otp=",
            self.base,
            percent_encode(uid)
        );
        if !otp.is_empty() {
            url.push_str(&percent_encode(otp));
        }
        let request = self.request("GET", &url, &referer);
        let response = self.send(request)?;
        let poll = parse_login_poll(&response)?;
        if let WebLoginPoll::Confirmed(success) = &poll {
            self.apply_login(success);
        }
        Ok(poll)
    }

    /// Merge a confirmed sign-in into the jar (`wr_vid`, `wr_skey`, `wr_rt`).
    pub fn apply_login(&mut self, success: &WebLoginSuccess) {
        self.jar.set("wr_vid", success.vid.clone());
        self.jar.set("wr_skey", success.access_token.clone());
        if !success.refresh_token.is_empty() {
            self.jar
                .set("wr_rt", percent_encode(&success.refresh_token));
        }
    }

    /// Fetch the reader page and read `psvts`/`pclts`/`token` from it.
    pub fn reader_state(&mut self, book_id: &str) -> Result<ReaderState, WeReadError> {
        let url = format!("{}{READER_PATH}{}", self.base, we(book_id));
        let referer = format!("{}/", self.base);
        let request = self.request("GET", &url, &referer);
        let response = self.send(request)?;
        let html = String::from_utf8_lossy(&response.body);
        let state = parse_reader_state(&html);
        if state.psvts.is_empty() {
            return Err(WeReadError::MissingReaderState(
                "reader.psvts is absent".to_string(),
            ));
        }
        Ok(state)
    }

    fn post_read(
        &mut self,
        book_id: &str,
        chapter_uid: i64,
        payload: &WebPayload,
    ) -> Result<(), WeReadError> {
        if self.jar.get("wr_vid").is_none() || self.jar.get("wr_skey").is_none() {
            return Err(WeReadError::NotAuthenticated);
        }
        let url = format!("{}{READ_PATH}", self.base);
        let referer = reader_url(book_id, Some(chapter_uid));
        let body = serde_json::to_vec(&payload_json(payload))
            .map_err(|error| WeReadError::InvalidParameter(format!("read body: {error}")))?;
        let request = self
            .request("POST", &url, &referer)
            .with_header("Origin", self.base.trim_end_matches('/'))
            .with_header("Content-Type", "application/json; charset=UTF-8")
            .with_body(body);
        match self.transport.send(request) {
            Ok(response) => {
                self.jar.absorb(&response.headers);
                parse_write_receipt(&response)
            }
            Err(TransportError::Blocked(detail)) => Err(WeReadError::Blocked(detail)),
            Err(TransportError::TooLarge(limit)) => Err(WeReadError::TooLarge(limit)),
            Err(error) => Err(WeReadError::UnconfirmedWrite(error.to_string())),
        }
    }

    /// Send the `enter` payload of §14.4 (session first report, no `rt/ts/rn/sg`).
    pub fn write_enter(
        &mut self,
        book_id: &str,
        chapter_uid: i64,
        payload: &WebPayload,
    ) -> Result<(), WeReadError> {
        self.post_read(book_id, chapter_uid, payload)
    }

    /// Send the `period` payload of §14.4 (position plus time fields).
    pub fn write_period(
        &mut self,
        book_id: &str,
        chapter_uid: i64,
        payload: &WebPayload,
    ) -> Result<(), WeReadError> {
        self.post_read(book_id, chapter_uid, payload)
    }
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod tests;
