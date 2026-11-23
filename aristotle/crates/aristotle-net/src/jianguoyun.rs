// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Jianguoyun (Nutstore) WebDAV channel: the network transport the
//! platform-independent client in `inkread_core::jianguoyun` runs on, and the
//! streaming download of a book into the library.
//!
//! `core` builds the URL, the `Authorization` header and the JSON bodies; this
//! module gives them a socket. [`JianguoyunTransport`] implements
//! `inkread_core::sync::HttpTransport` over [`crate::http::Client`], so every
//! request still passes the device's allowlist and the size limit, and a
//! transient failure is retried only where replaying cannot hurt: reads
//! (`PROPFIND`, `GET`, `HEAD`) after any connection-level failure, writes
//! (`PUT`, `MKCOL`, `DELETE`) only when the request never left the device.
//!
//! [`download_book`] streams one `RemoteEntry` into a hidden `.part` file next
//! to its destination, checks that the server's path stays under the configured
//! endpoint, hashes the finished file with the KOReader `partialMD5`, and
//! renames it into place under a name no other book has. A cancel or any
//! failure removes the partial file: a half-written book never reaches the
//! library, and no `.part` file is left behind.

use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use inkread_core::hash::partial_md5;
use inkread_core::jianguoyun::{
    is_ebook_extension, JianguoyunConfig, JianguoyunError, RemoteEntry, USER_AGENT,
};
use inkread_core::sync::{HttpRequest, HttpResponse, HttpTransport, TransportError};

use crate::download::unique_path;
use crate::http::{Client, Error as HttpError, Method, Request, Timeouts};
use crate::policy::Policy;
use crate::url::Url;

/// Body limit for a WebDAV answer: a `PROPFIND` listing or a sync record.
pub const MAX_JIANGUOYUN_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Connect/TLS handshake timeout: fail fast on a stalled edge so the retry can
/// dial again instead of eating the whole response budget.
const CONNECT_TIMEOUT_SECS: u64 = 8;

/// Response timeout for a WebDAV call, which may list a large folder.
const RESPONSE_TIMEOUT_SECS: u64 = 30;

/// Body timeout for a WebDAV call.
const BODY_TIMEOUT_SECS: u64 = 60;

/// Body timeout for a book download: long enough for a slow free-tier link.
const DOWNLOAD_BODY_TIMEOUT_SECS: u64 = 300;

/// Maximum retries after an initial transient failure (3 attempts total).
const MAX_RETRIES: usize = 2;

/// Delay between retry attempts.
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// The largest book [`download_book`] accepts; the same limit as every other
/// channel ([`crate::download::MAX_BOOK`]).
pub const MAX_DOWNLOAD: u64 = crate::download::MAX_BOOK;

/// A WebDAV transport for one Jianguoyun account.
///
/// Pages share one instance per device policy through [`Client::shared`], so a
/// shelf refresh and a progress push reuse the same keep-alive connection and
/// TLS session.
pub struct JianguoyunTransport {
    client: Arc<Client>,
    max_body_bytes: u64,
}

impl JianguoyunTransport {
    /// A transport with the channel's default timeouts and body limit.
    pub fn new(policy: Policy) -> Self {
        Self::with_timeout(policy, RESPONSE_TIMEOUT_SECS)
    }

    /// A transport with an explicit response/body timeout, for a short push or
    /// a generous first listing.
    pub fn with_timeout(policy: Policy, secs: u64) -> Self {
        Self::with_timeout_and_limit(policy, secs, MAX_JIANGUOYUN_BODY_BYTES)
    }

    /// A transport with an explicit timeout and response body size limit.
    pub fn with_timeout_and_limit(policy: Policy, secs: u64, max_body_bytes: u64) -> Self {
        let timeouts = Timeouts {
            connect: Duration::from_secs(secs.min(CONNECT_TIMEOUT_SECS)),
            response: Duration::from_secs(secs),
            body: Duration::from_secs(secs.max(BODY_TIMEOUT_SECS)),
        };
        Self {
            client: Client::shared(policy, timeouts),
            max_body_bytes,
        }
    }

    /// A transport around a client the caller already owns, for tests and for
    /// a caller that keeps its own connection pool.
    pub fn with_client(client: Client) -> Self {
        Self {
            client: Arc::new(client),
            max_body_bytes: MAX_JIANGUOYUN_BODY_BYTES,
        }
    }

    /// The response body limit this transport enforces.
    pub fn max_body_bytes(&self) -> u64 {
        self.max_body_bytes
    }
}

impl fmt::Debug for JianguoyunTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JianguoyunTransport")
            .field("max_body_bytes", &self.max_body_bytes)
            .finish_non_exhaustive()
    }
}

/// Turn an [`HttpError`] into the platform-independent [`TransportError`].
///
/// `NotAllowed` and `NoTls` are [`TransportError::Blocked`]: the request never
/// left the device, so the UI must not report a network failure. Everything
/// else keeps its class so `inkread_core::jianguoyun` can map it in turn
/// (`AuthFailed`, `Forbidden`, `RateLimited`, `Transport`, ...).
pub fn map_http_error(error: HttpError) -> TransportError {
    match error {
        HttpError::Timeout => TransportError::Timeout,
        HttpError::Connect(detail) => TransportError::Network(detail),
        HttpError::NotAllowed(host) => {
            TransportError::Blocked(format!("{host} is not in the allowed hosts"))
        }
        HttpError::NoTls => TransportError::Blocked("HTTPS is not supported in this build".into()),
        HttpError::Tls { detail, .. } => TransportError::Network(format!("TLS error: {detail}")),
        HttpError::Status(status, body) => TransportError::HttpStatus(status, body),
        HttpError::TooLarge(limit) => TransportError::TooLarge(limit),
        HttpError::Io(io) => TransportError::Network(io.to_string()),
        HttpError::Cancelled => TransportError::Other("cancelled".into()),
        other => TransportError::Other(other.to_string()),
    }
}

impl HttpTransport for JianguoyunTransport {
    fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let url = Url::parse(&req.url)
            .map_err(|e| TransportError::Network(format!("invalid URL: {e}")))?;
        // The contract allows plain HTTP only on loopback and private IPv4
        // (`sync-contract.md` §15.1). HTTPS is always allowed, so the official
        // endpoint and any self-hosted TLS server keep working.
        if !crate::endpoint::is_allowed(&url) {
            return Err(TransportError::Blocked(format!(
                "{} is not an allowed Jianguoyun endpoint",
                url.host_port()
            )));
        }
        let method = match req.method.to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PROPFIND" => Method::Propfind,
            "MKCOL" => Method::Mkcol,
            "HEAD" => Method::Head,
            "DELETE" => Method::Delete,
            other => {
                return Err(TransportError::Other(format!(
                    "unsupported HTTP method {other}"
                )))
            }
        };
        let http_req = Request {
            method,
            url,
            headers: req.headers,
            body: if req.body.is_empty() {
                None
            } else {
                Some(req.body)
            },
            credentials: None,
        };

        let mut last_error = None;
        for attempt in 0..=MAX_RETRIES {
            match self.client.send(&http_req, self.max_body_bytes) {
                Ok(response) => {
                    return Ok(HttpResponse {
                        status: response.status,
                        headers: response.headers,
                        body: response.body,
                    })
                }
                Err(error) => {
                    let retryable = if method.is_idempotent_read() {
                        error.is_idempotent_retryable()
                    } else {
                        error.is_pre_send_retryable()
                    };
                    if retryable && attempt < MAX_RETRIES {
                        std::thread::sleep(RETRY_DELAY);
                        continue;
                    }
                    last_error = Some(error);
                    break;
                }
            }
        }
        Err(map_http_error(
            last_error.expect("retry loop exits with Ok or an error"),
        ))
    }
}

/// What a finished [`download_book`] wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadedBook {
    /// The final file in the library folder.
    pub path: PathBuf,
    /// KOReader `partialMD5` of the bytes, the book's hash on every channel.
    pub book_hash: String,
    /// Bytes written.
    pub bytes: u64,
}

/// A failed [`download_book`].
#[derive(Debug)]
pub enum DownloadError {
    /// The configuration cannot produce a usable base URL.
    Config(JianguoyunError),
    /// The endpoint is plain HTTP outside loopback and the private IPv4
    /// ranges, so the credentials would be readable on the network.
    InsecureUrl,
    /// The remote path is relative, does not live under the endpoint, or tries
    /// to leave it with `..`.
    UnsafePath,
    /// The entry is a directory, not a book.
    NotAFile,
    /// The entry is not one of the supported ebook formats.
    UnsupportedFormat,
    /// The server answered with a status or a transport failure.
    Http(HttpError),
    /// The file system refused to write the file.
    Io(std::io::Error),
    /// The server sent a zero-byte body.
    Empty,
    /// The caller cancelled; the `.part` file is already removed.
    Cancelled,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::Config(e) => e.fmt(f),
            DownloadError::InsecureUrl => f.write_str("the Jianguoyun endpoint is not secure"),
            DownloadError::UnsafePath => f.write_str("the remote path is not inside the endpoint"),
            DownloadError::NotAFile => f.write_str("the entry is a directory, not a book"),
            DownloadError::UnsupportedFormat => f.write_str("unsupported book format"),
            DownloadError::Http(e) => e.fmt(f),
            DownloadError::Io(e) => e.fmt(f),
            DownloadError::Empty => f.write_str("the server sent an empty book"),
            DownloadError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<JianguoyunError> for DownloadError {
    fn from(e: JianguoyunError) -> Self {
        DownloadError::Config(e)
    }
}

impl From<HttpError> for DownloadError {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Cancelled => DownloadError::Cancelled,
            other => DownloadError::Http(other),
        }
    }
}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self {
        DownloadError::Io(e)
    }
}

/// Stream `entry` into `dir`, hash it, and rename it into place.
///
/// The bytes go to `<dir>/.<stem>.<ext>.part` never into memory; `progress`
/// receives `(bytes so far, total if known)`. `limit` caps the body
/// ([`MAX_DOWNLOAD`] by default). When `cancel` turns true the partial file is
/// removed and [`DownloadError::Cancelled`] returned. The returned
/// [`DownloadedBook::book_hash`] is the KOReader `partialMD5` of the final
/// bytes, so the book can be admitted to the library by the same hash the
/// Readest and WeRead channels use.
///
/// The endpoint must pass [`crate::endpoint::is_allowed`], and `entry.path`
/// must be an absolute server path under the configured endpoint; `..`,
/// backslashes and anything else that could leave the folder are refused
/// before a byte is requested.
#[allow(clippy::too_many_arguments)]
pub fn download_book(
    policy: Policy,
    config: &JianguoyunConfig,
    entry: &RemoteEntry,
    dir: &Path,
    limit: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<DownloadedBook, DownloadError> {
    let url = download_url(config, entry)?;
    if !crate::endpoint::is_allowed(&url) {
        return Err(DownloadError::InsecureUrl);
    }
    let ext = entry_extension(entry)?;
    let stem = crate::download::safe_name(&book_stem(&entry.name, &ext), 120, "jianguoyun-book");

    fs::create_dir_all(dir)?;
    let part = dir.join(format!(".{stem}.{ext}.part"));
    let client = Client::shared(
        policy,
        Timeouts {
            connect: Duration::from_secs(CONNECT_TIMEOUT_SECS),
            response: Duration::from_secs(RESPONSE_TIMEOUT_SECS),
            body: Duration::from_secs(DOWNLOAD_BODY_TIMEOUT_SECS),
        },
    );

    let result = (|| -> Result<DownloadedBook, DownloadError> {
        let request = Request::get(url)
            .header("Authorization", &config.basic_auth_header())
            .header("User-Agent", USER_AGENT);
        let mut file = File::create(&part)?;
        let outcome = client.download(&request, &mut file, limit, cancel, progress);
        let written = match outcome {
            Ok(info) => {
                file.flush()?;
                file.sync_all()?;
                Ok(info)
            }
            Err(e) => Err(e),
        };
        drop(file);
        let info = written?;
        // A redirect to a public plain-HTTP host would hand the (unauthenticated)
        // bytes to the library over a readable link. The header is already
        // dropped for another origin; refusing the endpoint keeps the whole
        // download on a link the contract allows.
        if !crate::endpoint::is_allowed(&info.url) {
            return Err(DownloadError::InsecureUrl);
        }
        let bytes = info.bytes;
        if bytes == 0 {
            return Err(DownloadError::Empty);
        }
        let mut downloaded = File::open(&part)?;
        let book_hash = partial_md5(&mut downloaded).map_err(DownloadError::Io)?;
        drop(downloaded);
        let path = unique_path(dir, &stem, &ext);
        fs::rename(&part, &path)?;
        Ok(DownloadedBook {
            path,
            book_hash,
            bytes,
        })
    })();

    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// The URL of `entry`, rebuilt from the configured endpoint.
///
/// `RemoteEntry::path` is the server's decoded absolute path. It must live
/// under the endpoint's own path (`/dav/`, plus a configured `remote_dir`), be
/// absolute, and contain no `..` or backslash. The remainder is handed to
/// [`JianguoyunConfig::join_remote_path`], which percent-encodes every segment
/// for the wire, so Chinese titles and spaces work on the real server.
fn download_url(config: &JianguoyunConfig, entry: &RemoteEntry) -> Result<Url, DownloadError> {
    if entry.is_dir {
        return Err(DownloadError::NotAFile);
    }
    let base = config.normalized_base_url()?;
    let base_url = Url::parse(&base).map_err(|error| {
        DownloadError::Config(JianguoyunError::InvalidConfig(error.to_string()))
    })?;
    let base_path = percent_decode(base_url.path());
    let prefix = base_path.trim_end_matches('/');

    let path = entry.path.trim();
    if !path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err(DownloadError::UnsafePath);
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err(DownloadError::UnsafePath);
    }
    let rest = path
        .strip_prefix(prefix)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .ok_or(DownloadError::UnsafePath)?;
    let relative = rest.trim_start_matches('/');
    if relative.is_empty() {
        return Err(DownloadError::NotAFile);
    }
    let joined = config.join_remote_path(relative)?;
    Url::parse(&joined)
        .map_err(|error| DownloadError::Config(JianguoyunError::InvalidConfig(error.to_string())))
}

/// The lowercase extension of an ebook entry: `format` when the listing gave
/// one, otherwise the extension of `name`. Anything else is refused.
fn entry_extension(entry: &RemoteEntry) -> Result<String, DownloadError> {
    if let Some(format) = entry
        .format
        .as_deref()
        .map(str::trim)
        .filter(|format| is_ebook_extension(format))
    {
        return Ok(format.trim_start_matches('.').to_ascii_lowercase());
    }
    let name = entry.name.trim();
    let (stem, extension) = name
        .rsplit_once('.')
        .ok_or(DownloadError::UnsupportedFormat)?;
    if stem.is_empty() || !is_ebook_extension(extension) {
        return Err(DownloadError::UnsupportedFormat);
    }
    Ok(extension.to_ascii_lowercase())
}

/// The file-name stem of `name` without its ebook extension, for
/// [`safe_name`].
fn book_stem(name: &str, extension: &str) -> String {
    let name = name.trim();
    let without = match name.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case(extension) && !stem.is_empty() => stem,
        _ => name,
    };
    without.trim().to_string()
}

/// Percent-decode an encoded URL path, keeping invalid escapes verbatim
/// (`+` is not a space in a path).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push(high << 4 | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(base: &str) -> JianguoyunConfig {
        JianguoyunConfig::new(base, "reader@example.com", "app-password", "")
    }

    fn entry(path: &str, name: &str, format: Option<&str>) -> RemoteEntry {
        RemoteEntry {
            path: path.to_string(),
            name: name.to_string(),
            is_dir: false,
            size: 10,
            etag: None,
            last_modified: None,
            content_type: None,
            format: format.map(String::from),
        }
    }

    #[test]
    fn download_url_keeps_chinese_paths_under_the_endpoint() {
        let config = config("https://dav.jianguoyun.com/dav/");
        let url = download_url(
            &config,
            &entry("/dav/Books/三体.epub", "三体.epub", Some("epub")),
        )
        .unwrap();
        assert_eq!(
            url.to_string(),
            "https://dav.jianguoyun.com/dav/Books/%E4%B8%89%E4%BD%93.epub"
        );

        let mut config = config;
        config.remote_dir = "书库".to_string();
        let url = download_url(
            &config,
            &entry("/dav/书库/我的 书.epub", "我的 书.epub", None),
        )
        .unwrap();
        assert_eq!(
            url.to_string(),
            "https://dav.jianguoyun.com/dav/%E4%B9%A6%E5%BA%93/%E6%88%91%E7%9A%84%20%E4%B9%A6.epub"
        );
    }

    #[test]
    fn download_url_refuses_escapes_and_directories() {
        let config = config("https://dav.jianguoyun.com/dav/");
        for path in [
            "/dav/../etc/passwd",
            "/dav/Books/../../etc/passwd",
            "/dav/Books\\x.epub",
            "Books/x.epub",
            "/other/x.epub",
            "/davish/x.epub",
            "/dav/",
        ] {
            let error = download_url(&config, &entry(path, "x.epub", Some("epub"))).unwrap_err();
            assert!(
                matches!(error, DownloadError::UnsafePath | DownloadError::NotAFile),
                "{path}: {error:?}"
            );
        }
        let mut directory = entry("/dav/Books/", "Books", None);
        directory.is_dir = true;
        assert!(matches!(
            download_url(&config, &directory),
            Err(DownloadError::NotAFile)
        ));
    }

    #[test]
    fn extensions_come_from_the_format_or_the_name() {
        assert_eq!(
            entry_extension(&entry("/dav/a.EPUB", "a.EPUB", Some("EPUB"))).unwrap(),
            "epub"
        );
        assert_eq!(
            entry_extension(&entry("/dav/三体.epub", "三体.epub", None)).unwrap(),
            "epub"
        );
        assert!(matches!(
            entry_extension(&entry("/dav/readme.md", "readme.md", None)),
            Err(DownloadError::UnsupportedFormat)
        ));
        assert!(matches!(
            entry_extension(&entry("/dav/noext", "noext", None)),
            Err(DownloadError::UnsupportedFormat)
        ));
        assert_eq!(book_stem("三体.epub", "epub"), "三体");
        assert_eq!(book_stem(".hidden.epub", "epub"), ".hidden");
        assert_eq!(book_stem("noext", "epub"), "noext");
    }

    #[test]
    fn percent_decode_keeps_invalid_escapes_and_plus() {
        assert_eq!(
            percent_decode("/dav/%E4%B8%89%E4%BD%93.epub"),
            "/dav/三体.epub"
        );
        assert_eq!(percent_decode("/a+b/%zz"), "/a+b/%zz");
    }

    #[test]
    fn every_http_error_keeps_its_class() {
        assert_eq!(map_http_error(HttpError::Timeout), TransportError::Timeout);
        assert_eq!(
            map_http_error(HttpError::Connect("refused".into())),
            TransportError::Network("refused".into())
        );
        assert_eq!(
            map_http_error(HttpError::NotAllowed("evil.example:443".into())),
            TransportError::Blocked("evil.example:443 is not in the allowed hosts".into())
        );
        assert_eq!(
            map_http_error(HttpError::NoTls),
            TransportError::Blocked("HTTPS is not supported in this build".into())
        );
        assert_eq!(
            map_http_error(HttpError::Tls {
                clock: false,
                detail: "bad certificate".into()
            }),
            TransportError::Network("TLS error: bad certificate".into())
        );
        assert_eq!(
            map_http_error(HttpError::Status(403, "no".into())),
            TransportError::HttpStatus(403, "no".into())
        );
        assert_eq!(
            map_http_error(HttpError::TooLarge(10)),
            TransportError::TooLarge(10)
        );
        assert_eq!(
            map_http_error(HttpError::Io(std::io::Error::other("disk"))),
            TransportError::Network("disk".into())
        );
        assert_eq!(
            map_http_error(HttpError::Cancelled),
            TransportError::Other("cancelled".into())
        );
        assert_eq!(
            map_http_error(HttpError::Other("odd".into())),
            TransportError::Other("odd".into())
        );
    }
}
