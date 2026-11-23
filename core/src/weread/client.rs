//! WeRead E-Ink client over an injected [`HttpTransport`] (contract §14.3–§14.5).

use std::collections::BTreeMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport};

use super::auth::{self, Credentials, EINK_BASE};
use super::model::{
    BaseBookReadPostBody, BatchUploadBody, BookProgressResponse, ChapterInfo, ChapterInfos,
    ChapterInfosResponse, ReadingHour, ReadingInterval, ShelfSync,
};
use super::notes::{parse_bookmark_list, parse_review_list, BookmarkList, ReviewList};
use super::{api_error, response_error, WeReadError};

/// E-Ink 2.1.2 signature remap table (contract §14.5, index = original byte).
const REMAP: [u8; 256] = [
    52, 202, 85, 64, 29, 182, 147, 198, 49, 48, 41, 53, 50, 167, 184, 17, 194, 181, 22, 250, 139,
    177, 36, 164, 16, 144, 4, 233, 8, 248, 59, 138, 156, 140, 68, 249, 188, 92, 105, 226, 161, 218,
    210, 211, 117, 137, 247, 30, 45, 80, 86, 215, 114, 83, 191, 34, 251, 32, 15, 1, 46, 69, 135,
    110, 102, 72, 242, 224, 205, 254, 103, 169, 67, 244, 148, 81, 206, 165, 74, 238, 19, 38, 142,
    204, 170, 51, 20, 93, 14, 57, 187, 207, 145, 43, 129, 77, 234, 153, 236, 26, 44, 133, 197, 217,
    54, 116, 75, 24, 225, 241, 61, 157, 65, 159, 180, 23, 13, 214, 76, 190, 220, 175, 151, 40, 119,
    240, 98, 255, 113, 193, 200, 39, 143, 108, 104, 168, 155, 230, 89, 28, 27, 18, 9, 152, 78, 63,
    6, 55, 0, 186, 31, 10, 25, 47, 201, 213, 208, 87, 73, 111, 253, 37, 228, 97, 12, 66, 203, 150,
    100, 95, 219, 173, 96, 35, 141, 154, 109, 195, 196, 94, 62, 185, 146, 106, 189, 91, 7, 127,
    118, 149, 237, 79, 171, 132, 122, 128, 231, 120, 199, 229, 235, 115, 131, 107, 252, 56, 70,
    125, 71, 101, 179, 82, 99, 58, 5, 209, 239, 163, 166, 222, 158, 60, 2, 174, 178, 123, 160, 246,
    243, 42, 192, 172, 134, 3, 90, 84, 11, 245, 130, 212, 126, 227, 223, 176, 216, 221, 33, 232,
    124, 136, 162, 121, 88, 112, 183, 21,
];

const CHAPTERDOWNLOAD_PF: &str = "wechat_wx-2001-android-100-weread";

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn rotate(value: &[u8]) -> Vec<u8> {
    if value.is_empty() {
        return Vec::new();
    }
    let checksum = value.iter().fold(0u8, |acc, byte| acc ^ byte);
    let offset = (checksum as usize % 11) % value.len();
    if offset == 0 {
        return value.to_vec();
    }
    let split = value.len() - offset;
    [&value[split..], &value[..split]].concat()
}

/// Inner signature of §14.5: prefix + remapped parts, sorted by bytes, double SHA-256 with rotate.
pub fn inner_signature<S: AsRef<str>>(parts: &[S]) -> String {
    let mut encoded: Vec<Vec<u8>> = Vec::with_capacity(parts.len() + 1);
    encoded.push(b"5a6f1".to_vec());
    for part in parts {
        encoded.push(
            part.as_ref()
                .bytes()
                .map(|byte| REMAP[byte as usize])
                .collect(),
        );
    }
    encoded.sort();
    let joined = encoded.concat();
    let first = sha256_hex(&rotate(&joined));
    sha256_hex(&rotate(first.as_bytes()))
}

/// Outer signature of §14.5: `sha256hex(timestamp_ms + config_token + outer_random)`.
pub fn outer_signature(timestamp_ms: i64, config_token: &str, random: u16) -> String {
    sha256_hex(format!("{timestamp_ms}{config_token}{random}").as_bytes())
}

/// Decrypt the `encryptkey` envelope into the chapter ZIP password (§14.3).
pub fn decrypt_chapter_envelope(vid: &str, envelope: &str) -> Result<String, WeReadError> {
    type Aes128Cbc = cbc::Decryptor<aes::Aes128>;

    let envelope = envelope.trim();
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(envelope)
        .map_err(|error| WeReadError::Envelope(format!("base64: {error}")))?;
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(WeReadError::Envelope(format!(
            "ciphertext length {} is not a positive multiple of 16",
            ciphertext.len()
        )));
    }
    let vid_bytes = vid.as_bytes();
    if vid_bytes.is_empty() {
        return Err(WeReadError::Envelope("empty vid".to_string()));
    }
    let mut material = Vec::with_capacity(32);
    while material.len() < 32 {
        material.extend_from_slice(vid_bytes);
    }
    material.truncate(32);

    let mut buffer = ciphertext;
    let plaintext = Aes128Cbc::new_from_slices(&material[..16], &material[16..32])
        .map_err(|_| WeReadError::Envelope("invalid key/iv length".to_string()))?
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| WeReadError::Envelope("invalid PKCS7 padding".to_string()))?;
    let password = String::from_utf8(plaintext.to_vec())
        .map_err(|error| WeReadError::Envelope(format!("utf8: {error}")))?;
    if password.is_empty() {
        return Err(WeReadError::Envelope("empty password".to_string()));
    }
    Ok(password)
}

fn valid_time_zone(zone: &str) -> bool {
    let bytes = zone.as_bytes();
    if bytes.len() != 6 || !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
        return false;
    }
    if !bytes[1..3].iter().all(u8::is_ascii_digit) || !bytes[4..6].iter().all(u8::is_ascii_digit) {
        return false;
    }
    let hours = (bytes[1] - b'0') * 10 + (bytes[2] - b'0');
    let minutes = (bytes[4] - b'0') * 10 + (bytes[5] - b'0');
    hours <= 23 && minutes <= 59
}

/// Split reading intervals into GMT+8-aligned hour buckets (§14.5).
///
/// Unix hour boundaries are the same instants as GMT+8 hour boundaries, so no offset is added.
pub fn split_reading_hours(intervals: &[ReadingInterval]) -> Result<Vec<ReadingHour>, WeReadError> {
    let mut buckets: BTreeMap<(i64, String), ReadingHour> = BTreeMap::new();
    for interval in intervals {
        if interval.duration <= 0 {
            return Err(WeReadError::InvalidParameter(
                "reading interval duration must be positive".to_string(),
            ));
        }
        if !valid_time_zone(&interval.time_zone) {
            return Err(WeReadError::InvalidParameter(format!(
                "invalid time zone {}",
                interval.time_zone
            )));
        }
        if interval.tts_time < 0 || interval.tts_time > interval.duration {
            return Err(WeReadError::InvalidParameter(
                "tts_time must be within the interval duration".to_string(),
            ));
        }
        let mut at = interval.start_time;
        let mut left = interval.duration;
        let mut tts_left = interval.tts_time;
        while left > 0 {
            let hour = at.div_euclid(3600) * 3600;
            let seconds = left.min(hour.saturating_add(3600).saturating_sub(at));
            if seconds <= 0 {
                return Err(WeReadError::InvalidParameter(
                    "reading interval overflows the hour boundary".to_string(),
                ));
            }
            let tts = tts_left.min(seconds);
            let bucket = buckets
                .entry((hour, interval.time_zone.clone()))
                .or_insert_with(|| ReadingHour {
                    start_of_hour: hour,
                    time_zone: interval.time_zone.clone(),
                    reading_time: 0,
                    tts_time: 0,
                });
            bucket.reading_time = bucket.reading_time.checked_add(seconds).ok_or_else(|| {
                WeReadError::InvalidParameter("reading time overflow".to_string())
            })?;
            bucket.tts_time = bucket
                .tts_time
                .checked_add(tts)
                .ok_or_else(|| WeReadError::InvalidParameter("tts time overflow".to_string()))?;
            if bucket.reading_time > 3600 {
                return Err(WeReadError::InvalidParameter(
                    "overlapping reading-time hour".to_string(),
                ));
            }
            at = at
                .checked_add(seconds)
                .ok_or_else(|| WeReadError::InvalidParameter("timestamp overflow".to_string()))?;
            left -= seconds;
            tts_left -= tts;
        }
    }
    if buckets.is_empty() {
        return Err(WeReadError::InvalidParameter(
            "reading upload needs at least one interval".to_string(),
        ));
    }
    Ok(buckets.into_values().collect())
}

/// Everything the caller controls for one `batchUploadProgress` request.
#[derive(Debug, Clone)]
pub struct BatchUploadRequest {
    pub book_id: String,
    pub book_version: i64,
    pub intervals: Vec<ReadingInterval>,
    pub now_s: i64,
    pub random: u16,
    pub outer_timestamp_ms: i64,
    pub outer_random: u16,
    pub time_zone: String,
    pub risk: i64,
}

impl BatchUploadRequest {
    /// A request with both randoms and `risk` at their defaults; the caller may override them.
    pub fn new(
        book_id: impl Into<String>,
        book_version: i64,
        intervals: Vec<ReadingInterval>,
        now_s: i64,
        time_zone: impl Into<String>,
    ) -> Self {
        Self {
            book_id: book_id.into(),
            book_version,
            intervals,
            now_s,
            random: 0,
            outer_timestamp_ms: now_s.saturating_mul(1000),
            outer_random: 0,
            time_zone: time_zone.into(),
            risk: 0,
        }
    }
}

/// Build the double-signed body (§14.5) without sending it.
pub fn build_batch_upload_body(
    credentials: &Credentials,
    config_token: &str,
    guest_token: &str,
    request: &BatchUploadRequest,
) -> Result<BatchUploadBody, WeReadError> {
    if request.book_id.is_empty() {
        return Err(WeReadError::InvalidParameter("empty book id".to_string()));
    }
    if config_token.is_empty() || guest_token.is_empty() {
        return Err(WeReadError::InvalidParameter(
            "config/guest token is empty".to_string(),
        ));
    }
    if !valid_time_zone(&request.time_zone) {
        return Err(WeReadError::InvalidParameter(format!(
            "invalid time zone {}",
            request.time_zone
        )));
    }
    let hours = split_reading_hours(&request.intervals)?;
    let total: i64 = hours.iter().map(|hour| hour.reading_time).sum();
    let mut book = BaseBookReadPostBody {
        book_id: request.book_id.clone(),
        book_version: request.book_version,
        app_id: String::new(),
        device_id: credentials.device_id.clone(),
        install_id: String::new(),
        summary: String::new(),
        progress: 0,
        current_progress: 0,
        auto_time: 0,
        is_lecture: 0,
        is_resend_reading_info: 1,
        lecture_time: 0,
        lecture_text_time: 0,
        noval_time: 0,
        tts_time: 0,
        finish: false,
        risk: request.risk,
        reading_time: total,
        record_create_time_zone: request.time_zone.clone(),
        timestamp: request.now_s,
        random: request.random,
        hours: hours.clone(),
        signature: String::new(),
        chapter_uid: None,
        chapter_idx: None,
        chapter_offset: None,
    };
    let mut parts = vec![
        credentials.vid.clone(),
        credentials.device_id.clone(),
        book.app_id.clone(),
        book.book_id.clone(),
        book.risk.to_string(),
        book.record_create_time_zone.clone(),
        book.reading_time.to_string(),
        "0".to_string(),
    ];
    for hour in &book.hours {
        parts.push(format!(
            "{}_{}_{}_{}",
            hour.start_of_hour, hour.time_zone, hour.reading_time, hour.tts_time
        ));
    }
    book.signature = inner_signature(&[
        guest_token.to_string(),
        request.random.to_string(),
        request.now_s.to_string(),
        parts.join("_"),
    ]);
    Ok(BatchUploadBody {
        books: vec![book],
        timestamp: request.outer_timestamp_ms,
        random: request.outer_random,
        signature: outer_signature(
            request.outer_timestamp_ms,
            config_token,
            request.outer_random,
        ),
    })
}

/// Book formats accepted by `/book/chapterdownload`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookFormat {
    Epub,
    Txt,
}

impl BookFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Txt => "txt",
        }
    }
}

/// A downloaded chapter ZIP and the password that opens it. `Debug` masks the password.
#[derive(Clone, PartialEq, Eq)]
pub struct ChapterPackage {
    pub zip: Vec<u8>,
    pub password: String,
}

impl fmt::Debug for ChapterPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChapterPackage")
            .field("zip_len", &self.zip.len())
            .field("password", &"[REDACTED]")
            .finish()
    }
}

fn default_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn default_random() -> u16 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_micros())
        .unwrap_or(0)
        % 1000) as u16
}

/// WeRead E-Ink client. Credentials belong to this client; no session is global.
pub struct WeReadClient<T: HttpTransport> {
    transport: T,
    base_url: String,
    credentials: Option<Credentials>,
    time_fn: Box<dyn Fn() -> i64 + Send + Sync>,
    random_fn: Box<dyn Fn() -> u16 + Send + Sync>,
}

impl<T: HttpTransport> WeReadClient<T> {
    /// A client for an already authenticated account.
    pub fn new(transport: T, credentials: Credentials) -> Self {
        Self {
            transport,
            base_url: EINK_BASE.to_string(),
            credentials: Some(credentials),
            time_fn: Box::new(default_now_ms),
            random_fn: Box::new(default_random),
        }
    }

    /// Override the gateway base URL (tests and mocks).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Set a deterministic millisecond clock (tests).
    pub fn with_time_provider(
        mut self,
        provider: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        self.time_fn = Box::new(provider);
        self
    }

    /// Set a deterministic random source (tests).
    pub fn with_random_provider(
        mut self,
        provider: impl Fn() -> u16 + Send + Sync + 'static,
    ) -> Self {
        self.random_fn = Box::new(provider);
        self
    }

    /// The account credentials, if authenticated.
    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    /// Replace the credentials after a sign-in.
    pub fn set_credentials(&mut self, credentials: Credentials) {
        self.credentials = Some(credentials);
    }

    /// Send a read-only request, refreshing the token once on HTTP 401.
    ///
    /// Contract §14.7: `-2041` and `-10102` are terminal and never retried.
    fn send_read<F>(&mut self, build: F) -> Result<HttpResponse, WeReadError>
    where
        F: Fn(&Credentials) -> HttpRequest,
    {
        let credentials = self
            .credentials
            .clone()
            .ok_or(WeReadError::NotAuthenticated)?;
        let response = self.transport.send(build(&credentials))?;
        let Some(error) = response_error(response.status, &response.body) else {
            return Ok(response);
        };
        let retryable = matches!(
            error,
            WeReadError::AuthExpired | WeReadError::AuthRejected(-2012)
        );
        if !retryable {
            return Err(error);
        }
        self.refresh()?;
        let credentials = self
            .credentials
            .clone()
            .ok_or(WeReadError::NotAuthenticated)?;
        let response = self.transport.send(build(&credentials))?;
        if let Some(error) = response_error(response.status, &response.body) {
            return Err(error);
        }
        Ok(response)
    }

    /// Send a write request without retrying (§14.7); a 401 is returned to the caller.
    fn send_write(&self, request: HttpRequest) -> Result<HttpResponse, WeReadError> {
        let response = self.transport.send(request)?;
        if let Some(error) = response_error(response.status, &response.body) {
            return Err(error);
        }
        Ok(response)
    }

    /// Rotate the refresh token through `POST /login` and store the new credentials.
    pub fn refresh(&mut self) -> Result<(), WeReadError> {
        let credentials = self
            .credentials
            .clone()
            .ok_or(WeReadError::NotAuthenticated)?;
        let timestamp = (self.time_fn)();
        let random = (self.random_fn)();
        let body = auth::build_refresh_body(&credentials, timestamp, random);
        let request = auth::build_refresh_request(&self.base_url, &body)?;
        let response = self.transport.send(request)?;
        let renewed = auth::parse_refresh_response(&response, &credentials)?;
        self.credentials = Some(renewed);
        Ok(())
    }

    fn fetch_config_token(&mut self) -> Result<String, WeReadError> {
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(&base_url, "GET", "/config?token=1", credentials, None)
        })?;
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|error| WeReadError::InvalidResponse(format!("config: {error}")))?;
        let token = value
            .get("token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| WeReadError::InvalidResponse("config has no token".to_string()))?;
        Ok(token.to_string())
    }

    fn fetch_guest_token(&mut self) -> Result<String, WeReadError> {
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(&base_url, "GET", "/feature", credentials, None)
        })?;
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|error| WeReadError::InvalidResponse(format!("feature: {error}")))?;
        Ok(value
            .pointer("/feature/guest_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .unwrap_or(auth::DEFAULT_GUEST_TOKEN)
            .to_string())
    }

    /// Fetch the full shelf and the `synckey` cursor.
    pub fn shelf_sync(&mut self) -> Result<ShelfSync, WeReadError> {
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(&base_url, "GET", "/shelf/sync", credentials, None)
        })?;
        parse_json(&response, "shelf/sync")
    }

    /// Fetch one book's cloud highlights since `synckey` (§14.3).
    ///
    /// `synckey` starts at `0`; every answer carries the cursor of the next
    /// delta. A response with neither a cursor nor a change list is refused
    /// by [`super::notes::parse_bookmark_list`].
    pub fn bookmark_list(
        &mut self,
        book_id: &str,
        synckey: i64,
    ) -> Result<BookmarkList, WeReadError> {
        if book_id.is_empty() {
            return Err(WeReadError::InvalidParameter("empty book id".to_string()));
        }
        let path = format!(
            "/book/bookmarklist?bookId={}&synckey={synckey}",
            auth::percent_encode(book_id)
        );
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(&base_url, "GET", &path, credentials, None)
        })?;
        parse_bookmark_list(&response.body)
    }

    /// Fetch the reader's own thoughts on `book_id` (§14.3).
    ///
    /// `synckey` starts at `0`; `count` is clamped to the endpoint's 1..=100.
    pub fn review_list(
        &mut self,
        book_id: &str,
        synckey: i64,
        count: u32,
    ) -> Result<ReviewList, WeReadError> {
        if book_id.is_empty() {
            return Err(WeReadError::InvalidParameter("empty book id".to_string()));
        }
        let count = count.clamp(1, 100);
        let path = format!(
            "/review/list?bookId={}&listType=1&listMode=0&mine=1&synckey={synckey}&count={count}",
            auth::percent_encode(book_id)
        );
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(&base_url, "GET", &path, credentials, None)
        })?;
        parse_review_list(&response.body)
    }

    /// Read the cloud reading position for `book_id`.
    pub fn get_progress(&mut self, book_id: &str) -> Result<BookProgressResponse, WeReadError> {
        if book_id.is_empty() {
            return Err(WeReadError::InvalidParameter("empty book id".to_string()));
        }
        let path = format!("/book/getProgress?bookId={}", auth::percent_encode(book_id));
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(&base_url, "GET", &path, credentials, None)
        })?;
        parse_json(&response, "getProgress")
    }

    /// Fetch and normalize the table of contents for `book_id`.
    pub fn chapter_infos(&mut self, book_id: &str) -> Result<ChapterInfos, WeReadError> {
        if book_id.is_empty() {
            return Err(WeReadError::InvalidParameter("empty book id".to_string()));
        }
        let body = serde_json::json!({
            "bookIds": [book_id],
            "synckeys": [0],
            "updateTimes": [0],
            "maxfreeIdx": [0],
        });
        let body = serde_json::to_vec(&body).map_err(|error| {
            WeReadError::InvalidParameter(format!("chapterInfos body: {error}"))
        })?;
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(
                &base_url,
                "POST",
                "/book/chapterInfos",
                credentials,
                Some(body.clone()),
            )
        })?;
        let wire: ChapterInfosResponse = parse_json(&response, "chapterInfos")?;
        Ok(wire.into_chapters(book_id))
    }

    /// Download one chapter ZIP after enforcing the §14.3 entitlement red line.
    ///
    /// Includes shared resource chapter `0` for EPUB downloads. Use
    /// [`Self::chapterdownload_with_public`] with `include_public = false` once
    /// chapter `0` has already been cached for the book.
    pub fn chapterdownload(
        &mut self,
        book_id: &str,
        book_format: BookFormat,
        book_version: &str,
        chapters: &[ChapterInfo],
    ) -> Result<ChapterPackage, WeReadError> {
        self.chapterdownload_with_public(book_id, book_format, book_version, chapters, true)
    }

    /// Download one chapter ZIP, optionally requesting shared resource chapter `0`.
    pub fn chapterdownload_with_public(
        &mut self,
        book_id: &str,
        book_format: BookFormat,
        book_version: &str,
        chapters: &[ChapterInfo],
        include_public: bool,
    ) -> Result<ChapterPackage, WeReadError> {
        if book_id.is_empty() {
            return Err(WeReadError::InvalidParameter("empty book id".to_string()));
        }
        if chapters.is_empty() {
            return Err(WeReadError::InvalidParameter(
                "no chapters requested".to_string(),
            ));
        }
        let mut uids = Vec::with_capacity(chapters.len() + 1);
        for chapter in chapters {
            let uid = chapter.uid()?;
            if !chapter.is_eligible() {
                return Err(WeReadError::IneligibleChapter {
                    chapter_uid: uid,
                    price: chapter.price.unwrap_or(-1.0),
                    paid: chapter.paid.unwrap_or(0),
                });
            }
            if !uids.contains(&uid) {
                uids.push(uid);
            }
        }
        let version = match book_format {
            BookFormat::Txt => "0".to_string(),
            BookFormat::Epub => {
                if book_version.is_empty() {
                    return Err(WeReadError::InvalidParameter(
                        "EPUB download needs a book version".to_string(),
                    ));
                }
                if include_public && !uids.contains(&0) {
                    uids.insert(0, 0);
                }
                book_version.to_string()
            }
        };
        let chapters_param = uids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let path = format!(
            "/book/chapterdownload?bookId={}&chapters={}&bookType={}&bookVersion={}&pf={}&pfkey=pfKey&zoneId=1&quote=&release=1&stopAutoPayWhenBNE=1&preload=2&preview=0&offline=1",
            auth::percent_encode(book_id),
            chapters_param,
            book_format.as_str(),
            auth::percent_encode(&version),
            auth::percent_encode(CHAPTERDOWNLOAD_PF),
        );
        let vid = self
            .credentials
            .as_ref()
            .ok_or(WeReadError::NotAuthenticated)?
            .vid
            .clone();
        let base_url = self.base_url.clone();
        let response = self.send_read(move |credentials| {
            authenticated_request(&base_url, "GET", &path, credentials, None)
        })?;
        let envelope = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("encryptkey"))
            .map(|(_, value)| value.clone())
            .ok_or(WeReadError::MissingEncryptKey)?;
        let password = decrypt_chapter_envelope(&vid, &envelope)?;
        Ok(ChapterPackage {
            zip: response.body,
            password,
        })
    }

    /// Upload reading-time intervals with the §14.5 double signature.
    pub fn batch_upload_progress(
        &mut self,
        request: &BatchUploadRequest,
    ) -> Result<(), WeReadError> {
        let config_token = self.fetch_config_token()?;
        let guest_token = self.fetch_guest_token()?;
        let credentials = self
            .credentials
            .clone()
            .ok_or(WeReadError::NotAuthenticated)?;
        let body = build_batch_upload_body(&credentials, &config_token, &guest_token, request)?;
        let bytes = serde_json::to_vec(&body)
            .map_err(|error| WeReadError::InvalidParameter(format!("upload body: {error}")))?;
        let request = authenticated_request(
            &self.base_url,
            "POST",
            "/book/batchUploadProgress",
            &credentials,
            Some(bytes),
        );
        let response = self.send_write(request)?;
        parse_receipt(&response, "batchUploadProgress")
    }

    /// Heartbeat `POST /book/read`, echoing the cloud `appId/summary/coordinate` values (§14.4).
    pub fn book_read_echo(
        &mut self,
        book_id: &str,
        now_s: i64,
        random: u16,
        time_zone: &str,
    ) -> Result<(), WeReadError> {
        if !valid_time_zone(time_zone) {
            return Err(WeReadError::InvalidParameter(format!(
                "invalid time zone {time_zone}"
            )));
        }
        let progress = self.get_progress(book_id)?;
        let book = progress.book.ok_or_else(|| {
            WeReadError::InvalidResponse("getProgress returned no book".to_string())
        })?;
        let position = book.position().ok_or_else(|| {
            WeReadError::InvalidResponse("cloud position has no coordinate triple".to_string())
        })?;
        let guest_token = self.fetch_guest_token()?;
        let credentials = self
            .credentials
            .clone()
            .ok_or(WeReadError::NotAuthenticated)?;
        let app_id = book.app_id.clone().unwrap_or_default();
        let summary = book.summary.clone().unwrap_or_default();
        let progress_value = book.progress.unwrap_or(0);
        let mut body = BaseBookReadPostBody {
            book_id: book_id.to_string(),
            book_version: book.book_version.unwrap_or(0),
            app_id: app_id.clone(),
            device_id: credentials.device_id.clone(),
            install_id: String::new(),
            summary,
            progress: progress_value,
            current_progress: progress_value,
            auto_time: 0,
            is_lecture: 0,
            is_resend_reading_info: 1,
            lecture_time: 0,
            lecture_text_time: 0,
            noval_time: 0,
            tts_time: 0,
            finish: false,
            risk: 0,
            reading_time: 0,
            record_create_time_zone: time_zone.to_string(),
            timestamp: now_s,
            random,
            hours: Vec::new(),
            signature: String::new(),
            chapter_uid: Some(position.chapter_uid),
            chapter_idx: Some(position.chapter_idx),
            chapter_offset: Some(position.chapter_offset),
        };
        let parts = [
            credentials.vid.clone(),
            credentials.device_id.clone(),
            app_id,
            body.book_id.clone(),
            body.risk.to_string(),
            body.record_create_time_zone.clone(),
            body.reading_time.to_string(),
            "0".to_string(),
        ];
        body.signature = inner_signature(&[
            guest_token,
            random.to_string(),
            now_s.to_string(),
            parts.join("_"),
        ]);
        let bytes = serde_json::to_vec(&body)
            .map_err(|error| WeReadError::InvalidParameter(format!("read body: {error}")))?;
        let request = authenticated_request(
            &self.base_url,
            "POST",
            "/book/read",
            &credentials,
            Some(bytes),
        );
        let response = self.send_write(request)?;
        parse_receipt(&response, "book/read")
    }
}

fn authenticated_request(
    base_url: &str,
    method: &str,
    path: &str,
    credentials: &Credentials,
    body: Option<Vec<u8>>,
) -> HttpRequest {
    let mut request = HttpRequest::new(
        method,
        format!("{}{}", base_url.trim_end_matches('/'), path),
    );
    for (name, value) in auth::auth_headers(credentials) {
        request = request.with_header(name, value);
    }
    if let Some(body) = body {
        request = request
            .with_header("Content-Type", "application/json; charset=UTF-8")
            .with_body(body);
    }
    request
}

fn parse_json<T: serde::de::DeserializeOwned>(
    response: &HttpResponse,
    context: &str,
) -> Result<T, WeReadError> {
    if let Some(error) = response_error(response.status, &response.body) {
        return Err(error);
    }
    serde_json::from_slice(&response.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("{context}: {error}")))
}

fn flexible_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => Some(value.as_i64().unwrap_or(0) == 1),
        Value::String(value) => Some(value == "1" || value.eq_ignore_ascii_case("true")),
        _ => None,
    }
}

fn parse_receipt(response: &HttpResponse, context: &str) -> Result<(), WeReadError> {
    if let Some(error) = response_error(response.status, &response.body) {
        return Err(error);
    }
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("{context}: {error}")))?;
    let succ = value.get("succ").and_then(flexible_bool).unwrap_or(false);
    if !succ {
        if let Some(code) = value
            .get("errcode")
            .or_else(|| value.get("errCode"))
            .and_then(|code| code.as_i64())
        {
            if code != 0 {
                return Err(api_error(code, "unconfirmed upload"));
            }
        }
        return Err(WeReadError::UnconfirmedUpload);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::transport::HttpRequest;
    use crate::weread::auth::{
        build_login_body, build_login_request, build_qrconnect_url, parse_login_response,
        parse_qr_ticket, parse_qrconnect_response, poll_status, QrPollStatus, DEFAULT_DEVICE_NAME,
    };
    use crate::weread::mock::{
        make_envelope, MockTransport, MOCK_BOOK_ID, MOCK_CHAPTER_ZIP, MOCK_DEVICE_ID,
        MOCK_ZIP_PASSWORD,
    };
    use crate::weread::model::ChapterPackageManifest;

    fn client(mock: &MockTransport) -> WeReadClient<MockTransport> {
        WeReadClient::new(mock.clone(), mock.credentials()).with_base_url("https://i.weread.qq.com")
    }

    fn login(mock: &MockTransport) -> Credentials {
        let ticket = parse_qr_ticket(
            &mock
                .send(HttpRequest::new(
                    "GET",
                    "https://i.weread.qq.com/wxticket?nonceStr=weread",
                ))
                .expect("ticket"),
        )
        .expect("ticket json");
        let url = build_qrconnect_url(&ticket);
        let uuid =
            parse_qrconnect_response(&mock.send(HttpRequest::new("GET", &url)).expect("qrconnect"))
                .expect("uuid");
        let first = poll_status(
            &mock
                .send(HttpRequest::new(
                    "GET",
                    format!(
                        "https://long.open.weixin.qq.com/connect/l/qrconnect?f=json&uuid={uuid}"
                    ),
                ))
                .expect("poll"),
        )
        .expect("poll json");
        assert_eq!(first, QrPollStatus::Continue);
        let confirmed = poll_status(
            &mock
                .send(HttpRequest::new(
                    "GET",
                    format!("https://long.open.weixin.qq.com/connect/l/qrconnect?f=json&uuid={uuid}&last=408"),
                ))
                .expect("poll"),
        )
        .expect("poll json");
        let QrPollStatus::Confirmed(code) = confirmed else {
            panic!("expected confirmation");
        };
        let body = build_login_body(
            code,
            MOCK_DEVICE_ID,
            DEFAULT_DEVICE_NAME,
            "eink31000000000000000000000001",
            1_795_006_800_000,
            123,
        );
        let request = build_login_request("https://i.weread.qq.com", &body).expect("request");
        parse_login_response(&mock.send(request).expect("login"), MOCK_DEVICE_ID).expect("creds")
    }

    fn upload_request() -> BatchUploadRequest {
        let mut request = BatchUploadRequest::new(
            MOCK_BOOK_ID,
            42,
            vec![ReadingInterval::new(1_795_008_600, 5_400, "+08:00")],
            1_795_006_800,
            "+08:00",
        );
        request.random = 123;
        request.outer_timestamp_ms = 1_795_006_800_000;
        request.outer_random = 456;
        request
    }

    // AC-03: synthetic vid + self-made AES envelope -> ZIP password; bad length is an error.
    #[test]
    fn chapter_envelope_round_trip_and_bad_length() {
        let envelope = make_envelope("9876543210", "zip-password");
        assert_eq!(
            decrypt_chapter_envelope("9876543210", &envelope).expect("password"),
            "zip-password"
        );
        let bad_len = base64::engine::general_purpose::STANDARD.encode([0u8; 15]);
        assert!(matches!(
            decrypt_chapter_envelope("9876543210", &bad_len),
            Err(WeReadError::Envelope(_))
        ));
        assert!(matches!(
            decrypt_chapter_envelope("9876543210", "%%%"),
            Err(WeReadError::Envelope(_))
        ));
    }

    // AC-04: fixed vid/device/token/hour buckets. The vector was produced by the independent
    // Node implementation in docs/tasks/WR-001/notes.md and is tied to contract §14.5.
    #[test]
    fn batch_signature_matches_the_golden_vector() {
        let credentials = Credentials::new("1234567890", "a", "r", "eink-test-device-0001");
        let body = build_batch_upload_body(
            &credentials,
            "config-token-fixture",
            "5ecdcfd7f",
            &upload_request(),
        )
        .expect("body");
        assert_eq!(
            body.books[0].signature,
            "990f0f7f08d7739d65c0e0d5ea362441d105603ffe9980a539164bac8271f067"
        );
        assert_eq!(
            body.signature,
            "9c12d71938fa52a47e304888257156151fec0c87efeeee5fa1e67bc0427e1daa"
        );
        assert_eq!(body.books[0].reading_time, 5_400);
        assert_eq!(body.books[0].hours.len(), 2);
        assert_eq!(body.books[0].hours[0].reading_time, 1_800);
        assert_eq!(body.books[0].hours[1].reading_time, 3_600);
    }

    #[test]
    fn reading_hours_split_at_the_gmt8_boundary() {
        let hours = split_reading_hours(&[ReadingInterval::new(1_795_008_600, 5_400, "+08:00")])
            .expect("hours");
        assert_eq!(hours.len(), 2);
        assert_eq!(hours[0].start_of_hour, 1_795_006_800);
        assert_eq!(hours[0].reading_time, 1_800);
        assert_eq!(hours[1].start_of_hour, 1_795_010_400);
        assert_eq!(hours[1].reading_time, 3_600);
        assert!(split_reading_hours(&[ReadingInterval::new(0, 0, "+08:00")]).is_err());
        assert!(split_reading_hours(&[ReadingInterval::new(0, 1, "UTC")]).is_err());
    }

    // AC-05: price>0 && paid=0 is refused before any HTTP request.
    #[test]
    fn paid_chapter_is_refused_before_http() {
        let mock = MockTransport::new();
        let mut client = client(&mock);
        let paid = ChapterInfo {
            chapter_uid: Some(8),
            price: Some(100.0),
            paid: Some(0),
            ..Default::default()
        };
        let error = client
            .chapterdownload(MOCK_BOOK_ID, BookFormat::Epub, "42", &[paid])
            .expect_err("must refuse");
        assert!(matches!(
            error,
            WeReadError::IneligibleChapter { chapter_uid: 8, .. }
        ));
        assert_eq!(mock.request_count("/book/chapterdownload"), 0);
    }

    // AC-06: login -> shelf -> progress -> TOC -> download -> upload, with one 401 refresh.
    #[test]
    fn mock_end_to_end_login_to_upload() {
        let mock = MockTransport::new();
        let credentials = login(&mock);
        let mut client =
            WeReadClient::new(mock.clone(), credentials).with_base_url("https://i.weread.qq.com");

        let shelf = client.shelf_sync().expect("shelf");
        assert_eq!(shelf.books.len(), 2);
        assert_eq!(shelf.synckey, Some(7));

        mock.invalidate_access_token();
        let shelf = client.shelf_sync().expect("shelf after refresh");
        assert_eq!(shelf.books.len(), 2);
        assert_eq!(mock.refresh_count(), 1);
        assert_eq!(mock.request_count("/shelf/sync"), 3);

        let progress = client.get_progress(MOCK_BOOK_ID).expect("progress");
        let position = progress
            .book
            .as_ref()
            .and_then(|book| book.position())
            .expect("coords");
        assert_eq!(position.chapter_uid, 7);
        assert_eq!(position.chapter_offset, 1_500);

        let toc = client.chapter_infos(MOCK_BOOK_ID).expect("toc");
        assert_eq!(toc.chapters.len(), 4);
        let chapter_7 = toc
            .chapters
            .iter()
            .find(|chapter| chapter.chapter_uid == Some(7))
            .expect("chapter 7")
            .clone();
        let package = client
            .chapterdownload(MOCK_BOOK_ID, BookFormat::Epub, "42", &[chapter_7])
            .expect("download");
        assert_eq!(package.zip, MOCK_CHAPTER_ZIP);
        assert_eq!(package.password, MOCK_ZIP_PASSWORD);
        assert!(mock
            .requests()
            .iter()
            .any(|request| request.url.contains("chapters=0,7")));

        client
            .batch_upload_progress(&upload_request())
            .expect("upload");
        assert_eq!(mock.uploads().len(), 1);

        client
            .book_read_echo(MOCK_BOOK_ID, 1_795_006_900, 321, "+08:00")
            .expect("heartbeat");
        let read = &mock.reads()[0];
        assert_eq!(read["appId"], "synthetic-app");
        assert_eq!(read["summary"], "synthetic summary");
        assert_eq!(read["chapterUid"], 7);
        assert_eq!(read["chapterOffset"], 1_500);
        assert_eq!(read["progress"], 57);
        assert_eq!(read["deviceId"], MOCK_DEVICE_ID);
    }

    // §14.7: -2041 and -10102 are terminal; no refresh and no retry.
    #[test]
    fn manual_verification_and_rate_limit_are_not_retried() {
        let mock = MockTransport::new();
        let mut client = client(&mock);
        mock.set_next_error(200, -2041, "verify");
        assert!(matches!(
            client.shelf_sync(),
            Err(WeReadError::ManualVerification(-2041))
        ));
        assert_eq!(mock.request_count("/shelf/sync"), 1);
        assert_eq!(mock.refresh_count(), 0);

        mock.set_next_error(200, -10102, "rate limit");
        assert!(matches!(
            client.shelf_sync(),
            Err(WeReadError::RateLimited(-10102))
        ));
        assert_eq!(mock.request_count("/shelf/sync"), 2);
        assert_eq!(mock.refresh_count(), 0);
    }

    // AC-06: only read endpoints refresh; a write 401 is returned to the caller.
    #[test]
    fn writes_never_refresh_or_retry() {
        let mock = MockTransport::new();
        let mut client = client(&mock);
        mock.fail_next_write(401);
        assert!(matches!(
            client.batch_upload_progress(&upload_request()),
            Err(WeReadError::AuthRejected(-2012))
        ));
        assert_eq!(mock.refresh_count(), 0);
        assert_eq!(mock.request_count("/book/batchUploadProgress"), 1);
    }

    #[test]
    fn models_ignore_unknown_fields_and_parse_manifest() {
        let shelf: ShelfSync = serde_json::from_str(
            r#"{"books":[{"bookId":"1","title":"t","unknown":1}],"synckey":3,"extra":true}"#,
        )
        .expect("shelf");
        assert_eq!(shelf.books[0].book_id, "1");
        let manifest = ChapterPackageManifest::from_json(
            br#"{"chapters":[{"chapterUid":7,"files":["Text/7.xhtml"],"tar":"https://example.invalid/t.tar"}]}"#,
        )
        .expect("manifest");
        assert_eq!(manifest.chapters[0].files, vec!["Text/7.xhtml"]);
        let package = ChapterPackage {
            zip: vec![1],
            password: "secret".to_string(),
        };
        assert!(!format!("{package:?}").contains("secret"));
    }

    // §14.3: the TOC's `book.version` and `synckey` are the download fallbacks
    // when `getProgress.book.bookVersion` is absent.
    #[test]
    fn chapter_infos_keeps_the_book_version_fallbacks() {
        let direct: ChapterInfosResponse = serde_json::from_str(
            r#"{"book":{"version":42,"synckey":7},"chapters":[{"chapterUid":1}]}"#,
        )
        .expect("direct chapterInfos");
        let infos = direct.into_chapters("1");
        assert_eq!(infos.book_version, Some(42));
        assert_eq!(infos.synckey, Some(7));

        let wrapped: ChapterInfosResponse = serde_json::from_str(
            r#"{"book":{"version":40},"data":[{"bookId":"1","book":{"version":43},
                "synckey":8,"updated":[{"chapterUid":1}]}]}"#,
        )
        .expect("wrapped chapterInfos");
        let infos = wrapped.into_chapters("1");
        assert_eq!(infos.book_version, Some(43));
        assert_eq!(infos.synckey, Some(8));
    }
}
