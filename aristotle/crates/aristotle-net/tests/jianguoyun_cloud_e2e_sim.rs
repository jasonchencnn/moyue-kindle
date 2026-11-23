// SPDX-License-Identifier: AGPL-3.0-or-later
//! Adversarial end-to-end suite for the Jianguoyun (Nutstore) WebDAV channel,
//! run against a real HTTP/1.1 server on `127.0.0.1:0`.
//!
//! The server speaks the wire subset of `docs/sync-contract.md` §15 that the
//! real service uses: Basic authentication over pre-sent credentials,
//! `PROPFIND` with a `d:` namespace and percent-encoded `href`s, `MKCOL`,
//! `GET`/`HEAD`, `PUT` and `DELETE`, with the error statuses the contract
//! names (`401`, `403`, `404`, `405`, `409`, `429`, `503`) and fault injection
//! for truncated bodies, dropped connections and slow chunked downloads.
//!
//! Scenarios:
//! 1. Basic authentication is pre-sent, a wrong password is `AuthFailed`, and
//!    `403`/`404` map to `Forbidden`/`NotFound`.
//! 2. `PROPFIND Depth: 1` lists Chinese folders and books, filters hidden and
//!    non-ebook entries, and `HEAD` distinguishes present from missing files.
//! 3. `409` on `PUT` creates `.moyue/` and `.moyue/sync/` with `MKCOL` and
//!    retries once; an existing collection answering `405` counts as ready.
//! 4. `429`/`503` become `RateLimited { retry_after_secs }` without a resend.
//! 5. A Chinese book streams into a `.part` file, is hashed with `partialMD5`
//!    and renamed; a second copy never overwrites the first.
//! 6. A cancel or a truncated body leaves no `.part` and no book behind.
//! 7. `scoped` firewall mode blocks the host until the account's entry allows
//!    it, and a public plain-HTTP endpoint never receives the app password.
//! 8. Two devices converge progress, notes and a same-millisecond tombstone.
//! 9. `ReadestTransport` dispatches `PROPFIND`/`MKCOL`/`HEAD`/`DELETE`.
//! 10. Transient reads are retried, writes are not.
//! 11. `accounts.json` is owner-only, `jianguoyun.json` loses its password,
//!     and the imported settings drive a real listing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aristotle_net::http::{Error as HttpError, Request};
use aristotle_net::jianguoyun::{download_book, DownloadError, JianguoyunTransport, MAX_DOWNLOAD};
use aristotle_net::policy::parse_allow;
use aristotle_net::store::{
    Accounts, Imported, JianguoyunSettings, FILE as ACCOUNTS_FILE, JIANGUOYUN_FEATURE,
};
use aristotle_net::{Policy, ReadestTransport};
use inkread_core::hash::partial_md5;
use inkread_core::jianguoyun::{
    decide_progress, is_ebook_extension, JianguoyunBookSync, JianguoyunClient, JianguoyunConfig,
    JianguoyunError, JianguoyunNote, JianguoyunProgress, NoteKind, ProgressDecision, RemoteEntry,
    PROPFIND_BODY,
};
use inkread_core::sync::{HttpRequest, HttpTransport, TransportError};

const USER: &str = "reader@example.com";
const APP_PASSWORD: &str = "app-password-1234";
const HASH: &str = "0123456789abcdef0123456789abcdef";

// ---------------------------------------------------------------------------
// The loopback WebDAV server
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    /// The raw request target, still percent-encoded.
    target: String,
    /// The decoded path without the query or a trailing slash.
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Debug)]
enum Fault {
    /// Answer the next matching request with no bytes at all.
    Drop { method: String },
    /// Answer with bytes that are not an HTTP response.
    Corrupt { method: String },
    /// Answer the next matching request with `status` (and `Retry-After`).
    Status {
        method: String,
        status: u16,
        retry_after: Option<u64>,
    },
    /// Answer the next matching request with a redirect to `location`.
    Redirect { method: String, location: String },
}

impl Fault {
    fn matches(&self, method: &str) -> bool {
        let wanted = match self {
            Fault::Drop { method }
            | Fault::Corrupt { method }
            | Fault::Status { method, .. }
            | Fault::Redirect { method, .. } => method,
        };
        wanted.eq_ignore_ascii_case(method)
    }
}

#[derive(Clone, Copy, Debug)]
struct SlowDownload {
    chunk: usize,
    delay: Duration,
}

#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// `HEAD` sends the headers (including `Content-Length`) but no body.
    omit_body: bool,
    /// Close the connection without writing anything.
    drop_connection: bool,
    /// Write these bytes as the body, not the HTTP status line.
    raw: Option<&'static [u8]>,
    /// Write the body in chunks with a pause, for a cancel mid-download.
    slow: Option<SlowDownload>,
    /// Send the headers and only this many body bytes, then close.
    truncate_after: Option<usize>,
}

impl Reply {
    fn status(status: u16) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            omit_body: false,
            drop_connection: false,
            raw: None,
            slow: None,
            truncate_after: None,
        }
    }

    fn body(status: u16, body: impl Into<Vec<u8>>) -> Reply {
        Reply {
            body: body.into(),
            ..Reply::status(status)
        }
    }

    fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// A `HEAD` answer: the headers go out, the body does not.
    fn omit_body(mut self) -> Reply {
        self.omit_body = true;
        self
    }
}

struct WebdavState {
    expected_auth: String,
    dirs: BTreeSet<String>,
    files: BTreeMap<String, Vec<u8>>,
    log: Vec<Recorded>,
    faults: VecDeque<Fault>,
    slow: Option<SlowDownload>,
    truncate_after: Option<usize>,
    require_auth: bool,
}

impl WebdavState {
    fn take_fault(&mut self, method: &str) -> Option<Reply> {
        let at = self.faults.iter().position(|fault| fault.matches(method))?;
        let fault = self.faults.remove(at)?;
        Some(match fault {
            Fault::Drop { .. } => Reply {
                drop_connection: true,
                ..Reply::status(0)
            },
            Fault::Corrupt { .. } => Reply {
                raw: Some(b"THIS IS NOT AN HTTP RESPONSE\r\n\r\n"),
                ..Reply::status(0)
            },
            Fault::Status {
                status,
                retry_after,
                ..
            } => {
                let reply = Reply::body(status, format!("injected HTTP {status}"));
                match retry_after {
                    Some(seconds) => reply.header("Retry-After", &seconds.to_string()),
                    None => reply,
                }
            }
            Fault::Redirect { location, .. } => Reply::status(302).header("Location", &location),
        })
    }

    fn route(&mut self, request: &Recorded) -> Reply {
        if self.require_auth && request.header("authorization") != Some(&self.expected_auth) {
            return Reply::status(401).header("WWW-Authenticate", "Basic realm=\"Jianguoyun\"");
        }
        if request.path == "/dav/forbidden" || request.path.starts_with("/dav/forbidden/") {
            return Reply::status(403);
        }
        match request.method.as_str() {
            "PROPFIND" => self.propfind(request),
            "GET" => self.get(request),
            "HEAD" => self.head(request),
            "PUT" => self.put(request),
            "MKCOL" => self.mkcol(request),
            "DELETE" => self.delete(request),
            _ => Reply::status(405),
        }
    }

    fn propfind(&self, request: &Recorded) -> Reply {
        if !self.dirs.contains(&request.path) {
            return Reply::body(404, "no such collection");
        }
        let depth = request.header("depth").unwrap_or("0");
        let xml = self.propfind_xml(&request.path, depth);
        Reply::body(207, xml).header("Content-Type", "application/xml; charset=utf-8")
    }

    fn get(&self, request: &Recorded) -> Reply {
        let Some(bytes) = self.files.get(&request.path) else {
            return Reply::body(404, "no such file");
        };
        let mut reply = Reply::body(200, bytes.clone()).header(
            "Content-Type",
            if request.path.ends_with(".pdf") {
                "application/pdf"
            } else {
                "application/epub+zip"
            },
        );
        reply.slow = self.slow;
        reply.truncate_after = self.truncate_after;
        reply
    }

    fn head(&self, request: &Recorded) -> Reply {
        if let Some(bytes) = self.files.get(&request.path) {
            return Reply::status(200)
                .header("Content-Length", &bytes.len().to_string())
                .header("Content-Type", "application/epub+zip")
                .omit_body();
        }
        if self.dirs.contains(&request.path) {
            return Reply::status(200).header("Content-Length", "0").omit_body();
        }
        Reply::status(404)
    }

    fn put(&mut self, request: &Recorded) -> Reply {
        if self.dirs.contains(&request.path) {
            return Reply::status(405);
        }
        if !self.dirs.contains(parent_path(&request.path)) {
            return Reply::status(409);
        }
        let existed = self
            .files
            .insert(request.path.clone(), request.body.clone())
            .is_some();
        Reply::status(if existed { 204 } else { 201 })
    }

    fn mkcol(&mut self, request: &Recorded) -> Reply {
        if self.dirs.contains(&request.path) {
            return Reply::status(405);
        }
        if !self.dirs.contains(parent_path(&request.path)) {
            return Reply::status(409);
        }
        self.dirs.insert(request.path.clone());
        Reply::status(201)
    }

    fn delete(&mut self, request: &Recorded) -> Reply {
        if self.files.remove(&request.path).is_some() {
            return Reply::status(204);
        }
        if request.path != "/dav" && self.dirs.contains(&request.path) {
            let occupied = self
                .dirs
                .iter()
                .any(|dir| dir != &request.path && parent_path(dir) == request.path)
                || self
                    .files
                    .keys()
                    .any(|file| parent_path(file) == request.path);
            if occupied {
                return Reply::status(409);
            }
            self.dirs.remove(&request.path);
            return Reply::status(204);
        }
        Reply::status(404)
    }

    fn propfind_xml(&self, dir: &str, depth: &str) -> String {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\">\n",
        );
        xml.push_str(&propfind_entry(dir, true, 0));
        if depth != "0" {
            let mut children: Vec<(String, bool, u64)> = Vec::new();
            for child in &self.dirs {
                if child != dir && parent_path(child) == dir {
                    children.push((child.clone(), true, 0));
                }
            }
            for (path, bytes) in &self.files {
                if parent_path(path) == dir {
                    children.push((path.clone(), false, bytes.len() as u64));
                }
            }
            for (path, is_dir, size) in children {
                xml.push_str(&propfind_entry(&path, is_dir, size));
            }
        }
        xml.push_str("</d:multistatus>\n");
        xml
    }
}

struct WebdavServer {
    port: u16,
    state: Arc<Mutex<WebdavState>>,
    running: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl WebdavServer {
    fn start() -> WebdavServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let mut dirs = BTreeSet::new();
        dirs.insert("/dav".to_string());
        // The Basic header a correct client must pre-send, built with the same
        // code the client uses so the test checks the wire, not a copy.
        let expected_auth =
            JianguoyunConfig::new("http://127.0.0.1", USER, APP_PASSWORD, "").basic_auth_header();
        let state = Arc::new(Mutex::new(WebdavState {
            expected_auth,
            dirs,
            files: BTreeMap::new(),
            log: Vec::new(),
            faults: VecDeque::new(),
            slow: None,
            truncate_after: None,
            require_auth: true,
        }));
        let running = Arc::new(AtomicBool::new(true));
        let accept_state = Arc::clone(&state);
        let accept_running = Arc::clone(&running);
        let accept = thread::spawn(move || {
            while accept_running.load(Ordering::SeqCst) {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let connections = Arc::clone(&accept_state);
                thread::spawn(move || handle_connection(stream, connections));
            }
        });
        WebdavServer {
            port,
            state,
            running,
            accept: Some(accept),
        }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}/dav/", self.port)
    }

    fn config(&self, remote_dir: &str) -> JianguoyunConfig {
        JianguoyunConfig::new(self.base(), USER, APP_PASSWORD, remote_dir)
    }

    fn policy(&self) -> Policy {
        let mut policy = Policy::open();
        policy.scoped = true;
        policy.allowed = parse_allow(&format!("127.0.0.1:{}", self.port));
        policy
    }

    fn blocked_policy(&self) -> Policy {
        Policy {
            scoped: true,
            allowed: Vec::new(),
        }
    }

    fn transport(&self) -> JianguoyunTransport {
        JianguoyunTransport::new(self.policy())
    }

    fn expected_auth(&self) -> String {
        self.state.lock().unwrap().expected_auth.clone()
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut WebdavState) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    fn seed_dir(&self, path: &str) {
        self.with_state(|state| state.dirs.insert(path.to_string()));
    }

    fn seed_file(&self, path: &str, bytes: Vec<u8>) {
        self.with_state(|state| state.files.insert(path.to_string(), bytes));
    }

    fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.with_state(|state| state.files.get(path).cloned())
    }

    fn requests(&self) -> Vec<Recorded> {
        self.with_state(|state| state.log.clone())
    }

    fn method_count(&self, method: &str) -> usize {
        self.with_state(|state| {
            state
                .log
                .iter()
                .filter(|request| request.method == method)
                .count()
        })
    }

    fn push_fault(&self, fault: Fault) {
        self.with_state(|state| state.faults.push_back(fault));
    }

    fn set_slow(&self, slow: SlowDownload) {
        self.with_state(|state| state.slow = Some(slow));
    }

    fn set_truncate(&self, after: usize) {
        self.with_state(|state| state.truncate_after = Some(after));
    }

    fn set_require_auth(&self, required: bool) {
        self.with_state(|state| state.require_auth = required);
    }
}

impl Drop for WebdavServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<WebdavState>>) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let reply = {
        let mut state = state.lock().unwrap();
        state.log.push(request.clone());
        match state.take_fault(&request.method) {
            Some(reply) => reply,
            None => state.route(&request),
        }
    };
    let _ = write_reply(&mut stream, &reply);
}

fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Some(Recorded {
        path: normalize_path(&target),
        method,
        target,
        headers,
        body,
    })
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    if reply.drop_connection {
        return Ok(());
    }
    if let Some(raw) = reply.raw {
        stream.write_all(raw)?;
        return stream.flush();
    }
    let body = if reply.omit_body {
        &[][..]
    } else {
        &reply.body[..]
    };
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !reply
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    {
        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    if reply.omit_body {
        return stream.flush();
    }
    if let Some(slow) = reply.slow {
        for chunk in body.chunks(slow.chunk.max(1)) {
            stream.write_all(chunk)?;
            stream.flush()?;
            thread::sleep(slow.delay);
        }
        return Ok(());
    }
    if let Some(after) = reply.truncate_after {
        let cut = after.min(body.len());
        stream.write_all(&body[..cut])?;
        return stream.flush();
    }
    stream.write_all(body)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        207 => "Multi-Status",
        301 => "Moved Permanently",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

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

fn normalize_path(target: &str) -> String {
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let decoded = percent_decode(path);
    let mut normalized = String::new();
    for segment in decoded.split('/') {
        if segment.is_empty() {
            continue;
        }
        normalized.push('/');
        normalized.push_str(segment);
    }
    if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    }
}

fn parent_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &path[..at],
    }
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(inkread_core::cloud::url_encode)
        .collect::<Vec<_>>()
        .join("/")
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn propfind_entry(path: &str, is_dir: bool, size: u64) -> String {
    let name = path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("");
    let href = format!("{}{}", encode_path(path), if is_dir { "/" } else { "" });
    let resource_type = if is_dir { "<d:collection/>" } else { "" };
    let content_type = if is_dir {
        "httpd/unix-directory"
    } else if path.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/epub+zip"
    };
    format!(
        "<d:response>\n\
         <d:href>{href}</d:href>\n\
         <d:propstat>\n\
         <d:prop>\n\
         <d:displayname>{name}</d:displayname>\n\
         <d:resourcetype>{resource_type}</d:resourcetype>\n\
         <d:getcontentlength>{size}</d:getcontentlength>\n\
         <d:getlastmodified>Mon, 23 Nov 2026 10:00:00 GMT</d:getlastmodified>\n\
         <d:getetag>\"etag-{etag}\"</d:getetag>\n\
         <d:getcontenttype>{content_type}</d:getcontenttype>\n\
         </d:prop>\n\
         <d:status>HTTP/1.1 200 OK</d:status>\n\
         </d:propstat>\n\
         </d:response>\n",
        name = xml_escape(name),
        etag = xml_escape(&inkread_core::cloud::url_encode(path)),
    )
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn scratch_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir()
        .join("aristotle-net-jianguoyun-e2e")
        .join(format!("{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn book_bytes(len: usize, seed: usize) -> Vec<u8> {
    (0..len)
        .map(|index| ((index.wrapping_mul(31).wrapping_add(seed)) % 251) as u8)
        .collect()
}

fn progress(fraction: f64, updated_at_ms: i64, device: &str) -> JianguoyunProgress {
    JianguoyunProgress {
        xpointer: Some("/body/DocFragment[5]/body/p[12]/text().34".to_string()),
        cfi: None,
        fraction,
        page: Some(85),
        total_pages: Some(200),
        chapter_title: Some("第五章".to_string()),
        updated_at_ms,
        device_id: device.to_string(),
    }
}

fn record(title: &str, device: &str, updated_at_ms: i64, fraction: f64) -> JianguoyunBookSync {
    JianguoyunBookSync {
        version: 1,
        book_hash: HASH.to_string(),
        title: title.to_string(),
        author: "刘慈欣".to_string(),
        remote_path: "/Books/三体.epub".to_string(),
        progress: Some(progress(fraction, updated_at_ms, device)),
        notes: Vec::new(),
    }
}

fn entry(path: &str, name: &str, format: Option<&str>) -> RemoteEntry {
    RemoteEntry {
        path: path.to_string(),
        name: name.to_string(),
        is_dir: false,
        size: 0,
        etag: None,
        last_modified: None,
        content_type: None,
        format: format.map(str::to_string),
    }
}

fn stored_record(server: &WebdavServer) -> JianguoyunBookSync {
    let bytes = server
        .file(&format!("/dav/.moyue/sync/{HASH}.json"))
        .expect("the sync record is on the server");
    serde_json::from_slice(&bytes).unwrap()
}

// ---------------------------------------------------------------------------
// 1. Authentication and status mapping
// ---------------------------------------------------------------------------

#[test]
fn basic_auth_is_present_and_status_codes_map_to_the_contract() {
    let server = WebdavServer::start();
    server.seed_dir("/dav/Books");
    server.seed_file("/dav/Books/dune.epub", b"dune".to_vec());

    let client = JianguoyunClient::new(server.config(""), server.transport());
    client.test_connection().unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PROPFIND");
    assert_eq!(requests[0].path, "/dav");
    assert_eq!(requests[0].header("depth"), Some("0"));
    assert_eq!(
        requests[0].header("authorization"),
        Some(server.expected_auth().as_str()),
        "the application password must be pre-sent"
    );
    assert!(
        String::from_utf8_lossy(&requests[0].body).contains("getcontentlength"),
        "PROPFIND must ask for the properties the contract names"
    );

    // A wrong application password: 401 becomes AuthFailed.
    let mut wrong = server.config("");
    wrong.password = "not-the-app-password".to_string();
    let rejected = JianguoyunClient::new(wrong, server.transport());
    assert_eq!(
        rejected.test_connection().unwrap_err(),
        JianguoyunError::AuthFailed
    );

    // 403 and 404 stay distinguishable.
    let forbidden = JianguoyunClient::new(server.config("forbidden"), server.transport());
    assert_eq!(
        forbidden.list_dir("").unwrap_err(),
        JianguoyunError::Forbidden
    );
    assert_eq!(
        client.list_dir("missing").unwrap_err(),
        JianguoyunError::NotFound
    );
    // A missing sync record is not an error, as the contract requires.
    assert!(client.pull_book_sync(HASH).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// 2. PROPFIND browsing, Chinese paths, filtering, HEAD
// ---------------------------------------------------------------------------

#[test]
fn propfind_lists_chinese_paths_filters_hidden_entries_and_head_sees_files() {
    let server = WebdavServer::start();
    for dir in [
        "/dav/书库",
        "/dav/书库/子目录",
        "/dav/书库/.moyue",
        "/dav/书库/三体.sdr",
    ] {
        server.seed_dir(dir);
    }
    server.seed_file("/dav/书库/三体.epub", b"san-ti".to_vec());
    server.seed_file("/dav/书库/我的 书.pdf", b"my-book".to_vec());
    server.seed_file("/dav/书库/readme.md", b"not-a-book".to_vec());
    server.seed_file("/dav/书库/.DS_Store", b"junk".to_vec());
    server.seed_file("/dav/书库/三体.sdr/metadata.epub", b"koreader".to_vec());

    let client = JianguoyunClient::new(server.config(""), server.transport());
    let entries = client.list_dir("书库").unwrap();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["子目录", "三体.epub", "我的 书.pdf"]);
    assert!(entries[0].is_dir);
    assert_eq!(entries[0].path, "/dav/书库/子目录");
    assert_eq!(entries[1].path, "/dav/书库/三体.epub");
    assert_eq!(entries[1].format.as_deref(), Some("epub"));
    assert_eq!(entries[2].format.as_deref(), Some("pdf"));

    let request = server.requests().pop().unwrap();
    assert_eq!(request.method, "PROPFIND");
    assert_eq!(request.path, "/dav/书库");
    assert_eq!(request.target, "/dav/%E4%B9%A6%E5%BA%93/");
    assert_eq!(request.header("depth"), Some("1"));
    assert_eq!(
        request.header("content-type"),
        Some("application/xml; charset=utf-8")
    );

    assert!(client.exists("书库/三体.epub").unwrap());
    assert!(!client.exists("书库/missing.epub").unwrap());
    let heads: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "HEAD")
        .map(|request| request.path)
        .collect();
    assert_eq!(heads, ["/dav/书库/三体.epub", "/dav/书库/missing.epub"]);
}

// ---------------------------------------------------------------------------
// 3. Missing collections: 409 -> MKCOL -> retry, 405 means ready
// ---------------------------------------------------------------------------

#[test]
fn push_creates_the_sync_collections_on_409_and_treats_mkcol_405_as_ready() {
    let server = WebdavServer::start();
    let client = JianguoyunClient::new(server.config(""), server.transport());
    let first = record("三体", "KINDLE-OASIS-2", 1_000, 0.25);

    client.push_book_sync(&first).unwrap();
    let sequence: Vec<(String, String)> = server
        .requests()
        .into_iter()
        .map(|request| (request.method, request.path))
        .collect();
    let target = format!("/dav/.moyue/sync/{HASH}.json");
    assert_eq!(
        sequence,
        [
            ("PUT".to_string(), target.clone()),
            ("MKCOL".to_string(), "/dav/.moyue".to_string()),
            ("MKCOL".to_string(), "/dav/.moyue/sync".to_string()),
            ("PUT".to_string(), target),
        ]
    );
    let stored = stored_record(&server);
    assert_eq!(stored, first);

    // Now the collections exist: the next push is a single PUT, and a 204
    // from the server is still a success.
    server.seed_dir("/dav/.moyue/sync");
    let second = record("三体", "KINDLE-OASIS-2", 2_000, 0.5);
    client.push_book_sync(&second).unwrap();
    assert_eq!(server.method_count("PUT"), 3);
    assert_eq!(server.method_count("MKCOL"), 2);

    // A 409 on a PUT whose collections already exist: both MKCOLs answer 405,
    // which the contract counts as "already there", and the PUT is retried
    // exactly once.
    server.push_fault(Fault::Status {
        method: "PUT".to_string(),
        status: 409,
        retry_after: None,
    });
    let third = record("三体", "KINDLE-OASIS-2", 3_000, 0.75);
    client.push_book_sync(&third).unwrap();
    assert_eq!(server.method_count("PUT"), 5);
    assert_eq!(server.method_count("MKCOL"), 4);
    assert_eq!(stored_record(&server).progress.unwrap().fraction, 0.75);

    // A `301` on `MKCOL` means the collection is already there too. The
    // redirect is not followed (only GET redirects), so the collection is
    // never listed twice.
    server.push_fault(Fault::Status {
        method: "PUT".to_string(),
        status: 409,
        retry_after: None,
    });
    server.push_fault(Fault::Status {
        method: "MKCOL".to_string(),
        status: 301,
        retry_after: None,
    });
    server.push_fault(Fault::Status {
        method: "MKCOL".to_string(),
        status: 301,
        retry_after: None,
    });
    let fourth = record("三体", "KINDLE-OASIS-2", 4_000, 0.9);
    client.push_book_sync(&fourth).unwrap();
    assert_eq!(server.method_count("PUT"), 7);
    assert_eq!(server.method_count("MKCOL"), 6);
    assert_eq!(stored_record(&server).progress.unwrap().fraction, 0.9);
}

// ---------------------------------------------------------------------------
// 4. Rate limiting
// ---------------------------------------------------------------------------

#[test]
fn rate_limiting_returns_retry_after_and_never_resends() {
    let server = WebdavServer::start();
    server.seed_dir("/dav/.moyue/sync");
    server.push_fault(Fault::Status {
        method: "PUT".to_string(),
        status: 503,
        retry_after: Some(42),
    });
    let client = JianguoyunClient::new(server.config(""), server.transport());
    assert_eq!(
        client
            .push_book_sync(&record("三体", "KINDLE", 1, 0.1))
            .unwrap_err(),
        JianguoyunError::RateLimited {
            retry_after_secs: Some(42)
        }
    );
    assert_eq!(server.method_count("PUT"), 1, "a rate limit is not retried");
    assert_eq!(server.method_count("MKCOL"), 0, "no MKCOL on a rate limit");
    assert!(server
        .file(&format!("/dav/.moyue/sync/{HASH}.json"))
        .is_none());

    server.push_fault(Fault::Status {
        method: "GET".to_string(),
        status: 429,
        retry_after: Some(30),
    });
    assert_eq!(
        client.pull_book_sync(HASH).unwrap_err(),
        JianguoyunError::RateLimited {
            retry_after_secs: Some(30)
        }
    );
    assert_eq!(server.method_count("GET"), 1);
}

// ---------------------------------------------------------------------------
// 5. Streaming download
// ---------------------------------------------------------------------------

#[test]
fn a_chinese_book_streams_hashes_and_renames_without_a_part_file() {
    let server = WebdavServer::start();
    let bytes = book_bytes(600 * 1024, 11);
    server.seed_dir("/dav/书库");
    server.seed_file("/dav/书库/三体.epub", bytes.clone());
    server.set_slow(SlowDownload {
        chunk: 32 * 1024,
        delay: Duration::from_millis(1),
    });
    let client = JianguoyunClient::new(server.config(""), server.transport());
    let entry = client
        .list_dir("书库")
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == "三体.epub")
        .expect("the book is listed");
    let dir = scratch_dir("download");
    let config = server.config("");
    let policy = server.policy();

    let mut seen = Vec::new();
    let downloaded = download_book(
        policy.clone(),
        &config,
        &entry,
        &dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |bytes_so_far, total| {
            assert_eq!(total, Some(bytes.len() as u64));
            seen.push(bytes_so_far);
        },
    )
    .unwrap();

    assert_eq!(downloaded.path, dir.join("三体.epub"));
    assert_eq!(fs::read(&downloaded.path).unwrap(), bytes);
    assert_eq!(
        downloaded.book_hash,
        partial_md5(&mut Cursor::new(bytes.clone())).unwrap()
    );
    assert_eq!(downloaded.bytes, bytes.len() as u64);
    assert_eq!(*seen.last().unwrap(), bytes.len() as u64);
    assert!(seen.windows(2).all(|pair| pair[0] < pair[1]));
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["三体.epub"]);
    assert!(
        !names.iter().any(|name| name.ends_with(".part")),
        "a finished download leaves no .part"
    );

    let gets: Vec<Recorded> = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "GET")
        .collect();
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].path, "/dav/书库/三体.epub");
    assert_eq!(
        gets[0].target,
        "/dav/%E4%B9%A6%E5%BA%93/%E4%B8%89%E4%BD%93.epub"
    );
    assert_eq!(
        gets[0].header("authorization"),
        Some(server.expected_auth().as_str())
    );

    // A second download of the same title is renamed, never overwritten.
    let again = download_book(
        policy,
        &config,
        &entry,
        &dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(again.path, dir.join("三体 (2).epub"));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 5b. A cross-origin download redirect keeps the password on the first origin
// ---------------------------------------------------------------------------

#[test]
fn a_cross_origin_download_redirect_does_not_leak_the_password() {
    let bytes = book_bytes(48 * 1024, 17);
    let target = WebdavServer::start();
    target.seed_dir("/dav/书库");
    target.seed_file("/dav/书库/三体.epub", bytes.clone());
    target.set_require_auth(false);
    let location = target
        .config("")
        .join_remote_path("书库/三体.epub")
        .unwrap();

    let source = WebdavServer::start();
    source.push_fault(Fault::Redirect {
        method: "GET".to_string(),
        location,
    });
    let dir = scratch_dir("redirect");
    let downloaded = download_book(
        Policy::open(),
        &source.config(""),
        &entry("/dav/书库/三体.epub", "三体.epub", Some("epub")),
        &dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();

    assert_eq!(fs::read(&downloaded.path).unwrap(), bytes);
    assert_eq!(
        downloaded.book_hash,
        partial_md5(&mut Cursor::new(bytes)).unwrap()
    );
    let redirected = target
        .requests()
        .into_iter()
        .find(|request| request.method == "GET")
        .expect("the redirect is followed");
    assert_eq!(redirected.path, "/dav/书库/三体.epub");
    assert_eq!(
        redirected.header("authorization"),
        None,
        "the application password must not follow a redirect to another origin"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 6. Cancel and failures leave no partial file
// ---------------------------------------------------------------------------

#[test]
fn cancelling_a_download_removes_the_part_file() {
    let server = WebdavServer::start();
    let bytes = book_bytes(512 * 1024, 3);
    server.seed_dir("/dav/书库");
    server.seed_file("/dav/书库/三体.epub", bytes);
    server.set_slow(SlowDownload {
        chunk: 8 * 1024,
        delay: Duration::from_millis(10),
    });
    let client = JianguoyunClient::new(server.config(""), server.transport());
    let entry = client
        .list_dir("书库")
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == "三体.epub")
        .unwrap();
    let dir = scratch_dir("cancel");
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let worker = {
        let cancel = Arc::clone(&cancel);
        let dir = dir.clone();
        let config = server.config("");
        let policy = server.policy();
        thread::spawn(move || {
            download_book(
                policy,
                &config,
                &entry,
                &dir,
                MAX_DOWNLOAD,
                &cancel,
                &mut |bytes_so_far, _| {
                    let _ = sender.send(bytes_so_far);
                },
            )
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut seen = 0;
    while seen < 64 * 1024 {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let Ok(bytes_so_far) = receiver.recv_timeout(left) else {
            panic!("the download did not report progress before {deadline:?}");
        };
        seen = bytes_so_far;
    }
    cancel.store(true, Ordering::SeqCst);
    let result = worker.join().unwrap();
    assert!(
        matches!(result, Err(DownloadError::Cancelled)),
        "{result:?}"
    );
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        0,
        "a cancelled download leaves no .part and no book"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn failed_downloads_leave_no_part_file_and_never_dial_for_unsafe_paths() {
    let server = WebdavServer::start();
    let bytes = book_bytes(128 * 1024, 5);
    server.seed_dir("/dav/书库");
    server.seed_file("/dav/书库/三体.epub", bytes.clone());
    server.seed_file("/dav/书库/empty.epub", Vec::new());
    let config = server.config("");
    let policy = server.policy();

    // A body cut short by the server.
    server.set_truncate(1024);
    let dir = scratch_dir("truncated");
    let error = download_book(
        policy.clone(),
        &config,
        &entry("/dav/书库/三体.epub", "三体.epub", Some("epub")),
        &dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(matches!(error, DownloadError::Http(_)), "{error:?}");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
    server.with_state(|state| state.truncate_after = None);

    // A missing remote file.
    let dir = scratch_dir("missing");
    let error = download_book(
        policy.clone(),
        &config,
        &entry("/dav/书库/nope.epub", "nope.epub", Some("epub")),
        &dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(
        matches!(error, DownloadError::Http(HttpError::Status(404, _))),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);

    // A body over the caller's limit.
    let dir = scratch_dir("limit");
    let error = download_book(
        policy.clone(),
        &config,
        &entry("/dav/书库/三体.epub", "三体.epub", Some("epub")),
        &dir,
        1024,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(
        matches!(error, DownloadError::Http(HttpError::TooLarge(1024))),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);

    // A zero-byte book is refused rather than renamed into the library.
    let dir = scratch_dir("empty");
    let error = download_book(
        policy.clone(),
        &config,
        &entry("/dav/书库/empty.epub", "empty.epub", Some("epub")),
        &dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(matches!(error, DownloadError::Empty), "{error:?}");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);

    // Paths that could leave the library folder are refused before any GET.
    let before = server.method_count("GET");
    let dir = scratch_dir("unsafe");
    for unsafe_entry in [
        entry("/dav/../etc/passwd", "passwd.epub", Some("epub")),
        entry(
            "/dav/书库/../../etc/passwd.epub",
            "passwd.epub",
            Some("epub"),
        ),
        entry("/other/三体.epub", "三体.epub", Some("epub")),
        entry("relative/三体.epub", "三体.epub", Some("epub")),
        entry("/dav/书库/readme.md", "readme.md", None),
    ] {
        let error = download_book(
            policy.clone(),
            &config,
            &unsafe_entry,
            &dir,
            MAX_DOWNLOAD,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                DownloadError::UnsafePath | DownloadError::UnsupportedFormat
            ),
            "{unsafe_entry:?}: {error:?}"
        );
    }
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    assert_eq!(
        server.method_count("GET"),
        before,
        "an unsafe entry must not cause a request"
    );
    let _ = fs::remove_dir_all(&dir);

    // A public plain-HTTP endpoint is refused before the credentials travel.
    let insecure = JianguoyunConfig::new("http://example.com/dav/", USER, APP_PASSWORD, "");
    let insecure_dir = scratch_dir("insecure");
    let error = download_book(
        Policy::open(),
        &insecure,
        &entry("/dav/书库/三体.epub", "三体.epub", Some("epub")),
        &insecure_dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(matches!(error, DownloadError::InsecureUrl), "{error:?}");
    assert_eq!(fs::read_dir(&insecure_dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&insecure_dir);
}

// ---------------------------------------------------------------------------
// 7. Scoped firewall
// ---------------------------------------------------------------------------

#[test]
fn scoped_firewall_blocks_and_allows_the_configured_host() {
    let server = WebdavServer::start();
    server.seed_dir("/dav/Books");
    server.seed_file("/dav/Books/dune.epub", b"dune".to_vec());

    let allowed = JianguoyunClient::new(server.config(""), server.transport());
    assert_eq!(allowed.list_dir("Books").unwrap().len(), 1);
    let entry = allowed.list_dir("Books").unwrap().remove(0);

    let blocked_policy = server.blocked_policy();
    let blocked = JianguoyunClient::new(
        server.config(""),
        JianguoyunTransport::new(blocked_policy.clone()),
    );
    let error = blocked.list_dir("Books").unwrap_err();
    assert!(
        matches!(error, JianguoyunError::Transport(ref detail) if detail.contains("not in the allowed hosts")),
        "{error:?}"
    );

    let before = server.requests().len();
    let dir = scratch_dir("blocked");
    let error = download_book(
        blocked_policy,
        &server.config(""),
        &entry,
        &dir,
        MAX_DOWNLOAD,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(
        matches!(error, DownloadError::Http(HttpError::NotAllowed(_))),
        "{error:?}"
    );
    assert_eq!(
        server.requests().len(),
        before,
        "a blocked download must not reach the server"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);

    // The endpoint guard refuses a public plain-HTTP address before DNS.
    let transport = JianguoyunTransport::new(Policy::open());
    let error = transport
        .send(HttpRequest::new("PROPFIND", "http://example.com/dav/"))
        .unwrap_err();
    assert!(matches!(error, TransportError::Blocked(_)), "{error:?}");

    // A response over the transport's limit is TooLarge, not a panic.
    let small = JianguoyunTransport::with_timeout_and_limit(Policy::open(), 10, 64);
    let error = small
        .send(
            HttpRequest::new("PROPFIND", server.base())
                .with_header("Authorization", server.expected_auth())
                .with_header("Depth", "1"),
        )
        .unwrap_err();
    assert!(matches!(error, TransportError::TooLarge(_)), "{error:?}");
}

// ---------------------------------------------------------------------------
// 8. Two devices: progress, notes, tombstones
// ---------------------------------------------------------------------------

#[test]
fn two_devices_converge_progress_notes_and_a_same_millisecond_tombstone() {
    let server = WebdavServer::start();
    server.seed_dir("/dav/.moyue/sync");
    let kindle = JianguoyunClient::new(server.config(""), server.transport());
    let phone = JianguoyunClient::new(server.config(""), server.transport());

    let mut first = record("三体", "KINDLE-OASIS-2", 1_000, 0.25);
    first.notes = vec![JianguoyunNote::highlight(
        "note-1",
        "/body/p[1]/text().0",
        "/body/p[1]/text().20",
        "给岁月以文明",
        900,
    )];
    let (merged, decision) = kindle.sync_book(&first).unwrap();
    assert_eq!(decision, ProgressDecision::PushLocal);
    assert_eq!(merged, first);

    // The phone pulls what the Kindle pushed, reads further, edits note-1 and
    // adds note-2.
    let pulled = phone.pull_book_sync(HASH).unwrap().unwrap();
    assert_eq!(pulled, first);
    let mut second = record("三体", "PHONE-ANDROID", 2_000, 0.5);
    second.notes = vec![
        JianguoyunNote {
            text: Some("给岁月以文明，而不是给文明以岁月".to_string()),
            updated_at_ms: 2_000,
            ..pulled.notes[0].clone()
        },
        JianguoyunNote::highlight(
            "note-2",
            "/body/p[2]/text().0",
            "/body/p[2]/text().12",
            "黑暗森林",
            2_000,
        ),
    ];
    let (merged, decision) = phone.sync_book(&second).unwrap();
    assert_eq!(decision, ProgressDecision::PushLocal);
    assert_eq!(merged.progress.as_ref().unwrap().device_id, "PHONE-ANDROID");

    // The Kindle sees the newer position and the phone's notes.
    let pulled = kindle.pull_book_sync(HASH).unwrap().unwrap();
    assert_eq!(
        decide_progress(first.progress.as_ref(), pulled.progress.as_ref()),
        ProgressDecision::PullRemote
    );
    assert_eq!(pulled.notes.len(), 2);
    assert_eq!(
        pulled
            .notes
            .iter()
            .find(|note| note.id == "note-1")
            .unwrap()
            .text
            .as_deref(),
        Some("给岁月以文明，而不是给文明以岁月")
    );

    // The Kindle tombstones note-2 at exactly the millisecond the phone
    // edits it: the tombstone wins, and it is kept on the wire.
    let mut tombstone = pulled.clone();
    tombstone.progress = Some(progress(0.6, 3_000, "KINDLE-OASIS-2"));
    tombstone.notes = vec![
        pulled.notes[0].clone(),
        JianguoyunNote {
            updated_at_ms: 3_000,
            deleted_at_ms: Some(3_000),
            ..pulled.notes[1].clone()
        },
    ];
    let (merged, _) = kindle.sync_book(&tombstone).unwrap();
    assert!(merged
        .notes
        .iter()
        .find(|note| note.id == "note-2")
        .unwrap()
        .is_deleted());
    assert_eq!(
        stored_record(&server)
            .notes
            .iter()
            .find(|note| note.id == "note-2")
            .unwrap()
            .deleted_at_ms,
        Some(3_000)
    );

    let mut live = pulled.clone();
    live.progress = Some(progress(0.6, 3_000, "PHONE-ANDROID"));
    live.notes = vec![
        pulled.notes[0].clone(),
        JianguoyunNote {
            note: Some("edited in the same millisecond".to_string()),
            updated_at_ms: 3_000,
            ..pulled.notes[1].clone()
        },
    ];
    let (merged, _) = phone.sync_book(&live).unwrap();
    assert!(
        merged
            .notes
            .iter()
            .find(|note| note.id == "note-2")
            .unwrap()
            .is_deleted(),
        "a tombstone beats a live note on the same millisecond"
    );
    assert_eq!(
        stored_record(&server)
            .notes
            .iter()
            .find(|note| note.id == "note-2")
            .unwrap()
            .deleted_at_ms,
        Some(3_000)
    );

    // A fresh pull on either device keeps the tombstone and the surviving
    // highlight, and the progress converged on the later position.
    for client in [&kindle, &phone] {
        let pulled = client.pull_book_sync(HASH).unwrap().unwrap();
        assert!(pulled
            .notes
            .iter()
            .find(|note| note.id == "note-2")
            .unwrap()
            .is_deleted());
        assert_eq!(
            pulled
                .notes
                .iter()
                .find(|note| note.id == "note-1")
                .unwrap()
                .kind,
            NoteKind::Highlight
        );
        assert_eq!(pulled.progress.as_ref().unwrap().fraction, 0.6);
    }
}

// ---------------------------------------------------------------------------
// 9. ReadestTransport dispatches the WebDAV methods
// ---------------------------------------------------------------------------

#[test]
fn readest_transport_dispatches_propfind_mkcol_head_and_delete() {
    let server = WebdavServer::start();
    server.seed_dir("/dav/Books");
    server.seed_file("/dav/Books/dune.epub", b"dune".to_vec());
    let transport = ReadestTransport::with_timeout(server.policy(), 20);
    let auth = server.expected_auth();
    let base = server.base();

    let propfind = HttpRequest::new("PROPFIND", format!("{base}Books/"))
        .with_header("Authorization", &auth)
        .with_header("Depth", "1")
        .with_header("Content-Type", "application/xml; charset=utf-8")
        .with_body(PROPFIND_BODY.as_bytes());
    let response = transport.send(propfind).unwrap();
    assert_eq!(response.status, 207);
    assert!(String::from_utf8_lossy(&response.body).contains("multistatus"));

    let mkcol =
        HttpRequest::new("MKCOL", format!("{base}新建目录/")).with_header("Authorization", &auth);
    assert_eq!(transport.send(mkcol).unwrap().status, 201);

    let head = HttpRequest::new("HEAD", format!("{base}Books/dune.epub"))
        .with_header("Authorization", &auth);
    let response = transport.send(head).unwrap();
    assert_eq!(response.status, 200);
    assert!(response.body.is_empty());

    let delete = HttpRequest::new("DELETE", format!("{base}Books/dune.epub"))
        .with_header("Authorization", &auth);
    assert_eq!(transport.send(delete).unwrap().status, 204);
    assert!(server.file("/dav/Books/dune.epub").is_none());

    let patch = HttpRequest::new("PATCH", format!("{base}Books/dune.epub"));
    let error = transport.send(patch).unwrap_err();
    assert!(
        matches!(error, TransportError::Other(ref message) if message.contains("unsupported HTTP method")),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// 10. Retry policy
// ---------------------------------------------------------------------------

#[test]
fn transient_reads_are_retried_and_writes_are_not() {
    let server = WebdavServer::start();
    server.seed_dir("/dav/.moyue/sync");
    let client = JianguoyunClient::new(server.config(""), server.transport());

    server.push_fault(Fault::Drop {
        method: "PROPFIND".to_string(),
    });
    client.test_connection().unwrap();
    assert_eq!(
        server.method_count("PROPFIND"),
        2,
        "an idempotent read is retried after a dropped connection"
    );

    server.push_fault(Fault::Corrupt {
        method: "PUT".to_string(),
    });
    let error = client
        .push_book_sync(&record("三体", "KINDLE", 1, 0.1))
        .unwrap_err();
    assert!(matches!(error, JianguoyunError::Transport(_)), "{error:?}");
    assert_eq!(
        server.method_count("PUT"),
        1,
        "a write is not replayed after the request bytes were sent"
    );
    assert_eq!(server.method_count("MKCOL"), 0);
}

// ---------------------------------------------------------------------------
// 11. Credentials on disk
// ---------------------------------------------------------------------------

#[test]
fn accounts_are_owner_only_and_import_wipes_the_plaintext_password() {
    let server = WebdavServer::start();
    server.seed_dir("/dav/书库");
    server.seed_file("/dav/书库/三体.epub", b"san-ti".to_vec());
    let dir = scratch_dir("accounts");
    let import = dir.join("jianguoyun.json");
    let file = serde_json::json!({
        "server_url": server.base(),
        "username": USER,
        "password": APP_PASSWORD,
        "remote_dir": "书库",
        "auto_push": false,
    });
    fs::write(&import, serde_json::to_vec_pretty(&file).unwrap()).unwrap();

    let mut accounts = Accounts::default();
    assert_eq!(
        accounts.import_jianguoyun(&import, false).unwrap(),
        Imported::Loaded
    );
    accounts.save(&dir).unwrap();

    let file_mode = fs::metadata(dir.join(ACCOUNTS_FILE))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600, "accounts.json must not be group-readable");
    let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "the settings folder is owner-only");

    let import_text = fs::read_to_string(&import).unwrap();
    assert!(
        !import_text.contains(APP_PASSWORD),
        "the import file must lose its password: {import_text}"
    );
    assert!(import_text.contains(USER));
    assert!(import_text.contains("dav.jianguoyun.com") || import_text.contains("127.0.0.1"));

    let loaded = Accounts::load(&dir);
    let settings = loaded.jianguoyun.clone().unwrap();
    assert_eq!(settings.password, APP_PASSWORD);
    assert!(!settings.auto_push);
    let debug = format!("{settings:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(APP_PASSWORD));

    let expected_entry = format!("127.0.0.1:{}", server.port);
    assert!(loaded
        .allow_entries()
        .contains(&(expected_entry.clone(), JIANGUOYUN_FEATURE)));
    assert!(loaded
        .jianguoyun_allow_entries()
        .contains(&(expected_entry.clone(), JIANGUOYUN_FEATURE)));
    assert!(
        Accounts::default()
            .jianguoyun_allow_entries()
            .contains(&("dav.jianguoyun.com:443".to_string(), JIANGUOYUN_FEATURE)),
        "a fresh device must allow the official host for the first login"
    );

    // The imported settings really drive the network, including `remote_dir`.
    let policy = Policy {
        scoped: true,
        allowed: parse_allow(&expected_entry),
    };
    let client = JianguoyunClient::new(settings.to_core_config(), JianguoyunTransport::new(policy));
    let entries = client.list_dir("").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "三体.epub");
    assert!(is_ebook_extension(entries[0].format.as_deref().unwrap()));
    assert!(matches!(
        settings.to_core_config().normalized_base_url().unwrap().as_str(),
        url if url.ends_with("/dav/%E4%B9%A6%E5%BA%93/")
    ));

    let _ = fs::remove_dir_all(&dir);
}

/// The stored settings and the transport agree on the `Debug` redaction even
/// when a caller builds the headers by hand.
#[test]
fn debug_output_never_carries_the_password_or_the_authorization_header() {
    let settings = JianguoyunSettings::official(USER, APP_PASSWORD);
    let shown = format!("{settings:?}");
    assert!(!shown.contains(APP_PASSWORD));
    assert!(shown.contains("[REDACTED]"));

    let request = Request::get(aristotle_net::Url::parse("http://127.0.0.1:1/dav/").unwrap())
        .header(
            "Authorization",
            &settings.to_core_config().basic_auth_header(),
        );
    let shown = format!("{request:?}");
    assert!(!shown.contains(APP_PASSWORD));
    assert!(!shown.contains("Basic "));
}
