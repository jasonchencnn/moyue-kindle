//! Cloud library and book download ticket management (task SYNC-006).
//!
//! Provides platform-independent models and logic for:
//! - Deriving cloud storage keys and querying authoritative keys via `GET /api/storage/list?bookHash=...`.
//! - Mapping `DbBook` records into `CloudBook` entries, filtering deleted or un-uploaded books, and sorting by recency.
//! - Requesting signed download tickets from `GET /api/storage/download?fileKey=...`.
//! - Validating ticket expiration and automatically renewing tickets on download expiry (up to 2 retries).
//! - Validating downloaded file content against the expected `book_hash` (partial MD5),
//!   cleaning up corrupt temporary files automatically.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hash::partial_md5;
use crate::sync::client::{SyncClient, SyncError};
use crate::sync::records::DbBook;
use crate::sync::time::parse_iso8601;
use crate::sync::transport::{HttpRequest, HttpTransport, TransportError};

/// High-level errors occurring during cloud book listing and downloading.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("sync error: {0}")]
    Sync(#[from] SyncError),
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("storage http status {0}: {1}")]
    HttpStatus(u16, String),
    #[error("invalid response payload: {0}")]
    InvalidResponse(String),
    #[error("missing download url in response")]
    MissingDownloadUrl,
    #[error("missing user id for cloud storage operation")]
    MissingUserId,
    #[error("download ticket expired")]
    TicketExpired,
    #[error("hash verification mismatch: expected {expected}, actual {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Cloud storage backend provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StorageBackend {
    /// Cloudflare R2 object storage (Readest production default: `{user_id}/Readest/Books/{hash}/{title}.{ext}`).
    #[default]
    R2,
    /// Amazon S3 object storage (`{user_id}/Readest/Books/{hash}/{hash}.{ext}`).
    S3,
}

/// A book entry representing an item available in the Readest cloud library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudBook {
    /// File-level partial MD5 checksum (primary identifier).
    pub book_hash: String,
    /// Book title.
    pub title: String,
    /// Original file/source title before import if present (used for R2 filename derivation).
    pub source_title: Option<String>,
    /// Author name.
    pub author: String,
    /// File format string (e.g. "EPUB", "TXT", "MOBI", "PDF").
    pub format: String,
    /// Reading progress `[current_page, total_pages]` if available.
    pub progress: Option<(u32, u32)>,
    /// Cover image checksum if available.
    pub cover_hash: Option<String>,
    /// Cloud file upload timestamp (ISO-8601).
    pub uploaded_at: Option<String>,
    /// Deletion tombstone timestamp (ISO-8601) if marked deleted.
    pub deleted_at: Option<String>,
    /// Derived fallback cloud storage file key (used if list endpoint is unavailable).
    pub file_key: String,
    /// Whether the book already exists in the local library (matched by `book_hash`).
    pub downloaded: bool,
    /// Metadata fingerprint `md5(NFC(title|authors|identifiers))` for cross-file version detection.
    pub meta_hash: Option<String>,
    /// Last row update timestamp (ISO-8601).
    pub updated_at: Option<String>,
}

/// Signed download ticket for fetching a remote book binary.
///
/// Implements custom `Debug` to ensure signed URLs (which contain temporary
/// signature credentials and query parameters) are redacted from logs and debugging outputs.
///
/// Notice: `Serialize` and `Deserialize` are intentionally not derived to prevent
/// inadvertent leaking of the presigned URL when logging or serializing structs.
#[derive(Clone, PartialEq, Eq)]
pub struct DownloadTicket {
    /// Presigned download URL.
    pub url: String,
    /// Absolute expiration timestamp in Unix epoch milliseconds.
    pub expires_at: i64,
}

impl DownloadTicket {
    /// Default signed URL expiration duration in milliseconds (1800 seconds = 30 minutes, contract §9.2).
    pub const VALIDITY_DURATION_MS: i64 = 1800 * 1000;

    /// Default buffer margin for checking ticket expiration (60 seconds).
    pub const DEFAULT_MARGIN_MS: i64 = 60_000;

    /// Create a new `DownloadTicket`.
    pub fn new(url: impl Into<String>, expires_at: i64) -> Self {
        Self {
            url: url.into(),
            expires_at,
        }
    }

    /// Check if the ticket is expired or within `margin_ms` of expiring.
    pub fn is_expired(&self, now_ms: i64, margin_ms: i64) -> bool {
        now_ms.saturating_add(margin_ms) >= self.expires_at
    }
}

impl fmt::Debug for DownloadTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DownloadTicket")
            .field("url", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Sanitize filename matching Readest `makeSafeFilename` (`src/utils/misc.ts:11-33`).
///
/// - Replaces Windows restricted characters (`< > : % # " / \ | ? *`) and ASCII control characters (0x00..=0x1F) with `_`.
/// - Trims leading and trailing whitespace.
/// - Appends `_` if matching DOS reserved names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`..=`COM9`, `LPT1`..=`LPT9`).
/// - Truncates to at most 250 UTF-8 bytes without slicing through multi-byte UTF-8 character boundaries.
pub fn make_safe_filename(filename: &str) -> String {
    let mut safe = String::with_capacity(filename.len());
    for ch in filename.chars() {
        if matches!(
            ch,
            '<' | '>' | ':' | '%' | '#' | '"' | '/' | '\\' | '|' | '?' | '*'
        ) || (ch as u32) <= 0x1F
        {
            safe.push('_');
        } else {
            safe.push(ch);
        }
    }

    let mut trimmed = safe.trim().to_string();

    let is_reserved = {
        let upper = trimmed.to_ascii_uppercase();
        matches!(
            upper.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
    };

    if is_reserved {
        trimmed.push('_');
    }

    const MAX_BYTES: usize = 250;
    while trimmed.len() > MAX_BYTES {
        trimmed.pop();
    }

    trimmed
}

/// Map a book format name to its canonical file extension per Readest `EXTS` (`src/libs/document.ts:143-158`).
pub fn format_to_extension(format: &str) -> &'static str {
    match format.trim().to_ascii_uppercase().as_str() {
        "EPUB" => "epub",
        "PDF" => "pdf",
        "MOBI" => "mobi",
        "AZW" => "azw",
        "AZW3" => "azw3",
        "CBZ" => "cbz",
        "FB2" => "fb2",
        "FBZ" => "fbz",
        "TXT" => "txt",
        "MD" => "md",
        _ => "epub",
    }
}

/// Determine whether a file extension represents a supported book format.
pub fn is_book_file_extension(ext: &str) -> bool {
    matches!(
        ext.trim().to_ascii_lowercase().as_str(),
        "epub" | "pdf" | "mobi" | "azw" | "azw3" | "cbz" | "fb2" | "fbz" | "txt" | "md" | "zip"
    )
}

/// Fallback storage `file_key` derivation for a book when `/api/storage/list` is unavailable.
///
/// - For R2 (production default): `{user_id}/Readest/Books/{book_hash}/{makeSafeFilename(title)}.{ext}`
/// - For S3: `{user_id}/Readest/Books/{book_hash}/{book_hash}.{ext}`
pub fn derive_file_key(
    user_id: &str,
    book_hash: &str,
    title: &str,
    format: &str,
    backend: StorageBackend,
) -> String {
    let ext = format_to_extension(format);
    let filename = match backend {
        StorageBackend::R2 => {
            let safe = make_safe_filename(title);
            let safe_name = if safe.is_empty() { book_hash } else { &safe };
            format!("{safe_name}.{ext}")
        }
        StorageBackend::S3 => {
            format!("{book_hash}.{ext}")
        }
    };

    let user_id_clean = user_id.trim();
    if user_id_clean.is_empty() {
        format!("Readest/Books/{book_hash}/{filename}")
    } else {
        format!("{user_id_clean}/Readest/Books/{book_hash}/{filename}")
    }
}

/// Derive standard S3 storage `file_key` for a book: `{user_id}/Readest/Books/{hash}/{hash}.{ext}`.
pub fn derive_file_key_s3(user_id: &str, book_hash: &str, format: &str) -> String {
    derive_file_key(user_id, book_hash, "", format, StorageBackend::S3)
}

/// Derive standard R2 storage `file_key` for a book: `{user_id}/Readest/Books/{hash}/{safe_title}.{ext}`.
pub fn derive_file_key_r2(user_id: &str, book_hash: &str, title: &str, format: &str) -> String {
    derive_file_key(user_id, book_hash, title, format, StorageBackend::R2)
}

/// A file record entry returned by the storage listing endpoint (`GET /api/storage/list`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StorageFileRecord {
    pub file_key: String,
    pub file_size: u64,
    pub book_hash: Option<String>,
    pub replica_kind: Option<String>,
    pub replica_id: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ListFilesWireResponse {
    pub files: Vec<StorageFileRecord>,
    #[allow(dead_code)]
    pub total: usize,
    #[allow(dead_code)]
    pub page: usize,
    #[allow(dead_code)]
    #[serde(rename = "pageSize")]
    pub page_size: usize,
    #[serde(rename = "totalPages")]
    pub total_pages: usize,
}

/// Query the storage file listing for a given book hash (`GET /api/storage/list?bookHash=<hash>`).
///
/// Fetches file records across pages if necessary, returning all files associated with the book hash.
pub fn list_book_files<T: HttpTransport>(
    transport: &T,
    api_base: &str,
    token: &str,
    book_hash: &str,
) -> Result<Vec<StorageFileRecord>, CloudError> {
    let base = api_base.trim_end_matches('/');
    let encoded_hash = url_encode(book_hash);

    let mut all_files = Vec::new();
    let mut current_page = 1;
    let page_size = 100;

    loop {
        let url = format!(
            "{base}/storage/list?bookHash={encoded_hash}&page={current_page}&pageSize={page_size}"
        );
        let req = HttpRequest::new("GET", &url)
            .with_header("Authorization", format!("Bearer {token}"))
            .with_header("Accept", "application/json");

        let resp = transport.send(req)?;
        if resp.status != 200 {
            let msg = String::from_utf8_lossy(&resp.body).into_owned();
            return Err(CloudError::HttpStatus(resp.status, msg));
        }

        let wire: ListFilesWireResponse = serde_json::from_slice(&resp.body)
            .map_err(|e| CloudError::InvalidResponse(e.to_string()))?;

        all_files.extend(wire.files);

        if current_page >= wire.total_pages || wire.total_pages == 0 {
            break;
        }
        current_page += 1;
    }

    Ok(all_files)
}

/// Select the primary book binary record from a list of storage file records.
///
/// Excludes replicas (`replica_kind` present or `/Replicas/` in path), covers (`cover.png`),
/// and metadata sidecars (`nav.json`, `config.json`).
///
/// Prefers a record matching `preferred_format` if specified, otherwise the first record
/// having a supported book format extension.
pub fn select_book_file<'a>(
    files: &'a [StorageFileRecord],
    preferred_format: Option<&str>,
) -> Option<&'a StorageFileRecord> {
    let mut candidates: Vec<&StorageFileRecord> = files
        .iter()
        .filter(|f| {
            // Must not be a replica
            if let Some(kind) = &f.replica_kind {
                if !kind.trim().is_empty() {
                    return false;
                }
            }
            if let Some(rep_id) = &f.replica_id {
                if !rep_id.trim().is_empty() {
                    return false;
                }
            }
            if f.file_key.contains("/Replicas/") {
                return false;
            }

            // Exclude covers and sidecars
            let key_lower = f.file_key.to_ascii_lowercase();
            if key_lower.ends_with("/cover.png")
                || key_lower.ends_with("/cover.jpg")
                || key_lower.ends_with("/cover.jpeg")
                || key_lower.ends_with("/nav.json")
                || key_lower.ends_with("/config.json")
            {
                return false;
            }

            // Check file extension
            let ext = f.file_key.rsplit('.').next().unwrap_or("");
            is_book_file_extension(ext)
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    if let Some(pref) = preferred_format {
        let expected_ext = format_to_extension(pref);
        if let Some(&matched) = candidates.iter().find(|f| {
            let ext = f.file_key.rsplit('.').next().unwrap_or("");
            ext.eq_ignore_ascii_case(expected_ext)
        }) {
            return Some(matched);
        }
    }

    Some(candidates.remove(0))
}

/// Select the primary book binary `file_key` from a list of storage file records.
pub fn select_book_file_key<'a>(
    files: &'a [StorageFileRecord],
    preferred_format: Option<&str>,
) -> Option<&'a str> {
    select_book_file(files, preferred_format).map(|f| f.file_key.as_str())
}

/// Resolve the authoritative storage `file_key` for a book.
///
/// Queries `GET /api/storage/list?bookHash=<hash>` to fetch the actual `file_key` stored on the server.
/// If found, returns the server's real `file_key`.
/// If the list call fails or returns no book binary file, falls back to `derive_file_key`.
pub fn resolve_book_file_key<T: HttpTransport>(
    transport: &T,
    api_base: &str,
    token: &str,
    user_id: &str,
    book: &CloudBook,
    fallback_backend: StorageBackend,
) -> String {
    if let Ok(files) = list_book_files(transport, api_base, token, &book.book_hash) {
        if let Some(real_key) = select_book_file_key(&files, Some(&book.format)) {
            return real_key.to_string();
        }
    }

    let title_to_use = book.source_title.as_deref().unwrap_or(&book.title);
    derive_file_key(
        user_id,
        &book.book_hash,
        title_to_use,
        &book.format,
        fallback_backend,
    )
}

/// Filter out deleted and un-uploaded books, derive fallback `file_key`, mark `downloaded` status,
/// and sort by most recently read (newest `updated_at` first, ties broken by title and hash).
///
/// Returns `Err(CloudError::MissingUserId)` if any eligible book lacks a `user_id` and no
/// `default_user_id` is supplied.
pub fn filter_and_sort_cloud_books(
    books: &[DbBook],
    default_user_id: Option<&str>,
    local_hashes: &HashSet<String>,
) -> Result<Vec<CloudBook>, CloudError> {
    let mut cloud_books = Vec::new();
    // The local index and the server may differ in the case of a hex hash:
    // one spelling of the same book must not look like two.
    let local: HashSet<String> = local_hashes
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();

    for b in books {
        // Filter out dummy book (contract §1.4)
        if b.book_hash == "00000000000000000000000000000000" {
            continue;
        }
        // Filter out deleted books
        if let Some(del) = &b.deleted_at {
            if !del.trim().is_empty() {
                continue;
            }
        }
        // Filter out books without uploaded_at (file not uploaded to cloud storage)
        let has_uploaded = matches!(&b.uploaded_at, Some(up) if !up.trim().is_empty());
        if !has_uploaded {
            continue;
        }

        let uid = match b
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(u) => u,
            None => match default_user_id.map(str::trim).filter(|s| !s.is_empty()) {
                Some(u) => u,
                None => return Err(CloudError::MissingUserId),
            },
        };

        let title_source = b.source_title.as_deref().unwrap_or(&b.title);
        let fallback_file_key = derive_file_key(
            uid,
            &b.book_hash,
            title_source,
            &b.format,
            StorageBackend::R2,
        );
        let downloaded = local.contains(&b.book_hash.to_ascii_lowercase());

        cloud_books.push(CloudBook {
            book_hash: b.book_hash.clone(),
            title: b.title.clone(),
            source_title: b.source_title.clone(),
            author: b.author.clone(),
            format: b.format.clone(),
            progress: b.progress,
            cover_hash: b.cover_hash.clone(),
            uploaded_at: b.uploaded_at.clone(),
            deleted_at: b.deleted_at.clone(),
            file_key: fallback_file_key,
            downloaded,
            meta_hash: b.meta_hash.clone(),
            updated_at: b.updated_at.clone(),
        });
    }

    cloud_books.sort_by(|a, b| {
        let a_ms = a
            .updated_at
            .as_deref()
            .and_then(|ts| parse_iso8601(ts).ok());
        let b_ms = b
            .updated_at
            .as_deref()
            .and_then(|ts| parse_iso8601(ts).ok());
        match (a_ms, b_ms) {
            (Some(t_a), Some(t_b)) => t_b
                .cmp(&t_a)
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then_with(|| a.book_hash.cmp(&b.book_hash)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a
                .title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.book_hash.cmp(&b.book_hash)),
        }
    });

    Ok(cloud_books)
}

/// Fetch all cloud books using `SyncClient` cursor pagination with configurable page size.
pub fn fetch_cloud_books_paged<T: HttpTransport>(
    client: &SyncClient<T>,
    default_user_id: Option<&str>,
    local_hashes: &HashSet<String>,
    page_size: usize,
) -> Result<Vec<CloudBook>, CloudError> {
    let books = client.pull_all_books(0, page_size)?;
    filter_and_sort_cloud_books(&books, default_user_id, local_hashes)
}

/// Fetch all cloud books with an optional fallback `user_id`.
pub fn fetch_cloud_books_with_user<T: HttpTransport>(
    client: &SyncClient<T>,
    default_user_id: Option<&str>,
    local_hashes: &HashSet<String>,
) -> Result<Vec<CloudBook>, CloudError> {
    fetch_cloud_books_paged(client, default_user_id, local_hashes, 100)
}

/// Fetch all cloud books using `SyncClient` cursor pagination, filtering and sorting them.
pub fn fetch_cloud_books<T: HttpTransport>(
    client: &SyncClient<T>,
    local_hashes: &HashSet<String>,
) -> Result<Vec<CloudBook>, CloudError> {
    fetch_cloud_books_with_user(client, None, local_hashes)
}

/// URL percent-encode a string per RFC 3986 (safe for path and query parameters).
pub fn url_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{:02X}", b);
            }
        }
    }
    encoded
}

/// Request a signed download URL from `GET /api/storage/download?fileKey=...`.
///
/// Returns a `DownloadTicket` with an 1800-second validity window.
pub fn request_download_ticket<T: HttpTransport>(
    transport: &T,
    api_base: &str,
    token: &str,
    file_key: &str,
    now_ms: i64,
) -> Result<DownloadTicket, CloudError> {
    let encoded_key = url_encode(file_key);
    let base = api_base.trim_end_matches('/');
    let url = format!("{base}/storage/download?fileKey={encoded_key}");

    let req = HttpRequest::new("GET", &url)
        .with_header("Authorization", format!("Bearer {token}"))
        .with_header("Accept", "application/json");

    let resp = transport.send(req)?;

    if resp.status != 200 {
        let msg = String::from_utf8_lossy(&resp.body).into_owned();
        return Err(CloudError::HttpStatus(resp.status, msg));
    }

    #[derive(Deserialize)]
    struct WireResponse {
        #[serde(rename = "downloadUrl")]
        download_url: Option<String>,
    }

    let wire: WireResponse = serde_json::from_slice(&resp.body)
        .map_err(|e| CloudError::InvalidResponse(e.to_string()))?;

    let download_url = match wire.download_url {
        Some(u) if !u.trim().is_empty() => u,
        _ => return Err(CloudError::MissingDownloadUrl),
    };

    let expires_at = now_ms.saturating_add(DownloadTicket::VALIDITY_DURATION_MS);
    Ok(DownloadTicket::new(download_url, expires_at))
}

/// Ensure a valid download ticket is available.
///
/// If `current` ticket is Some and not expired (with `margin_ms` buffer), returns it.
/// Otherwise requests a fresh ticket via `request_download_ticket`.
pub fn ensure_valid_ticket<T: HttpTransport>(
    transport: &T,
    api_base: &str,
    token: &str,
    file_key: &str,
    current: Option<&DownloadTicket>,
    now_ms: i64,
    margin_ms: i64,
) -> Result<DownloadTicket, CloudError> {
    if let Some(ticket) = current {
        if !ticket.is_expired(now_ms, margin_ms) {
            return Ok(ticket.clone());
        }
    }
    request_download_ticket(transport, api_base, token, file_key, now_ms)
}

/// Verify that a downloaded temporary file matches the expected `book_hash` (partial MD5).
///
/// NOTE: The path provided must be a temporary file path created during downloading.
/// If hash calculation succeeds but the computed hash does not match `expected_book_hash`,
/// the temporary file at `path` is deleted immediately, and `CloudError::HashMismatch` is returned.
pub fn verify_download_hash(path: &Path, expected_book_hash: &str) -> Result<(), CloudError> {
    let mut file = File::open(path)?;

    let computed_hash = partial_md5(&mut file)?;
    drop(file);

    if !computed_hash.eq_ignore_ascii_case(expected_book_hash.trim()) {
        let _ = fs::remove_file(path);
        return Err(CloudError::HashMismatch {
            expected: expected_book_hash.to_string(),
            actual: computed_hash,
        });
    }

    Ok(())
}

/// Download binary bytes from `ticket.url` using `transport`, save to `target_path`,
/// and verify `partial_md5` against `expected_book_hash`.
///
/// NOTE: This helper buffers the entire response in memory via `HttpTransport::send`
/// and is intended for unit tests and small files. Part B (Aristotle app) must implement
/// a streaming downloader to prevent high memory consumption on Kindle hardware.
///
/// If download fails with ticket expiration (HTTP 410 or HTTP 403 indicating expiration),
/// returns `CloudError::TicketExpired`.
/// If verification fails, the temporary file is removed by `verify_download_hash`.
pub fn download_and_verify<T: HttpTransport>(
    transport: &T,
    ticket: &DownloadTicket,
    target_path: &Path,
    expected_book_hash: &str,
) -> Result<(), CloudError> {
    let req = HttpRequest::new("GET", &ticket.url);
    let resp = transport.send(req)?;

    if resp.status == 410 {
        return Err(CloudError::TicketExpired);
    }
    if resp.status == 403 {
        let msg = String::from_utf8_lossy(&resp.body);
        let lower = msg.to_ascii_lowercase();
        if lower.contains("expired") || lower.contains("requestexpired") || msg.trim().is_empty() {
            return Err(CloudError::TicketExpired);
        }
        return Err(CloudError::HttpStatus(403, msg.into_owned()));
    }
    if resp.status != 200 {
        let msg = String::from_utf8_lossy(&resp.body).into_owned();
        return Err(CloudError::HttpStatus(resp.status, msg));
    }

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(target_path, &resp.body)?;

    verify_download_hash(target_path, expected_book_hash)
}

/// Options and targets for downloading and verifying a cloud book binary.
#[derive(Debug, Clone)]
pub struct DownloadOptions<'a> {
    /// Readest API base URL (e.g. `https://web.readest.com/api`).
    pub api_base: &'a str,
    /// Bearer authentication token.
    pub token: &'a str,
    /// Remote storage file key.
    pub file_key: &'a str,
    /// Destination path for temporary file download.
    pub target_path: &'a Path,
    /// Expected partial MD5 checksum for content validation.
    pub expected_book_hash: &'a str,
}

impl<'a> DownloadOptions<'a> {
    /// Create a new `DownloadOptions`.
    pub fn new(
        api_base: &'a str,
        token: &'a str,
        file_key: &'a str,
        target_path: &'a Path,
        expected_book_hash: &'a str,
    ) -> Self {
        Self {
            api_base,
            token,
            file_key,
            target_path,
            expected_book_hash,
        }
    }
}

/// Download book content with automatic ticket retrieval, expiration renewal, and retries.
///
/// If `initial_ticket` is absent or expired (using a 60-second margin), requests a fresh ticket.
/// If downloading encounters a ticket expiration (`CloudError::TicketExpired`),
/// automatically requests a fresh ticket and retries downloading (up to 2 renewal attempts).
///
/// Validates `partial_md5` against `expected_book_hash` upon download completion.
/// Returns the valid/latest `DownloadTicket` used for the successful download.
///
/// NOTE: This helper buffers file bytes in memory and is intended for tests and small files;
/// Part B must implement streaming downloads.
pub fn download_with_ticket_renewal<T: HttpTransport, F: Fn() -> i64>(
    transport: &T,
    opts: &DownloadOptions<'_>,
    initial_ticket: Option<DownloadTicket>,
    now_ms_provider: F,
) -> Result<DownloadTicket, CloudError> {
    let mut ticket = match initial_ticket {
        Some(t) if !t.is_expired(now_ms_provider(), DownloadTicket::DEFAULT_MARGIN_MS) => t,
        _ => request_download_ticket(
            transport,
            opts.api_base,
            opts.token,
            opts.file_key,
            now_ms_provider(),
        )?,
    };

    let mut retries = 0;
    loop {
        match download_and_verify(
            transport,
            &ticket,
            opts.target_path,
            opts.expected_book_hash,
        ) {
            Ok(()) => return Ok(ticket),
            Err(CloudError::TicketExpired) if retries < 2 => {
                retries += 1;
                ticket = request_download_ticket(
                    transport,
                    opts.api_base,
                    opts.token,
                    opts.file_key,
                    now_ms_provider(),
                )?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Find local books that share the same `meta_hash` but have a different `book_hash`.
///
/// Returns the list of matching local `book_hash`es.
pub fn detect_meta_hash_matches<'a>(
    cloud_book: &CloudBook,
    local_books: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Vec<&'a str> {
    let target_meta = match cloud_book.meta_hash.as_deref() {
        Some(m) if !m.trim().is_empty() => m.trim(),
        _ => return Vec::new(),
    };

    let mut matches = Vec::new();
    for (local_hash, local_meta) in local_books {
        if local_hash != cloud_book.book_hash {
            if let Some(meta) = local_meta {
                if meta.trim() == target_meta {
                    matches.push(local_hash);
                }
            }
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::auth::{Session, SyncConfig};
    use crate::sync::mock::MockServer;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(1);

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let count = TEST_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("{prefix}_{}_{time}_{count}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn test_make_safe_filename() {
        // Reserved filenames
        assert_eq!(make_safe_filename("CON"), "CON_");
        assert_eq!(make_safe_filename("con"), "con_");
        assert_eq!(make_safe_filename("prn"), "prn_");
        assert_eq!(make_safe_filename("aux"), "aux_");
        assert_eq!(make_safe_filename("nul"), "nul_");
        assert_eq!(make_safe_filename("com1"), "com1_");
        assert_eq!(make_safe_filename("lpt9"), "lpt9_");

        // Restricted characters
        assert_eq!(make_safe_filename("file<name>.txt"), "file_name_.txt");
        assert_eq!(make_safe_filename("file:name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file*name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file?name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file\"name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file/name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file\\name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file|name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file%name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file#name.txt"), "file_name.txt");
        assert_eq!(make_safe_filename("file\x00name.txt"), "file_name.txt");

        // Whitespace trimming
        assert_eq!(make_safe_filename("  filename.txt  "), "filename.txt");

        // Truncate at 250 bytes
        let long_title = "a".repeat(300);
        let safe_long = make_safe_filename(&long_title);
        assert_eq!(safe_long.len(), 250);

        // UTF-8 multi-byte characters
        let chinese = "三体: 地球往事";
        assert_eq!(make_safe_filename(chinese), "三体_ 地球往事");
        let fullwidth = "三体：地球往事";
        assert_eq!(make_safe_filename(fullwidth), "三体：地球往事");
    }

    #[test]
    fn test_derive_file_key() {
        // R2 format (default): includes safe title
        assert_eq!(
            derive_file_key(
                "usr-01",
                "abc12345",
                "My Great Book",
                "EPUB",
                StorageBackend::R2
            ),
            "usr-01/Readest/Books/abc12345/My Great Book.epub"
        );
        // S3 format: uses book_hash as filename
        assert_eq!(
            derive_file_key(
                "usr-01",
                "abc12345",
                "My Great Book",
                "EPUB",
                StorageBackend::S3
            ),
            "usr-01/Readest/Books/abc12345/abc12345.epub"
        );
        // Empty title in R2 falls back to book_hash
        assert_eq!(
            derive_file_key("usr-01", "abc12345", "", "TXT", StorageBackend::R2),
            "usr-01/Readest/Books/abc12345/abc12345.txt"
        );
        // Empty user_id
        assert_eq!(
            derive_file_key("", "abc12345", "Test Book", "epub", StorageBackend::R2),
            "Readest/Books/abc12345/Test Book.epub"
        );
    }

    #[test]
    fn test_filter_and_sort_cloud_books() {
        let books = vec![
            DbBook {
                book_hash: "00000000000000000000000000000000".to_string(), // dummy book -> dropped
                title: "Dummy Book".to_string(),
                uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
            DbBook {
                book_hash: "hash-deleted".to_string(),
                title: "Deleted Book".to_string(),
                uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                deleted_at: Some("2026-11-06T12:00:00.000Z".to_string()), // deleted -> dropped
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
            DbBook {
                book_hash: "hash-unuploaded".to_string(),
                title: "Unuploaded Book".to_string(),
                uploaded_at: None, // unuploaded -> dropped
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
            DbBook {
                book_hash: "hash-old".to_string(),
                title: "Book Older".to_string(),
                source_title: Some("Older Original".to_string()),
                author: "Author A".to_string(),
                format: "EPUB".to_string(),
                uploaded_at: Some("2026-11-01T10:00:00.000Z".to_string()),
                updated_at: Some("2026-11-01T10:00:00.000Z".to_string()),
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
            DbBook {
                book_hash: "hash-new".to_string(),
                title: "Book Newer".to_string(),
                source_title: None,
                author: "Author B".to_string(),
                format: "EPUB".to_string(),
                uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                updated_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
            DbBook {
                book_hash: "hash-no-updated".to_string(),
                title: "Book Without Updated".to_string(),
                uploaded_at: Some("2026-11-01T00:00:00.000Z".to_string()),
                updated_at: None,
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
        ];

        let mut local_hashes = HashSet::new();
        local_hashes.insert("hash-old".to_string());

        let result = filter_and_sort_cloud_books(&books, Some("usr-01"), &local_hashes).unwrap();

        // Only hash-new, hash-old, and hash-no-updated pass
        assert_eq!(result.len(), 3);

        // Sort order: hash-new (2026-11-05) -> hash-old (2026-11-01) -> hash-no-updated (no timestamp)
        assert_eq!(result[0].book_hash, "hash-new");
        assert!(!result[0].downloaded);
        assert_eq!(
            result[0].file_key,
            "usr-01/Readest/Books/hash-new/Book Newer.epub"
        );
        assert_eq!(result[0].source_title, None);

        assert_eq!(result[1].book_hash, "hash-old");
        assert!(result[1].downloaded);
        assert_eq!(
            result[1].file_key,
            "usr-01/Readest/Books/hash-old/Older Original.epub"
        );
        assert_eq!(result[1].source_title, Some("Older Original".to_string()));

        assert_eq!(result[2].book_hash, "hash-no-updated");
        assert!(!result[2].downloaded);
    }

    #[test]
    fn local_hashes_match_server_hashes_ignoring_case() {
        let books = vec![DbBook {
            book_hash: "AABBCC".to_string(),
            title: "Book".to_string(),
            uploaded_at: Some("2026-11-01T00:00:00.000Z".to_string()),
            user_id: Some("usr-01".to_string()),
            ..Default::default()
        }];
        let local: HashSet<String> = ["aabbcc".to_string()].into_iter().collect();
        let result = filter_and_sort_cloud_books(&books, Some("usr-01"), &local).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].downloaded);
    }

    #[test]
    fn test_missing_user_id_returns_error() {
        let books = vec![DbBook {
            book_hash: "hash-1".to_string(),
            title: "Book 1".to_string(),
            uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            user_id: None,
            ..Default::default()
        }];

        // Missing user_id without fallback
        let err = filter_and_sort_cloud_books(&books, None, &HashSet::new()).unwrap_err();
        match err {
            CloudError::MissingUserId => (),
            other => panic!("expected MissingUserId, got {other:?}"),
        }

        // Empty user_id ("") is also treated as missing
        let books_empty = vec![DbBook {
            book_hash: "hash-1".to_string(),
            title: "Book 1".to_string(),
            uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            user_id: Some("   ".to_string()),
            ..Default::default()
        }];
        let err2 =
            filter_and_sort_cloud_books(&books_empty, Some("  "), &HashSet::new()).unwrap_err();
        match err2 {
            CloudError::MissingUserId => (),
            other => panic!("expected MissingUserId, got {other:?}"),
        }
    }

    #[test]
    fn test_sorting_ties_break_by_title_then_hash() {
        let books = vec![
            DbBook {
                book_hash: "hash-z".to_string(),
                title: "Beta Book".to_string(),
                uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                updated_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
            DbBook {
                book_hash: "hash-a".to_string(),
                title: "Alpha Book".to_string(),
                uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                updated_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
            DbBook {
                book_hash: "hash-b".to_string(),
                title: "Alpha Book".to_string(),
                uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                updated_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                user_id: Some("usr-01".to_string()),
                ..Default::default()
            },
        ];

        let result = filter_and_sort_cloud_books(&books, Some("usr-01"), &HashSet::new()).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].book_hash, "hash-a");
        assert_eq!(result[1].book_hash, "hash-b");
        assert_eq!(result[2].book_hash, "hash-z");
    }

    #[test]
    fn test_detect_meta_hash_matches() {
        let cloud_book = CloudBook {
            book_hash: "content-hash-v2".to_string(),
            title: "Pride and Prejudice".to_string(),
            source_title: None,
            author: "Jane Austen".to_string(),
            format: "EPUB".to_string(),
            progress: None,
            cover_hash: None,
            uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            deleted_at: None,
            file_key: "usr-01/Readest/Books/content-hash-v2/Pride and Prejudice.epub".to_string(),
            downloaded: false,
            meta_hash: Some("meta-jane-austen-pride".to_string()),
            updated_at: None,
        };

        let local_books = vec![
            ("content-hash-v1", Some("meta-jane-austen-pride")),
            ("different-book", Some("meta-different")),
            ("content-hash-v2", Some("meta-jane-austen-pride")), // same book_hash, not a conflict
        ];

        let matches = detect_meta_hash_matches(&cloud_book, local_books);
        assert_eq!(matches, vec!["content-hash-v1"]);
    }

    #[test]
    fn test_download_ticket_redacted_debug_and_http_request_redacted_debug() {
        let ticket = DownloadTicket::new(
            "https://r2.cloudflarestorage.com/signed?X-Amz-Signature=secret123&token=tkn",
            1_793_880_000_000,
        );
        let debug_str = format!("{ticket:?}");
        assert!(debug_str.contains("[REDACTED]"));
        assert!(!debug_str.contains("secret123"));
        assert!(!debug_str.contains("r2.cloudflarestorage.com"));

        let req = HttpRequest::new("GET", &ticket.url);
        let req_debug = format!("{req:?}");
        assert!(!req_debug.contains("secret123"));
        assert!(req_debug.contains("[QUERY_REDACTED]"));
    }

    #[test]
    fn test_download_ticket_expiration_check() {
        let ticket = DownloadTicket::new("https://example.com/download", 1_000_000);

        // Not expired
        assert!(!ticket.is_expired(900_000, 60_000));
        // Expired within margin
        assert!(ticket.is_expired(950_000, 60_000));
        // Expired past timestamp
        assert!(ticket.is_expired(1_000_001, 0));
    }

    #[test]
    fn test_hash_verification_success_and_failure() {
        let temp_dir = unique_temp_dir("cloud_hash_test");

        let valid_file = temp_dir.join("valid.epub");
        let sample_bytes = b"Sample EPUB content for test verification";
        fs::write(&valid_file, sample_bytes).unwrap();

        let mut cursor = Cursor::new(sample_bytes.to_vec());
        let expected_hash = partial_md5(&mut cursor).unwrap();

        // 1. Success check: file matches expected hash
        assert!(verify_download_hash(&valid_file, &expected_hash).is_ok());
        assert!(valid_file.exists()); // file is preserved

        // 2. Failure check: file does not match expected hash
        let corrupt_file = temp_dir.join("corrupt.epub");
        fs::write(&corrupt_file, b"Corrupted content").unwrap();

        let err = verify_download_hash(&corrupt_file, &expected_hash).unwrap_err();
        match err {
            CloudError::HashMismatch { expected, actual } => {
                assert_eq!(expected, expected_hash);
                assert_ne!(actual, expected_hash);
            }
            other => panic!("expected HashMismatch, got {other:?}"),
        }
        // Crucial requirement: corrupt temporary file MUST be deleted!
        assert!(!corrupt_file.exists());

        // Cleanup test directory
        let _ = fs::remove_file(&valid_file);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_list_book_files_and_select_book_file() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let user_id = "usr-01";
        let book_hash = "book-hash-999";

        // Insert primary book binary (R2 style filename)
        server.insert_file(
            user_id,
            "usr-01/Readest/Books/book-hash-999/My Great Novel.epub",
            b"Epub book content".to_vec(),
        );
        // Insert cover image
        server.insert_file(
            user_id,
            "usr-01/Readest/Books/book-hash-999/cover.png",
            b"Cover png bytes".to_vec(),
        );
        // Insert replica
        server.insert_file_with_metadata(
            user_id,
            "usr-01/Readest/Replicas/audio/rep-1/audio.m4a",
            b"Audio replica".to_vec(),
            Some(book_hash),
            Some("audio"),
            Some("rep-1"),
        );

        let files =
            list_book_files(&server, &config.api_base, "test-access-token", book_hash).unwrap();
        assert_eq!(files.len(), 3);

        let selected = select_book_file_key(&files, Some("EPUB"));
        assert_eq!(
            selected,
            Some("usr-01/Readest/Books/book-hash-999/My Great Novel.epub")
        );
    }

    #[test]
    fn test_resolve_book_file_key() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let user_id = "usr-01";
        let book_hash = "book-hash-resolve";

        let book = CloudBook {
            book_hash: book_hash.to_string(),
            title: "Resolve Title".to_string(),
            source_title: Some("Original Title".to_string()),
            author: "Tester".to_string(),
            format: "EPUB".to_string(),
            progress: None,
            cover_hash: None,
            uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            deleted_at: None,
            file_key: "fallback.epub".to_string(),
            downloaded: false,
            meta_hash: None,
            updated_at: None,
        };

        // 1. Without server listing: falls back to derive_file_key (R2 format)
        let resolved_fallback = resolve_book_file_key(
            &server,
            &config.api_base,
            "test-access-token",
            user_id,
            &book,
            StorageBackend::R2,
        );
        assert_eq!(
            resolved_fallback,
            "usr-01/Readest/Books/book-hash-resolve/Original Title.epub"
        );

        // 2. With server listing: returns authoritative key
        let authoritative_key = "usr-01/Readest/Books/book-hash-resolve/CustomName_ServerSide.epub";
        server.insert_file(user_id, authoritative_key, b"Content".to_vec());

        let resolved_auth = resolve_book_file_key(
            &server,
            &config.api_base,
            "test-access-token",
            user_id,
            &book,
            StorageBackend::R2,
        );
        assert_eq!(resolved_auth, authoritative_key);
    }

    #[test]
    fn test_download_with_ticket_renewal_on_expiration() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let user_id = "usr-01";

        let book_content = b"Content for automatic ticket renewal verification";
        let mut cursor = Cursor::new(book_content.to_vec());
        let expected_hash = partial_md5(&mut cursor).unwrap();

        let file_key = format!("usr-01/Readest/Books/{expected_hash}/Book.epub");
        server.insert_file(user_id, &file_key, book_content.to_vec());

        let t0 = server.clock_ms();
        // Request an initial ticket at t0
        let expired_ticket = request_download_ticket(
            &server,
            &config.api_base,
            "test-access-token",
            &file_key,
            t0,
        )
        .unwrap();

        // Advance clock past expiration (1801 seconds)
        server.advance_clock_ms(1801 * 1000);

        let temp_dir = unique_temp_dir("cloud_renew_test");
        let dest_file = temp_dir.join("renewed.epub");

        let server_clone = server.clone();
        // Download with the expired ticket: download_with_ticket_renewal should catch 403,
        // request a new ticket automatically, and complete download successfully
        let opts = DownloadOptions::new(
            &config.api_base,
            "test-access-token",
            &file_key,
            &dest_file,
            &expected_hash,
        );
        let final_ticket =
            download_with_ticket_renewal(&server, &opts, Some(expired_ticket.clone()), move || {
                server_clone.clock_ms()
            })
            .unwrap();

        assert_ne!(final_ticket.url, expired_ticket.url);
        assert!(dest_file.exists());
        assert_eq!(fs::read(&dest_file).unwrap(), book_content);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_paged_cloud_books_with_same_millisecond_across_pages() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let server_clock = server.clone();
        let client = SyncClient::new(server.clone(), config)
            .with_time_provider(move || server_clock.clock_ms());
        client.set_session(Some(Session::new(
            "test-access-token",
            "test-refresh-token",
            server.clock_ms() + 3_600_000,
        )));

        // Insert 5 books: Book 2 and Book 3 have identical synced_at timestamp
        let timestamps = [
            "2026-11-05T12:00:01.000Z",
            "2026-11-05T12:00:02.000Z",
            "2026-11-05T12:00:02.000Z", // same millisecond!
            "2026-11-05T12:00:03.000Z",
            "2026-11-05T12:00:04.000Z",
        ];

        for (idx, &ts) in timestamps.iter().enumerate() {
            let i = idx + 1;
            let content = format!("Content {i}").into_bytes();
            let mut cursor = Cursor::new(content.clone());
            let hash = partial_md5(&mut cursor).unwrap();
            let book = DbBook {
                book_hash: hash.clone(),
                title: format!("Paged Book {i}"),
                format: "EPUB".to_string(),
                user_id: Some("usr-01".to_string()),
                synced_at: Some(ts.to_string()),
                updated_at: Some(ts.to_string()),
                uploaded_at: Some(ts.to_string()),
                ..Default::default()
            };
            let file_key = format!("usr-01/Readest/Books/{hash}/Paged Book {i}.epub");
            server.insert_book("usr-01", book);
            server.insert_file("usr-01", &file_key, content);
        }

        // Fetch with page size of 2 -> requires multiple page roundtrips and same-millisecond handling
        let local_hashes = HashSet::new();
        let cloud_books =
            fetch_cloud_books_paged(&client, Some("usr-01"), &local_hashes, 2).unwrap();

        assert_eq!(cloud_books.len(), 5);
        // Newest updated first
        assert_eq!(cloud_books[0].title, "Paged Book 5");
        assert_eq!(cloud_books[1].title, "Paged Book 4");
    }

    #[test]
    fn test_mock_r2_download_flow() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let user_id = "usr-01";

        let book_content = b"Mock R2 storage test binary book data";
        let mut cursor = Cursor::new(book_content.to_vec());
        let expected_hash = partial_md5(&mut cursor).unwrap();

        // Use upstream R2 key format: {user_id}/Readest/Books/{hash}/{safe_title}.epub
        let title = "Readest R2 Guide & Manual";
        let safe_title = make_safe_filename(title);
        let r2_file_key = format!("usr-01/Readest/Books/{expected_hash}/{safe_title}.epub");

        let book = DbBook {
            book_hash: expected_hash.clone(),
            title: title.to_string(),
            source_title: None,
            format: "EPUB".to_string(),
            user_id: Some(user_id.to_string()),
            uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            updated_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            ..Default::default()
        };

        // Insert explicitly into server
        server.insert_book(user_id, book);
        server.insert_file(user_id, &r2_file_key, book_content.to_vec());

        // 1. Query server file list for book hash
        let files = list_book_files(
            &server,
            &config.api_base,
            "test-access-token",
            &expected_hash,
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_key, r2_file_key);

        let selected_key = select_book_file_key(&files, Some("EPUB")).unwrap();
        assert_eq!(selected_key, r2_file_key);

        // 2. Download and verify
        let temp_dir = unique_temp_dir("cloud_r2_test");
        let dest_file = temp_dir.join("r2_downloaded.epub");

        let server_clone = server.clone();
        let opts = DownloadOptions::new(
            &config.api_base,
            "test-access-token",
            selected_key,
            &dest_file,
            &expected_hash,
        );
        download_with_ticket_renewal(&server, &opts, None, move || server_clone.clock_ms())
            .unwrap();

        assert!(dest_file.exists());
        assert_eq!(fs::read(&dest_file).unwrap(), book_content);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_mock_s3_download_flow() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let user_id = "usr-01";

        let book_content = b"Mock S3 storage test binary book data";
        let mut cursor = Cursor::new(book_content.to_vec());
        let expected_hash = partial_md5(&mut cursor).unwrap();

        // S3 key format: {user_id}/Readest/Books/{hash}/{hash}.epub
        let s3_file_key = format!("usr-01/Readest/Books/{expected_hash}/{expected_hash}.epub");

        let book = DbBook {
            book_hash: expected_hash.clone(),
            title: "S3 Architecture Book".to_string(),
            format: "EPUB".to_string(),
            user_id: Some(user_id.to_string()),
            uploaded_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            updated_at: Some("2026-11-05T12:00:00.000Z".to_string()),
            ..Default::default()
        };

        server.insert_book(user_id, book);
        server.insert_file(user_id, &s3_file_key, book_content.to_vec());

        let files = list_book_files(
            &server,
            &config.api_base,
            "test-access-token",
            &expected_hash,
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_key, s3_file_key);

        let selected_key = select_book_file_key(&files, Some("EPUB")).unwrap();
        assert_eq!(selected_key, s3_file_key);

        let temp_dir = unique_temp_dir("cloud_s3_test");
        let dest_file = temp_dir.join("s3_downloaded.epub");

        let server_clone = server.clone();
        let opts = DownloadOptions::new(
            &config.api_base,
            "test-access-token",
            selected_key,
            &dest_file,
            &expected_hash,
        );
        download_with_ticket_renewal(&server, &opts, None, move || server_clone.clock_ms())
            .unwrap();

        assert!(dest_file.exists());
        assert_eq!(fs::read(&dest_file).unwrap(), book_content);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_download_and_verify_hash_mismatch_cleans_up_file() {
        let server = MockServer::new();
        let config = SyncConfig::default();

        let genuine_content = b"Server file content";
        let fake_hash = "11112222333344445555666677778888"; // Does not match content

        let file_key = format!("usr-01/Readest/Books/{fake_hash}/Corrupt.epub");
        server.insert_file("usr-01", &file_key, genuine_content.to_vec());

        let ticket = request_download_ticket(
            &server,
            &config.api_base,
            "test-access-token",
            &file_key,
            server.clock_ms(),
        )
        .unwrap();

        let temp_dir = unique_temp_dir("cloud_mismatch_test");
        let dest_file = temp_dir.join("corrupt.epub");

        let err = download_and_verify(&server, &ticket, &dest_file, fake_hash).unwrap_err();
        match err {
            CloudError::HashMismatch { expected, actual } => {
                assert_eq!(expected, fake_hash);
                assert_ne!(actual, fake_hash);
            }
            other => panic!("expected HashMismatch, got {other:?}"),
        }

        // Temp file must be removed!
        assert!(!dest_file.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_txt_book_download_representation() {
        assert_eq!(format_to_extension("TXT"), "txt");
        assert_eq!(format_to_extension("txt"), "txt");
        assert_eq!(format_to_extension("EPUB"), "epub");
        assert_eq!(format_to_extension("FB2"), "fb2");
        assert_eq!(format_to_extension("FBZ"), "fbz");
    }
}
