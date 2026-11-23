//! WeRead (微信读书) E-Ink channel: login, shelf, progress, chapters and reading-time uploads.
//!
//! The wire contract is `docs/sync-contract.md` §14 (v1). HTTP is injected through
//! [`crate::sync::transport`], so this module has no network stack of its own and no global state.
//! Credentials never appear in `Debug` output, and fixtures use synthetic ids only.

use serde_json::Value;

pub mod arbiter;
pub mod auth;
pub mod client;
pub mod download;
pub mod epub;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod model;
pub mod notes;
pub mod position;
pub mod readtime;
pub mod web;

pub use arbiter::{arbitrate, FenceState, PositionStamp, SyncBaseline, Verdict, WriteFence};
pub use auth::{
    build_login_body, build_login_request, build_qrconnect_url, build_refresh_body,
    build_refresh_request, parse_login_response, parse_qr_ticket, parse_qrconnect_response,
    poll_status, version_headers, Credentials, LoginBody, QrPollStatus, QrTicket, RefreshBody,
    DEFAULT_DEVICE_NAME, DEFAULT_GUEST_TOKEN, DEVICE_TYPE_EINK, EINK_BASE, USER_AGENT, WX_APPID,
    WX_SCOPE,
};
pub use client::{
    build_batch_upload_body, decrypt_chapter_envelope, inner_signature, outer_signature,
    split_reading_hours, BatchUploadRequest, BookFormat, ChapterPackage, WeReadClient,
};
pub use download::{
    download_book, download_book_with_progress, plan_download, BookMapping, BookMappingStore,
    DownloadError, DownloadOutcome, DownloadPlan, DownloadRequest, MappingError, SkippedChapter,
    SourceFormat, BOOK_MAPPING_SCHEMA_VERSION,
};
pub use epub::{
    build_epub, extract_chapter_package, BuiltEpub, EpubChapter, EpubError, EpubSource,
    ExtractedPackage, SpineEntry, EPUB_MIMETYPE,
};
pub use model::{
    BatchUploadBody, BookChapters, BookProgressResponse, ChapterInfo, ChapterInfos,
    ChapterPackageManifest, ProgressBook, ReadingHour, ReadingInterval, ReadingPosition, ShelfBook,
    ShelfSync, TtsProgress,
};
pub use notes::{
    map_notes, parse_bookmark_list, parse_note_range, parse_review_list, BookNotes, BookmarkList,
    DegradeReason, MappedNote, MappingStats, NoteAnchor, NoteChapter, NoteKind, NoteRange,
    NotesMapping, ReviewList, WeReadBookmark, WeReadNote, WeReadReview,
};
pub use position::{
    AnchorMap, BookPositions, ChapterAnchors, ChapterMapping, EpubPosition, MappedCoordinate,
    MappedPosition, PositionError, PositionMap, Precision, TextAnchor, WeReadCoordinate,
    XPointerTarget, MAX_ANCHORLESS_CHAPTER_CHARS,
};
pub use readtime::{
    hours_for, intervals_for, InflightBatch, PauseReason, QueuedSpan, ReadtimeQueue, StaySpan,
    MAX_BATCH_SECS, MAX_SPAN_SECS, READTIME_QUEUE_VERSION, READ_TIME_ZONE,
};
pub use web::{
    enter_payload, parse_login_poll, parse_reader_state, payload_json, read_payload, reader_url,
    sign, sorted_query, summary_prefix, we, web_app_id, web_confirm_url, CookieJar, QueryValue,
    ReadPayloadInput, ReaderState, WeReadWebClient, WebLoginPoll, WebLoginSuccess, WebPayload,
    WebSession, DEFAULT_READER_TOKEN, READ_PATH, WEB_BASE, WEB_USER_AGENT,
};

/// Failures of the WeRead E-Ink channel.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WeReadError {
    #[error("not authenticated")]
    NotAuthenticated,
    #[error("authentication expired")]
    AuthExpired,
    #[error("Web sign-in expired; scan the code again")]
    WebAuthExpired,
    #[error("the Web write was not confirmed: {0}")]
    UnconfirmedWrite(String),
    #[error("the reader page has no progress state: {0}")]
    MissingReaderState(String),
    #[error("authentication rejected by WeRead (code {0})")]
    AuthRejected(i64),
    #[error("manual verification required (code {0}); finish it in the WeRead app and retry")]
    ManualVerification(i64),
    #[error("rate limited (code {0}); stop and cool down")]
    RateLimited(i64),
    #[error("WeRead API error {code}: {message}")]
    Api { code: i64, message: String },
    #[error("unexpected http status {0}: {1}")]
    HttpStatus(u16, String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
    #[error("chapter {chapter_uid} is neither free nor paid (price={price}, paid={paid})")]
    IneligibleChapter {
        chapter_uid: i64,
        price: f64,
        paid: i64,
    },
    #[error("chapter download response omitted the encryptkey header")]
    MissingEncryptKey,
    #[error("chapter envelope is invalid: {0}")]
    Envelope(String),
    #[error("upload was not confirmed by the server")]
    UnconfirmedUpload,
    #[error("endpoint not reachable from this device: {0}")]
    Blocked(String),
    #[error("response exceeded the {0}-byte limit")]
    TooLarge(u64),
}

impl From<crate::sync::transport::TransportError> for WeReadError {
    fn from(err: crate::sync::transport::TransportError) -> Self {
        use crate::sync::transport::TransportError;
        match err {
            TransportError::HttpStatus(status, body) => Self::HttpStatus(status, body),
            TransportError::Blocked(detail) => Self::Blocked(detail),
            TransportError::TooLarge(limit) => Self::TooLarge(limit),
            TransportError::Network(detail) => Self::Transport(detail),
            TransportError::Timeout => Self::Transport(TransportError::Timeout.to_string()),
            TransportError::Other(detail) => Self::Transport(detail),
        }
    }
}

/// Error code from a WeRead JSON error envelope (`errcode` or `errCode`).
pub(crate) fn api_error_code(body: &[u8]) -> Option<i64> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .get("errcode")
        .or_else(|| value.get("errCode"))
        .and_then(as_i64)
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn api_message(value: &Value) -> String {
    for key in ["errmsg", "errMsg", "msg", "message"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    String::new()
}

/// Map a WeRead error code to the contract §14.7 class and never auto-bypass it.
pub(crate) fn api_error(code: i64, message: impl Into<String>) -> WeReadError {
    match code {
        -2011 | -2041 => WeReadError::ManualVerification(code),
        -2012 => WeReadError::AuthRejected(code),
        -10102 | -2014 => WeReadError::RateLimited(code),
        other => WeReadError::Api {
            code: other,
            message: message.into(),
        },
    }
}

/// Turn a non-success HTTP response into a typed error, or `None` when it is usable.
pub(crate) fn response_error(status: u16, body: &[u8]) -> Option<WeReadError> {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        if let Some(code) = api_error_code(body) {
            if code != 0 {
                return Some(api_error(code, api_message(&value)));
            }
        }
    }
    if status == 401 {
        return Some(WeReadError::AuthExpired);
    }
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(body);
        let text: String = text.chars().take(200).collect();
        return Some(WeReadError::HttpStatus(status, text));
    }
    None
}
