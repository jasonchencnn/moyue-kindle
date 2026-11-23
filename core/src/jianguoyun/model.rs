//! Configuration, wire models and typed errors for the Jianguoyun WebDAV channel.
//!
//! The wire format is `docs/sync-contract.md` §15.4. Nothing here touches the network:
//! [`JianguoyunConfig`] only builds URLs and the `Authorization` header, and every record is a
//! plain value that round-trips through `serde_json`.

use std::fmt;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::sync::transport::TransportError;

/// Default Jianguoyun WebDAV endpoint (contract §15.1).
pub const DEFAULT_WEBDAV_BASE: &str = "https://dav.jianguoyun.com/dav/";
/// Directory that holds Moyue metadata at the WebDAV root (contract §15.4).
pub const SYNC_DIR_NAME: &str = ".moyue";
/// Sub-directory that holds the per-book sync files (contract §15.4).
pub const SYNC_SUBDIR: &str = "sync";
/// Schema version written into [`JianguoyunBookSync::version`].
pub const SYNC_SCHEMA_VERSION: u32 = 1;
/// Longest accepted `book_hash` segment, in bytes.
pub const MAX_BOOK_HASH_LEN: usize = 64;
/// Ebook extensions discovered by the directory browser (contract §15.2).
pub const EBOOK_EXTENSIONS: [&str; 8] =
    ["epub", "pdf", "txt", "mobi", "azw3", "fb2", "cbz", "djvu"];

/// Whether `extension` (with or without a leading dot, any case) is a supported ebook format.
pub fn is_ebook_extension(extension: &str) -> bool {
    let needle = extension.trim().trim_start_matches('.');
    !needle.is_empty()
        && EBOOK_EXTENSIONS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(needle))
}

/// Reject anything that is not a 1..=`MAX_BOOK_HASH_LEN` ASCII hexadecimal `book_hash`.
///
/// The contract stores one file per 32-character lowercase `partialMD5`; the wider bound keeps
/// shorter synthetic hashes usable while still refusing path separators, `..` and any non-hex
/// character.
pub fn validate_book_hash(book_hash: &str) -> Result<(), JianguoyunError> {
    if book_hash.is_empty() {
        return Err(JianguoyunError::InvalidHash(
            "book_hash must not be empty".to_string(),
        ));
    }
    if book_hash.len() > MAX_BOOK_HASH_LEN {
        return Err(JianguoyunError::InvalidHash(format!(
            "book_hash must be at most {MAX_BOOK_HASH_LEN} bytes, got {}",
            book_hash.len()
        )));
    }
    if !book_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(JianguoyunError::InvalidHash(format!(
            "book_hash must be ASCII hexadecimal, got `{book_hash}`"
        )));
    }
    Ok(())
}

/// Percent-decode a URL path without treating `+` as a space (that rule is query-only).
///
/// Invalid escapes are kept verbatim so a malformed server response still yields a usable name
/// instead of failing the whole listing.
pub(crate) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn append_url_segments(url: &mut String, raw: &str, encode: bool) -> Result<(), JianguoyunError> {
    for segment in raw.split('/') {
        let decoded = percent_decode(segment);
        if decoded.is_empty() || decoded == "." {
            continue;
        }
        if decoded == ".."
            || decoded.contains('/')
            || decoded.contains('\\')
            || segment.contains('\\')
        {
            return Err(JianguoyunError::InvalidPath(
                "path must not escape its parent directory".to_string(),
            ));
        }
        if !url.ends_with('/') {
            url.push('/');
        }
        if encode {
            url.push_str(&crate::cloud::url_encode(segment));
        } else {
            url.push_str(segment);
        }
    }
    Ok(())
}

/// Connection settings for one Jianguoyun WebDAV account (contract §15.1).
#[derive(Clone, PartialEq, Eq)]
pub struct JianguoyunConfig {
    /// WebDAV endpoint, e.g. [`DEFAULT_WEBDAV_BASE`]; normalized by
    /// [`Self::normalized_base_url`].
    pub server_url: String,
    /// Nutstore account e-mail.
    pub username: String,
    /// Third-party application password, never the account login password.
    pub password: String,
    /// Optional decoded sub-directory under the endpoint, e.g. `书库`.
    pub remote_dir: String,
}

impl JianguoyunConfig {
    /// Build a configuration from decoded parts; nothing is validated until it is used.
    pub fn new(
        server_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
        remote_dir: impl Into<String>,
    ) -> Self {
        Self {
            server_url: server_url.into(),
            username: username.into(),
            password: password.into(),
            remote_dir: remote_dir.into(),
        }
    }

    /// Configuration for the official endpoint without a remote sub-directory.
    pub fn official(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::new(DEFAULT_WEBDAV_BASE, username, password, "")
    }

    /// The endpoint as an absolute URL that always ends with `/`.
    ///
    /// The scheme and host are lowercased, empty path segments are collapsed, and the decoded
    /// [`Self::remote_dir`] is percent-encoded segment by segment. `..` segments, backslashes
    /// and embedded credentials are rejected instead of being resolved.
    pub fn normalized_base_url(&self) -> Result<String, JianguoyunError> {
        let raw = self.server_url.trim();
        let (scheme, rest) = raw.split_once("://").ok_or_else(|| {
            JianguoyunError::InvalidConfig(
                "server_url must start with http:// or https://".to_string(),
            )
        })?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(JianguoyunError::InvalidConfig(format!(
                "unsupported WebDAV scheme `{scheme}`"
            )));
        }
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        let authority = authority.trim();
        if authority.is_empty() {
            return Err(JianguoyunError::InvalidConfig(
                "server_url has no host".to_string(),
            ));
        }
        if authority.contains('@') {
            return Err(JianguoyunError::InvalidConfig(
                "server_url must not embed credentials; use username and password".to_string(),
            ));
        }
        let mut url = format!("{scheme}://{}", authority.to_ascii_lowercase());
        append_url_segments(&mut url, path, false)?;
        append_url_segments(&mut url, &self.remote_dir, true)?;
        url.push('/');
        Ok(url)
    }

    /// The `Authorization` header value for HTTP Basic authentication.
    ///
    /// The caller is responsible for never logging this value; [`fmt::Debug`] of
    /// [`JianguoyunConfig`] and [`crate::jianguoyun::JianguoyunClient`] redacts the password.
    pub fn basic_auth_header(&self) -> String {
        let credentials = format!("{}:{}", self.username, self.password);
        format!("Basic {}", BASE64_STANDARD.encode(credentials))
    }

    /// Join a decoded relative path onto the normalized endpoint.
    ///
    /// Each segment is percent-encoded so Chinese titles and spaces work on the server, `.` and
    /// empty segments are dropped, and `..` or backslashes yield
    /// [`JianguoyunError::InvalidPath`]. A trailing `/` in `rel_path` is preserved; an empty path
    /// returns the bare endpoint.
    pub fn join_remote_path(&self, rel_path: &str) -> Result<String, JianguoyunError> {
        let base = self.normalized_base_url()?;
        let trimmed = rel_path.trim();
        if trimmed.is_empty() || trimmed == "/" {
            return Ok(base);
        }
        let keep_trailing_slash = trimmed.ends_with('/');
        let mut url = base;
        append_url_segments(&mut url, trimmed, true)?;
        if keep_trailing_slash && !url.ends_with('/') {
            url.push('/');
        }
        Ok(url)
    }

    /// URL of the `.moyue/` collection (with the trailing slash WebDAV expects).
    pub fn sync_dir_url(&self) -> Result<String, JianguoyunError> {
        let base = self.normalized_base_url()?;
        Ok(format!("{base}{SYNC_DIR_NAME}/"))
    }

    /// URL of the `.moyue/sync/` collection (with the trailing slash WebDAV expects).
    pub fn sync_subdir_url(&self) -> Result<String, JianguoyunError> {
        let base = self.normalized_base_url()?;
        Ok(format!("{base}{SYNC_DIR_NAME}/{SYNC_SUBDIR}/"))
    }

    /// URL of the per-book sync file `<base>/.moyue/sync/<book_hash>.json`.
    ///
    /// `book_hash` must be 1..=[`MAX_BOOK_HASH_LEN`] ASCII hexadecimal characters and is
    /// normalized to lowercase; `/`, `..` and every non-hex byte are rejected with
    /// [`JianguoyunError::InvalidHash`].
    pub fn sync_file_path(&self, book_hash: &str) -> Result<String, JianguoyunError> {
        let hash = book_hash.trim();
        validate_book_hash(hash)?;
        let hash = hash.to_ascii_lowercase();
        let base = self.normalized_base_url()?;
        Ok(format!("{base}{SYNC_DIR_NAME}/{SYNC_SUBDIR}/{hash}.json"))
    }
}

impl Default for JianguoyunConfig {
    fn default() -> Self {
        Self::new(DEFAULT_WEBDAV_BASE, "", "", "")
    }
}

impl fmt::Debug for JianguoyunConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JianguoyunConfig")
            .field("server_url", &self.server_url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("remote_dir", &self.remote_dir)
            .finish()
    }
}

/// One entry of a `PROPFIND Depth: 1` listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// Percent-decoded absolute path on the server, without a trailing slash.
    pub path: String,
    /// Display name; falls back to the last path segment when the server omits `displayname`.
    pub name: String,
    /// Whether the entry is a WebDAV collection.
    pub is_dir: bool,
    /// `getcontentlength`, or `0` when the server does not report it.
    pub size: u64,
    /// `getetag` as returned by the server, if any.
    pub etag: Option<String>,
    /// `getlastmodified` as returned by the server, if any.
    pub last_modified: Option<String>,
    /// `getcontenttype` as returned by the server, if any.
    pub content_type: Option<String>,
    /// Lowercase file extension for ebook files, `None` for directories.
    pub format: Option<String>,
}

/// Per-book sync record stored at `<base>/.moyue/sync/<book_hash>.json` (contract §15.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JianguoyunBookSync {
    /// Schema version; [`SYNC_SCHEMA_VERSION`] for records written by this client.
    pub version: u32,
    /// Lowercase `partialMD5` of the book.
    pub book_hash: String,
    /// Book title used for display on other devices.
    #[serde(default)]
    pub title: String,
    /// Book author, may be empty.
    #[serde(default)]
    pub author: String,
    /// Path of the book inside the WebDAV tree, when the book lives there too.
    #[serde(default)]
    pub remote_path: String,
    /// Latest reading position, if the book was opened.
    #[serde(default)]
    pub progress: Option<JianguoyunProgress>,
    /// Notes, highlights and bookmarks, tombstones included.
    #[serde(default)]
    pub notes: Vec<JianguoyunNote>,
}

impl JianguoyunBookSync {
    /// A record with no progress and no notes.
    pub fn new(book_hash: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            version: SYNC_SCHEMA_VERSION,
            book_hash: book_hash.into(),
            title: title.into(),
            author: String::new(),
            remote_path: String::new(),
            progress: None,
            notes: Vec::new(),
        }
    }
}

/// Reading position of one book, keyed by `updated_at_ms` for last-writer-wins merges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JianguoyunProgress {
    /// CREngine XPointer, the primary anchor for reflowable books.
    #[serde(default)]
    pub xpointer: Option<String>,
    /// EPUB CFI, used when the XPointer is unavailable.
    #[serde(default)]
    pub cfi: Option<String>,
    /// Position in the book, `0.0`..=`1.0`.
    pub fraction: f64,
    /// One-based page number when the book is paginated.
    #[serde(default)]
    pub page: Option<u32>,
    /// Total page count that [`Self::page`] refers to.
    #[serde(default)]
    pub total_pages: Option<u32>,
    /// Human-readable chapter title, used by the "continue reading" UI.
    #[serde(default)]
    pub chapter_title: Option<String>,
    /// Milliseconds since the Unix epoch when this position was recorded.
    pub updated_at_ms: i64,
    /// Identifier of the device that wrote this position.
    #[serde(default)]
    pub device_id: String,
}

impl JianguoyunProgress {
    /// Whether this position outranks `other` under the contract §15.4 LWW rule.
    ///
    /// A newer `updated_at_ms` wins; on an exact tie the larger `fraction` wins. A full tie is
    /// not "newer", because the merge then keeps the remote record for determinism.
    pub fn is_newer_than(&self, other: &Self) -> bool {
        (self.updated_at_ms, self.fraction) > (other.updated_at_ms, other.fraction)
    }
}

/// Kind of a [`JianguoyunNote`] on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    /// A text highlight.
    Highlight,
    /// A bookmark without selected text.
    Bookmark,
    /// A free-form annotation attached to a position.
    #[default]
    Note,
}

impl NoteKind {
    /// The lowercase wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Highlight => "highlight",
            Self::Bookmark => "bookmark",
            Self::Note => "note",
        }
    }
}

impl<'de> Deserialize<'de> for NoteKind {
    /// Unknown kinds degrade to [`NoteKind::Note`] so a newer peer cannot break the pull.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "highlight" => Self::Highlight,
            "bookmark" => Self::Bookmark,
            _ => Self::Note,
        })
    }
}

/// A highlight, bookmark or annotation with a `deleted_at_ms` tombstone (contract §15.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JianguoyunNote {
    /// Stable note identifier shared by every device.
    pub id: String,
    /// What kind of note this is; a missing kind degrades to [`NoteKind::Note`].
    #[serde(default)]
    pub kind: NoteKind,
    /// Start anchor of a highlight.
    #[serde(default)]
    pub xpointer0: Option<String>,
    /// End anchor of a highlight.
    #[serde(default)]
    pub xpointer1: Option<String>,
    /// Selected text.
    #[serde(default)]
    pub text: Option<String>,
    /// User annotation attached to the note.
    #[serde(default)]
    pub note: Option<String>,
    /// Highlight colour name, e.g. `yellow`.
    #[serde(default)]
    pub color: Option<String>,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at_ms: i64,
    /// Last edit time in milliseconds since the Unix epoch.
    pub updated_at_ms: i64,
    /// Deletion marker in milliseconds since the Unix epoch; kept as a tombstone.
    #[serde(default)]
    pub deleted_at_ms: Option<i64>,
}

impl JianguoyunNote {
    /// A highlight with no annotation, positioned only by its two anchors.
    pub fn highlight(
        id: impl Into<String>,
        xpointer0: impl Into<String>,
        xpointer1: impl Into<String>,
        text: impl Into<String>,
        created_at_ms: i64,
    ) -> Self {
        Self {
            id: id.into(),
            kind: NoteKind::Highlight,
            xpointer0: Some(xpointer0.into()),
            xpointer1: Some(xpointer1.into()),
            text: Some(text.into()),
            note: None,
            color: None,
            created_at_ms,
            updated_at_ms: created_at_ms,
            deleted_at_ms: None,
        }
    }

    /// Whether this record is a deletion tombstone.
    pub fn is_deleted(&self) -> bool {
        self.deleted_at_ms.is_some()
    }

    /// Effective LWW timestamp: `max(updated_at_ms, deleted_at_ms.unwrap_or(0))`.
    pub fn effective_timestamp_ms(&self) -> i64 {
        self.updated_at_ms.max(self.deleted_at_ms.unwrap_or(0))
    }
}

/// Failures of the Jianguoyun channel, mapped to the statuses of contract §15.5.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JianguoyunError {
    /// `401`: the e-mail or application password was rejected.
    #[error("Jianguoyun rejected the credentials; check the e-mail and application password")]
    AuthFailed,
    /// `403`: WebDAV is disabled for the account or the path is refused.
    #[error("Jianguoyun refused WebDAV access; enable an application password in the web UI")]
    Forbidden,
    /// `404`: the remote collection or sync file does not exist.
    #[error("remote path not found")]
    NotFound,
    /// `409`: a `PUT`/`MKCOL` parent collection is missing.
    #[error("remote conflict: parent collection is missing")]
    Conflict,
    /// `429`/`503`: the request quota is exhausted; never retry blindly.
    #[error(
        "rate limited by Jianguoyun{}",
        match retry_after_secs {
            Some(seconds) => format!("; retry after {seconds}s"),
            None => String::new(),
        }
    )]
    RateLimited {
        /// Seconds from `Retry-After`, when the server sent a delta-seconds value.
        retry_after_secs: Option<u64>,
    },
    /// The configuration cannot produce a usable URL.
    #[error("invalid Jianguoyun configuration: {0}")]
    InvalidConfig(String),
    /// The `book_hash` is not a usable file stem.
    #[error("invalid book hash: {0}")]
    InvalidHash(String),
    /// A relative path tried to escape the endpoint.
    #[error("invalid remote path: {0}")]
    InvalidPath(String),
    /// The `207 Multi-Status` body is missing or malformed.
    #[error("malformed PROPFIND response: {0}")]
    XmlParse(String),
    /// A sync record is not valid JSON or does not match the requested book.
    #[error("malformed sync record: {0}")]
    JsonParse(String),
    /// The HTTP transport failed before a response was available.
    #[error("transport error: {0}")]
    Transport(String),
    /// Any other non-success HTTP status.
    #[error("unexpected http status {0}: {1}")]
    HttpStatus(u16, String),
}

impl JianguoyunError {
    /// Build the typed error for a non-success HTTP status (contract §15.5).
    ///
    /// The response body is truncated to 200 characters so a large error page cannot bloat logs.
    pub fn from_status(status: u16, body: &str, retry_after_secs: Option<u64>) -> Self {
        match status {
            401 => Self::AuthFailed,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            409 => Self::Conflict,
            429 | 503 => Self::RateLimited { retry_after_secs },
            other => Self::HttpStatus(other, truncate_body(body)),
        }
    }

    /// Stable classification for UI localization.
    pub fn kind(&self) -> JianguoyunErrorKind {
        match self {
            Self::AuthFailed => JianguoyunErrorKind::Authentication,
            Self::Forbidden => JianguoyunErrorKind::Permission,
            Self::NotFound => JianguoyunErrorKind::NotFound,
            Self::Conflict => JianguoyunErrorKind::Conflict,
            Self::RateLimited { .. } => JianguoyunErrorKind::RateLimited,
            Self::InvalidConfig(_) => JianguoyunErrorKind::Configuration,
            Self::InvalidHash(_) | Self::InvalidPath(_) => JianguoyunErrorKind::InvalidInput,
            Self::XmlParse(_) | Self::JsonParse(_) => JianguoyunErrorKind::Protocol,
            Self::Transport(_) => JianguoyunErrorKind::Network,
            Self::HttpStatus(status, _) if *status >= 500 => JianguoyunErrorKind::Server,
            Self::HttpStatus(_, _) => JianguoyunErrorKind::Protocol,
        }
    }
}

fn truncate_body(body: &str) -> String {
    body.trim().chars().take(200).collect()
}

impl From<TransportError> for JianguoyunError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::HttpStatus(status, body) => Self::from_status(status, &body, None),
            TransportError::Network(detail) => Self::Transport(detail),
            TransportError::Timeout => Self::Transport("request timed out".to_string()),
            TransportError::Blocked(detail) => Self::Transport(detail),
            TransportError::TooLarge(limit) => {
                Self::Transport(format!("response exceeded the {limit}-byte limit"))
            }
            TransportError::Other(detail) => Self::Transport(detail),
        }
    }
}

/// Coarse error class for the UI, stable across releases and safe to localize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JianguoyunErrorKind {
    /// Credentials were rejected (`401`).
    Authentication,
    /// WebDAV is disabled or the path is refused (`403`).
    Permission,
    /// The remote path does not exist (`404`).
    NotFound,
    /// A parent collection is missing (`409`).
    Conflict,
    /// The request quota is exhausted (`429`/`503`).
    RateLimited,
    /// The configuration cannot build a URL.
    Configuration,
    /// A caller-supplied hash or path is invalid.
    InvalidInput,
    /// The server returned a body that does not match the contract.
    Protocol,
    /// The transport failed.
    Network,
    /// A `5xx` server failure.
    Server,
}

impl JianguoyunErrorKind {
    /// Stable machine key for message lookup, e.g. `jianguoyun.error.rateLimited`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Permission => "permission",
            Self::NotFound => "notFound",
            Self::Conflict => "conflict",
            Self::RateLimited => "rateLimited",
            Self::Configuration => "configuration",
            Self::InvalidInput => "invalidInput",
            Self::Protocol => "protocol",
            Self::Network => "network",
            Self::Server => "server",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> JianguoyunConfig {
        JianguoyunConfig::new(
            DEFAULT_WEBDAV_BASE,
            "reader@example.com",
            "app-password",
            "",
        )
    }

    fn progress(fraction: f64, updated_at_ms: i64, device_id: &str) -> JianguoyunProgress {
        JianguoyunProgress {
            xpointer: None,
            cfi: None,
            fraction,
            page: None,
            total_pages: None,
            chapter_title: None,
            updated_at_ms,
            device_id: device_id.to_string(),
        }
    }

    #[test]
    fn config_debug_redacts_password() {
        let debug = format!("{:?}", config());
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("app-password"));
        assert!(debug.contains("reader@example.com"));
    }

    #[test]
    fn normalizes_scheme_host_and_slashes() {
        let mut config = config();
        config.server_url = "HTTPS://DAV.Jianguoyun.com//dav//".to_string();
        assert_eq!(
            config.normalized_base_url().unwrap(),
            "https://dav.jianguoyun.com/dav/"
        );

        config.server_url = "https://dav.jianguoyun.com".to_string();
        assert_eq!(
            config.normalized_base_url().unwrap(),
            "https://dav.jianguoyun.com/"
        );

        config.server_url = "https://dav.jianguoyun.com/".to_string();
        assert_eq!(
            config.normalized_base_url().unwrap(),
            "https://dav.jianguoyun.com/"
        );
    }

    #[test]
    fn remote_dir_is_percent_encoded_per_segment() {
        let mut config = config();
        config.remote_dir = "书库/我的 书".to_string();
        assert_eq!(
            config.normalized_base_url().unwrap(),
            "https://dav.jianguoyun.com/dav/%E4%B9%A6%E5%BA%93/%E6%88%91%E7%9A%84%20%E4%B9%A6/"
        );
    }

    #[test]
    fn rejects_unusable_endpoints() {
        let mut config = config();
        for url in [
            "",
            "dav.jianguoyun.com/dav",
            "ftp://dav.jianguoyun.com/dav/",
            "https:///dav/",
            "https://user:secret@dav.jianguoyun.com/dav/",
        ] {
            config.server_url = url.to_string();
            assert!(
                matches!(
                    config.normalized_base_url(),
                    Err(JianguoyunError::InvalidConfig(_))
                ),
                "`{url}` should be rejected"
            );
        }
    }

    #[test]
    fn rejects_path_traversal_in_endpoint_and_remote_dir() {
        let mut config = config();
        config.server_url = "https://dav.jianguoyun.com/dav/../etc".to_string();
        assert!(matches!(
            config.normalized_base_url(),
            Err(JianguoyunError::InvalidPath(_))
        ));

        config.server_url = DEFAULT_WEBDAV_BASE.to_string();
        config.remote_dir = "书库/..".to_string();
        assert!(matches!(
            config.normalized_base_url(),
            Err(JianguoyunError::InvalidPath(_))
        ));

        config.remote_dir = "书库\\..".to_string();
        assert!(matches!(
            config.normalized_base_url(),
            Err(JianguoyunError::InvalidPath(_))
        ));

        for encoded in [
            "https://dav.jianguoyun.com/dav/%2e%2e",
            "https://dav.jianguoyun.com/dav/%2e%2e%2f%2e%2e%2fetc",
            "https://dav.jianguoyun.com/dav/Books%2f..%2fetc",
        ] {
            config.server_url = encoded.to_string();
            config.remote_dir.clear();
            assert!(
                matches!(
                    config.normalized_base_url(),
                    Err(JianguoyunError::InvalidPath(_))
                ),
                "`{encoded}` should be rejected"
            );
        }
    }

    #[test]
    fn join_remote_path_encodes_and_preserves_shape() {
        let config = config();
        assert_eq!(
            config.join_remote_path("").unwrap(),
            "https://dav.jianguoyun.com/dav/"
        );
        assert_eq!(
            config.join_remote_path("书库").unwrap(),
            "https://dav.jianguoyun.com/dav/%E4%B9%A6%E5%BA%93"
        );
        assert_eq!(
            config.join_remote_path("书库/").unwrap(),
            "https://dav.jianguoyun.com/dav/%E4%B9%A6%E5%BA%93/"
        );
        assert_eq!(
            config.join_remote_path("Books/My Book.epub").unwrap(),
            "https://dav.jianguoyun.com/dav/Books/My%20Book.epub"
        );
        assert_eq!(
            config.join_remote_path("./Books//dune.epub").unwrap(),
            "https://dav.jianguoyun.com/dav/Books/dune.epub"
        );
    }

    #[test]
    fn join_remote_path_rejects_traversal() {
        let config = config();
        for path in ["..", "../etc", "Books/../../etc/passwd", "Books/..", "a\\b"] {
            assert!(
                matches!(
                    config.join_remote_path(path),
                    Err(JianguoyunError::InvalidPath(_))
                ),
                "`{path}` should be rejected"
            );
        }
    }

    #[test]
    fn sync_file_path_requires_hex_hash() {
        let config = config();
        assert_eq!(
            config
                .sync_file_path("0123456789abcdef0123456789abcdef")
                .unwrap(),
            "https://dav.jianguoyun.com/dav/.moyue/sync/0123456789abcdef0123456789abcdef.json"
        );
        assert!(config.sync_file_path("a").is_ok());
        assert!(config.sync_file_path(&"A".repeat(64)).is_ok());
        for hash in [
            "",
            &"a".repeat(65),
            "../../etc/passwd",
            "abc/def",
            "0123 4567",
            "zz",
            "-1",
        ] {
            assert!(
                matches!(
                    config.sync_file_path(hash),
                    Err(JianguoyunError::InvalidHash(_))
                ),
                "`{hash}` should be rejected"
            );
        }
    }

    #[test]
    fn sync_collection_urls_end_with_slash() {
        let config = config();
        assert_eq!(
            config.sync_dir_url().unwrap(),
            "https://dav.jianguoyun.com/dav/.moyue/"
        );
        assert_eq!(
            config.sync_subdir_url().unwrap(),
            "https://dav.jianguoyun.com/dav/.moyue/sync/"
        );
    }

    #[test]
    fn basic_auth_header_encodes_email_and_password() {
        let config = config();
        let header = config.basic_auth_header();
        let encoded = header.strip_prefix("Basic ").unwrap();
        let decoded = BASE64_STANDARD.decode(encoded).unwrap();
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            "reader@example.com:app-password"
        );
        assert!(!header.contains("app-password"));
    }

    #[test]
    fn book_sync_round_trips_contract_payload() {
        let payload = json!({
            "version": 1,
            "bookHash": "0123456789abcdef0123456789abcdef",
            "title": "三体",
            "author": "刘慈欣",
            "remotePath": "/Books/三体.epub",
            "progress": {
                "xpointer": "/body/DocFragment[5]/body/p[12]/text().34",
                "cfi": null,
                "fraction": 0.425,
                "page": 85,
                "totalPages": 200,
                "chapterTitle": "第五章",
                "updatedAtMs": 1_763_865_600_000i64,
                "deviceId": "KINDLE-OASIS-2"
            },
            "notes": [{
                "id": "note-uuid-1",
                "kind": "highlight",
                "xpointer0": "/body/DocFragment[5]/body/p[12]/text().0",
                "xpointer1": "/body/DocFragment[5]/body/p[12]/text().20",
                "text": "给岁月以文明",
                "note": "名句",
                "color": "yellow",
                "createdAtMs": 1_763_865_500_000i64,
                "updatedAtMs": 1_763_865_600_000i64,
                "deletedAtMs": null
            }]
        });
        let record: JianguoyunBookSync = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(record.version, SYNC_SCHEMA_VERSION);
        assert_eq!(record.book_hash, "0123456789abcdef0123456789abcdef");
        assert_eq!(record.title, "三体");
        assert_eq!(record.remote_path, "/Books/三体.epub");
        assert_eq!(
            record.progress.as_ref().unwrap().updated_at_ms,
            1_763_865_600_000
        );
        assert_eq!(record.notes[0].kind, NoteKind::Highlight);
        assert_eq!(record.notes[0].deleted_at_ms, None);

        let encoded = serde_json::to_value(&record).unwrap();
        assert_eq!(encoded["bookHash"], payload["bookHash"]);
        assert_eq!(encoded["remotePath"], payload["remotePath"]);
        assert_eq!(encoded["progress"]["totalPages"], json!(200));
        assert_eq!(encoded["notes"][0]["deletedAtMs"], json!(null));
    }

    #[test]
    fn unknown_note_kind_degrades_to_note() {
        let note: JianguoyunNote = serde_json::from_value(json!({
            "id": "n-1",
            "kind": "sticky",
            "createdAtMs": 1,
            "updatedAtMs": 2
        }))
        .unwrap();
        assert_eq!(note.kind, NoteKind::Note);
        assert_eq!(note.kind.as_str(), "note");

        let missing: JianguoyunNote = serde_json::from_value(json!({
            "id": "n-2",
            "createdAtMs": 1,
            "updatedAtMs": 2
        }))
        .unwrap();
        assert_eq!(missing.kind, NoteKind::Note);
        assert_eq!(note.effective_timestamp_ms(), 2);
        assert!(!note.is_deleted());
    }

    #[test]
    fn tombstone_effective_timestamp_uses_deleted_time() {
        let mut note = JianguoyunNote::highlight("n-1", "a", "b", "text", 10);
        note.deleted_at_ms = Some(40);
        assert_eq!(note.effective_timestamp_ms(), 40);
        assert!(note.is_deleted());
        note.updated_at_ms = 50;
        assert_eq!(note.effective_timestamp_ms(), 50);
    }

    #[test]
    fn progress_ordering_ties_by_fraction() {
        let mut older = progress(0.5, 1_000, "A");
        let mut newer = older.clone();
        newer.fraction = 0.7;
        assert!(newer.is_newer_than(&older));
        assert!(!older.is_newer_than(&newer));

        older.updated_at_ms = 2_000;
        assert!(older.is_newer_than(&newer));
        let same = older.clone();
        assert!(!same.is_newer_than(&older));
    }

    #[test]
    fn error_status_mapping_follows_contract() {
        assert_eq!(
            JianguoyunError::from_status(401, "", None),
            JianguoyunError::AuthFailed
        );
        assert_eq!(
            JianguoyunError::from_status(403, "", None),
            JianguoyunError::Forbidden
        );
        assert_eq!(
            JianguoyunError::from_status(404, "", None),
            JianguoyunError::NotFound
        );
        assert_eq!(
            JianguoyunError::from_status(409, "", None),
            JianguoyunError::Conflict
        );
        assert_eq!(
            JianguoyunError::from_status(429, "", Some(60)),
            JianguoyunError::RateLimited {
                retry_after_secs: Some(60)
            }
        );
        assert_eq!(
            JianguoyunError::from_status(503, "", None),
            JianguoyunError::RateLimited {
                retry_after_secs: None
            }
        );
        let other = JianguoyunError::from_status(500, &"x".repeat(300), None);
        match &other {
            JianguoyunError::HttpStatus(status, body) => {
                assert_eq!(*status, 500);
                assert_eq!(body.chars().count(), 200);
            }
            _ => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(other.kind(), JianguoyunErrorKind::Server);
        assert_eq!(other.kind().as_str(), "server");
        assert_eq!(
            JianguoyunError::AuthFailed.kind(),
            JianguoyunErrorKind::Authentication
        );
        assert_eq!(
            JianguoyunError::InvalidPath("x".to_string()).kind(),
            JianguoyunErrorKind::InvalidInput
        );
        assert_eq!(
            JianguoyunError::from_status(418, "", None).kind(),
            JianguoyunErrorKind::Protocol
        );
    }

    #[test]
    fn rate_limited_message_mentions_retry_delay() {
        let with_delay = JianguoyunError::RateLimited {
            retry_after_secs: Some(30),
        };
        assert!(with_delay.to_string().contains("30s"));
        let without = JianguoyunError::RateLimited {
            retry_after_secs: None,
        };
        assert_eq!(without.to_string(), "rate limited by Jianguoyun");
    }

    #[test]
    fn ebook_extension_filter_is_case_insensitive() {
        for extension in ["epub", ".EPUB", "Pdf", "azw3", "djvu"] {
            assert!(is_ebook_extension(extension), "`{extension}` should pass");
        }
        for extension in ["", ".", "md", "jpg", "azw"] {
            assert!(
                !is_ebook_extension(extension),
                "`{extension}` should be filtered"
            );
        }
    }

    #[test]
    fn percent_decode_handles_utf8_and_bad_escapes() {
        assert_eq!(
            percent_decode("/Books/%E4%B8%89%E4%BD%93.epub"),
            "/Books/三体.epub"
        );
        assert_eq!(percent_decode("/a%20b"), "/a b");
        assert_eq!(percent_decode("/a+b"), "/a+b");
        assert_eq!(percent_decode("/a%zz"), "/a%zz");
    }
}
