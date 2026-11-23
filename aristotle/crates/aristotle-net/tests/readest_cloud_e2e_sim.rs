// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rigorous, adversarial end-to-end simulation test suite for the Readest cloud
//! library and synchronization stack over real loopback TCP (`127.0.0.1:0`).
//!
//! Exercises the real production transport (`aristotle_net::ReadestTransport`),
//! streaming downloader (`aristotle_net::cloud::download_book`), host policy
//! (`aristotle_net::Policy`), cursor persistence (`aristotle_net::store::ReadestSettings`),
//! and core sync/cloud logic (`inkread_core::sync::*`, `inkread_core::cloud::*`)
//! against a **strict upstream-exact Readest mock server** modeled directly on
//! upstream `src/utils/transform.ts`, `src/pages/api/sync.ts`,
//! `src/pages/api/storage/list.ts`, and `src/pages/api/storage/download.ts`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use aristotle_net::cloud::{download_book, DownloadError, MAX_BOOK};
use aristotle_net::http::Error as HttpError;
use aristotle_net::policy::{parse_allow, Policy};
use aristotle_net::store::{CursorKind, ReadestSettings};
use aristotle_net::{ReadestTransport, Url};

use inkread_core::cloud::{fetch_cloud_books_paged, CloudBook, CloudError};
use inkread_core::hash::partial_md5;
use inkread_core::model::{clean_xpointer, Annotation, NoteType};
use inkread_core::sync::local::{
    merge_pulled_notes, tombstone_to_db_book_note, LocalNote, LocalTombstone, READEST_SOURCE,
};
use inkread_core::sync::policy::{plan_progress_update, PositionResolution, ProgressPlan};
use inkread_core::sync::{
    format_iso8601, parse_iso8601, DbBook, DbBookConfig, DbBookNote, HttpRequest, HttpTransport,
    Session, SessionStore, SyncClient, SyncConfig, SyncError, SyncErrorKind, SyncPushRequest,
};

/// Keys that Readest's `POST /api/sync` (`transformsToDB` in `src/utils/transform.ts`)
/// NEVER expects in client payloads; sending snake_case keys causes `undefined`
/// destructuring and Supabase NOT NULL / schema failures (HTTP 500).
const FORBIDDEN_SNAKE_KEYS: &[&str] = &[
    "book_hash",
    "meta_hash",
    "user_id",
    "source_title",
    "group_id",
    "group_name",
    "group_updated_at",
    "reading_status",
    "reading_status_updated_at",
    "cover_hash",
    "cover_updated_at",
    "metadata_updated_at",
    "created_at",
    "updated_at",
    "deleted_at",
    "uploaded_at",
    "synced_at",
    "rsvp_position",
    "search_config",
    "view_settings",
];

// ---------------------------------------------------------------------------
// Strict upstream-exact loopback TCP Readest mock server
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum SyncFault {
    HttpStatus(u16, String),
    MalformedJson,
    DropSocket,
    Auth401,
    Auth403,
}

#[derive(Clone, Debug)]
enum StorageFault {
    DownloadApiStatus(u16, String),
    SignedUrlOverride(String),
    RedirectTo(String),
    BlobStatus(u16, String),
    CorruptBlob,
    ExpiredOnce403,
    SlowChunks { chunk_size: usize, delay_ms: u64 },
}

#[derive(Clone, Debug)]
struct StoredBook {
    user_id: String,
    book_hash: String,
    meta_hash: Option<String>,
    format: String,
    title: String,
    source_title: Option<String>,
    author: String,
    group_id: Option<String>,
    group_name: Option<String>,
    group_updated_at: Option<String>,
    tags: Option<Vec<String>>,
    progress: Option<(u32, u32)>,
    reading_status: Option<String>,
    reading_status_updated_at: Option<String>,
    cover_hash: Option<String>,
    cover_updated_at: Option<String>,
    /// Stored in Supabase as a JSON string (result of `JSON.stringify(metadata)`).
    metadata: Option<String>,
    metadata_updated_at: Option<String>,
    created_at: String,
    updated_at: String,
    deleted_at: Option<String>,
    uploaded_at: Option<String>,
    /// Server-assigned `books_set_synced_at` trigger column.
    synced_at: String,
}

impl StoredBook {
    fn to_db_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("user_id".into(), serde_json::json!(self.user_id));
        obj.insert("book_hash".into(), serde_json::json!(self.book_hash));
        if let Some(ref v) = self.meta_hash {
            obj.insert("meta_hash".into(), serde_json::json!(v));
        }
        obj.insert("format".into(), serde_json::json!(self.format));
        obj.insert("title".into(), serde_json::json!(self.title));
        if let Some(ref v) = self.source_title {
            obj.insert("source_title".into(), serde_json::json!(v));
        }
        obj.insert("author".into(), serde_json::json!(self.author));
        if let Some(ref v) = self.group_id {
            obj.insert("group_id".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.group_name {
            obj.insert("group_name".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.group_updated_at {
            obj.insert("group_updated_at".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.tags {
            obj.insert("tags".into(), serde_json::json!(v));
        }
        if let Some((cur, total)) = self.progress {
            obj.insert("progress".into(), serde_json::json!([cur, total]));
        }
        if let Some(ref v) = self.reading_status {
            obj.insert("reading_status".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.reading_status_updated_at {
            obj.insert("reading_status_updated_at".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.cover_hash {
            obj.insert("cover_hash".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.cover_updated_at {
            obj.insert("cover_updated_at".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.metadata {
            obj.insert("metadata".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.metadata_updated_at {
            obj.insert("metadata_updated_at".into(), serde_json::json!(v));
        }
        obj.insert("created_at".into(), serde_json::json!(self.created_at));
        obj.insert("updated_at".into(), serde_json::json!(self.updated_at));
        obj.insert("deleted_at".into(), serde_json::json!(self.deleted_at));
        obj.insert("uploaded_at".into(), serde_json::json!(self.uploaded_at));
        obj.insert("synced_at".into(), serde_json::json!(self.synced_at));
        serde_json::Value::Object(obj)
    }
}

#[derive(Clone, Debug)]
struct StoredConfig {
    user_id: String,
    book_hash: String,
    meta_hash: Option<String>,
    location: Option<String>,
    xpointer: Option<String>,
    /// Stored in Supabase as a JSON string `"[cur,total]"` (matching `JSON.stringify(progress)`).
    progress: Option<String>,
    rsvp_position: Option<String>,
    search_config: Option<String>,
    view_settings: Option<String>,
    updated_at: String,
    deleted_at: Option<String>,
}

impl StoredConfig {
    fn to_db_json(&self) -> serde_json::Value {
        serde_json::json!({
            "user_id": self.user_id,
            "book_hash": self.book_hash,
            "meta_hash": self.meta_hash,
            "location": self.location,
            "xpointer": self.xpointer,
            "progress": self.progress,
            "rsvp_position": self.rsvp_position,
            "search_config": self.search_config,
            "view_settings": self.view_settings,
            "updated_at": self.updated_at,
            "deleted_at": self.deleted_at,
        })
    }
}

#[derive(Clone, Debug)]
struct StoredNote {
    user_id: String,
    book_hash: String,
    meta_hash: Option<String>,
    id: String,
    kind: String,
    cfi: Option<String>,
    xpointer0: Option<String>,
    xpointer1: Option<String>,
    page: Option<u32>,
    text: Option<String>,
    style: Option<String>,
    color: Option<String>,
    note: Option<String>,
    global: Option<bool>,
    created_at: String,
    updated_at: String,
    deleted_at: Option<String>,
}

impl StoredNote {
    fn to_db_json(&self) -> serde_json::Value {
        serde_json::json!({
            "user_id": self.user_id,
            "book_hash": self.book_hash,
            "meta_hash": self.meta_hash,
            "id": self.id,
            "type": self.kind,
            "cfi": self.cfi,
            "xpointer0": self.xpointer0,
            "xpointer1": self.xpointer1,
            "page": self.page,
            "text": self.text,
            "style": self.style,
            "color": self.color,
            "note": self.note,
            "global": self.global,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "deleted_at": self.deleted_at,
        })
    }
}

#[derive(Clone, Debug)]
struct StoredFile {
    file_key: String,
    book_hash: Option<String>,
    replica_kind: Option<String>,
    replica_id: Option<String>,
    created_at: String,
    updated_at: Option<String>,
    data: Vec<u8>,
}

#[derive(Clone, Debug)]
struct UserAccount {
    password: String,
    user_id: String,
}

struct ServerState {
    port: u16,
    anon_key: String,
    clock_ms: i64,
    token_seq: usize,
    users: HashMap<String, UserAccount>,
    valid_access_tokens: HashMap<String, String>,
    valid_refresh_tokens: HashMap<String, String>,
    books: HashMap<(String, String), StoredBook>,
    configs: HashMap<(String, String), StoredConfig>,
    notes: HashMap<(String, String, String), StoredNote>,
    files: HashMap<String, Vec<StoredFile>>,
    tickets: HashMap<String, Vec<u8>>,
    ticket_seq: usize,
    refresh_status_override: Option<u16>,
    refresh_calls: usize,
    sync_faults: VecDeque<SyncFault>,
    storage_faults: VecDeque<StorageFault>,
    storage_list_page_size_cap: Option<usize>,
    use_cdn_redirect: bool,
    enable_upstream_dummy_book: bool,
    recorded_pushes: Vec<serde_json::Value>,
    recorded_pulls: Vec<String>,
    download_api_hits: usize,
    keep_alive: bool,
    gzip_sync_responses: bool,
    accepted_tcp_connections: usize,
    total_http_requests: usize,
    observed_accept_encoding: Vec<String>,
    gzip_responses_sent: usize,
}

struct StrictReadestServer {
    port: u16,
    state: Arc<Mutex<ServerState>>,
    running: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StrictReadestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let anon_key = SyncConfig::default().supabase_anon_key;

        let mut users = HashMap::new();
        users.insert(
            "reader@example.com".to_string(),
            UserAccount {
                password: "correct-horse-battery".to_string(),
                user_id: "usr-sim-01".to_string(),
            },
        );

        let state = Arc::new(Mutex::new(ServerState {
            port,
            anon_key,
            clock_ms: parse_iso8601("2026-11-10T10:00:00.000Z").unwrap(),
            token_seq: 0,
            users,
            valid_access_tokens: HashMap::new(),
            valid_refresh_tokens: HashMap::new(),
            books: HashMap::new(),
            configs: HashMap::new(),
            notes: HashMap::new(),
            files: HashMap::new(),
            tickets: HashMap::new(),
            ticket_seq: 0,
            refresh_status_override: None,
            refresh_calls: 0,
            sync_faults: VecDeque::new(),
            storage_faults: VecDeque::new(),
            storage_list_page_size_cap: None,
            use_cdn_redirect: false,
            enable_upstream_dummy_book: false,
            recorded_pushes: Vec::new(),
            recorded_pulls: Vec::new(),
            download_api_hits: 0,
            keep_alive: false,
            gzip_sync_responses: false,
            accepted_tcp_connections: 0,
            total_http_requests: 0,
            observed_accept_encoding: Vec::new(),
            gzip_responses_sent: 0,
        }));

        let running = Arc::new(AtomicBool::new(true));
        let srv_state = Arc::clone(&state);
        let srv_running = Arc::clone(&running);

        let handle = thread::spawn(move || {
            while srv_running.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                if !srv_running.load(Ordering::Relaxed) {
                    break;
                }
                srv_state.lock().unwrap().accepted_tcp_connections += 1;
                let st = Arc::clone(&srv_state);
                thread::spawn(move || {
                    handle_connection(stream, st);
                });
            }
        });

        Self {
            port,
            state,
            running,
            handle: Some(handle),
        }
    }

    fn sync_config(&self) -> SyncConfig {
        SyncConfig {
            api_base: format!("http://127.0.0.1:{}/api", self.port),
            supabase_url: format!("http://127.0.0.1:{}", self.port),
            supabase_anon_key: SyncConfig::default().supabase_anon_key,
        }
    }

    fn api_base(&self) -> String {
        format!("http://127.0.0.1:{}/api", self.port)
    }

    fn clock_ms(&self) -> i64 {
        self.state.lock().unwrap().clock_ms
    }

    fn advance_clock_ms(&self, delta: i64) -> i64 {
        let mut st = self.state.lock().unwrap();
        st.clock_ms += delta;
        st.clock_ms
    }

    fn seed_book(&self, book: StoredBook) {
        let mut st = self.state.lock().unwrap();
        st.books
            .insert((book.user_id.clone(), book.book_hash.clone()), book);
    }

    fn get_book(&self, user_id: &str, book_hash: &str) -> Option<StoredBook> {
        self.state
            .lock()
            .unwrap()
            .books
            .get(&(user_id.to_string(), book_hash.to_string()))
            .cloned()
    }

    fn add_storage_file(&self, user_id: &str, file: StoredFile) {
        let mut st = self.state.lock().unwrap();
        st.files.entry(user_id.to_string()).or_default().push(file);
    }

    fn push_sync_fault(&self, fault: SyncFault) {
        self.state.lock().unwrap().sync_faults.push_back(fault);
    }

    fn push_storage_fault(&self, fault: StorageFault) {
        self.state.lock().unwrap().storage_faults.push_back(fault);
    }

    fn set_refresh_status(&self, status: Option<u16>) {
        self.state.lock().unwrap().refresh_status_override = status;
    }

    fn refresh_calls(&self) -> usize {
        self.state.lock().unwrap().refresh_calls
    }

    fn recorded_pushes(&self) -> Vec<serde_json::Value> {
        self.state.lock().unwrap().recorded_pushes.clone()
    }

    fn recorded_pulls(&self) -> Vec<String> {
        self.state.lock().unwrap().recorded_pulls.clone()
    }

    fn clear_recorded(&self) {
        let mut st = self.state.lock().unwrap();
        st.recorded_pushes.clear();
        st.recorded_pulls.clear();
    }

    fn enable_keep_alive(&self, on: bool) {
        self.state.lock().unwrap().keep_alive = on;
    }

    fn enable_gzip_sync(&self, on: bool) {
        self.state.lock().unwrap().gzip_sync_responses = on;
    }

    fn accepted_tcp_connections(&self) -> usize {
        self.state.lock().unwrap().accepted_tcp_connections
    }

    fn total_http_requests(&self) -> usize {
        self.state.lock().unwrap().total_http_requests
    }

    fn observed_accept_encoding(&self) -> Vec<String> {
        self.state.lock().unwrap().observed_accept_encoding.clone()
    }

    fn gzip_responses_sent(&self) -> usize {
        self.state.lock().unwrap().gzip_responses_sent
    }

    fn seed_config(&self, cfg: StoredConfig) {
        let mut st = self.state.lock().unwrap();
        st.configs
            .insert((cfg.user_id.clone(), cfg.book_hash.clone()), cfg);
    }

    fn seed_note(&self, note: StoredNote) {
        let mut st = self.state.lock().unwrap();
        st.notes.insert(
            (
                note.user_id.clone(),
                note.book_hash.clone(),
                note.id.clone(),
            ),
            note,
        );
    }
}

impl Drop for StrictReadestServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(percent_decode(k), percent_decode(v));
        } else {
            map.insert(percent_decode(pair), String::new());
        }
    }
    map
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Pure-Rust RFC 1952 gzip encoder using valid RFC 1951 stored DEFLATE blocks.
fn gzip_compress_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 32);
    // 10-byte gzip header: ID1, ID2, CM=deflate, FLG=0, MTIME=0, XFL=0, OS=unknown
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]);
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(65_535);
            let is_last = offset + chunk_len >= data.len();
            out.push(if is_last { 0x01 } else { 0x00 });
            let len_u16 = chunk_len as u16;
            let nlen_u16 = !len_u16;
            out.extend_from_slice(&len_u16.to_le_bytes());
            out.extend_from_slice(&nlen_u16.to_le_bytes());
            out.extend_from_slice(&data[offset..offset + chunk_len]);
            offset += chunk_len;
        }
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

fn write_http_response_opts(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
    keep_alive: bool,
) {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Response",
    };
    let conn = if keep_alive { "keep-alive" } else { "close" };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: {conn}\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) {
    write_http_response_opts(stream, status, content_type, extra_headers, body, false);
}

fn write_json(stream: &mut TcpStream, status: u16, value: &serde_json::Value) {
    let bytes = serde_json::to_vec(value).unwrap();
    write_http_response_opts(stream, status, "application/json", &[], &bytes, false);
}

fn write_json_conn(
    stream: &mut TcpStream,
    status: u16,
    value: &serde_json::Value,
    keep_alive: bool,
) {
    let bytes = serde_json::to_vec(value).unwrap();
    write_http_response_opts(stream, status, "application/json", &[], &bytes, keep_alive);
}

fn write_sync_json(
    stream: &mut TcpStream,
    status: u16,
    value: &serde_json::Value,
    headers: &[(String, String)],
    st: &mut ServerState,
    keep_alive: bool,
) {
    let raw = serde_json::to_vec(value).unwrap();
    let accepts_gzip = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
        .is_some_and(|(_, v)| v.to_ascii_lowercase().contains("gzip"));
    if st.gzip_sync_responses && accepts_gzip {
        let compressed = gzip_compress_stored(&raw);
        st.gzip_responses_sent += 1;
        write_http_response_opts(
            stream,
            status,
            "application/json",
            &[("Content-Encoding", "gzip")],
            &compressed,
            keep_alive,
        );
    } else {
        write_http_response_opts(stream, status, "application/json", &[], &raw, keep_alive);
    }
}

fn parse_ts_field(val: Option<&serde_json::Value>, default_ms: i64) -> Result<String, String> {
    match val {
        None | Some(serde_json::Value::Null) => Ok(format_iso8601(default_ms)),
        Some(serde_json::Value::String(s)) => {
            parse_iso8601(s).map_err(|_| format!("invalid ISO-8601 timestamp: {s}"))?;
            Ok(s.clone())
        }
        Some(serde_json::Value::Number(n)) => {
            let ms = n
                .as_i64()
                .ok_or_else(|| format!("invalid numeric timestamp: {n}"))?;
            Ok(format_iso8601(ms))
        }
        Some(other) => Err(format!("invalid timestamp type: {other}")),
    }
}

fn parse_opt_ts_field(val: Option<&serde_json::Value>) -> Result<Option<String>, String> {
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => Ok(Some(parse_ts_field(Some(v), 0)?)),
    }
}

fn ts_ms(ts: Option<&str>) -> i64 {
    ts.and_then(|s| parse_iso8601(s).ok()).unwrap_or(0)
}

fn validate_no_snake_case(
    obj: &serde_json::Map<String, serde_json::Value>,
    section: &str,
) -> Result<(), String> {
    for &bad in FORBIDDEN_SNAKE_KEYS {
        if obj.contains_key(bad) {
            return Err(format!(
                "strict schema violation: snake_case key '{bad}' in POST /api/sync {section}[]"
            ));
        }
    }
    Ok(())
}

fn validate_json_object_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    section: &str,
) -> Result<Option<String>, String> {
    match obj.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v @ serde_json::Value::Object(_)) => Ok(Some(serde_json::to_string(v).unwrap())),
        Some(other) => Err(format!(
            "strict schema violation: {section}[].{key} must be a JSON object (not stringified): got {other}"
        )),
    }
}

fn validate_progress_pair(
    val: Option<&serde_json::Value>,
    field_desc: &str,
) -> Result<Option<(u32, u32)>, String> {
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(arr)) if arr.len() == 2 => {
            let a = arr[0]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| format!("{field_desc}[0] must be u32 integer"))?;
            let b = arr[1]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| format!("{field_desc}[1] must be u32 integer"))?;
            Ok(Some((a, b)))
        }
        Some(other) => Err(format!(
            "strict schema violation: {field_desc} must be a 2-element integer array [cur, total], got {other}"
        )),
    }
}

fn bearer_user(headers: &[(String, String)], st: &ServerState) -> Option<String> {
    let auth = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.as_str())?;
    let token = auth.strip_prefix("Bearer ")?;
    st.valid_access_tokens.get(token).cloned()
}

fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<ServerState>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    loop {
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        };

        let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let mut lines = head.lines();
        let Some(req_line) = lines.next() else {
            return;
        };
        let mut parts = req_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();

        let mut content_length = 0usize;
        let mut headers = Vec::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim().to_string();
                let val = v.trim().to_string();
                if key.eq_ignore_ascii_case("content-length") {
                    content_length = val.parse().unwrap_or(0);
                }
                headers.push((key, val));
            }
        }

        let body_start = header_end + 4;
        let total_needed = body_start + content_length;
        while buf.len() < total_needed {
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        let body_end = total_needed.min(buf.len());
        let body = buf[body_start..body_end].to_vec();
        buf.drain(..body_end);

        let client_wants_close = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("connection") && v.to_ascii_lowercase().contains("close")
        });
        let use_keep_alive = {
            let mut st = state.lock().unwrap();
            st.total_http_requests += 1;
            st.keep_alive && !client_wants_close
        };

        let (path, raw_query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        let query = parse_query(raw_query);

        // 1. Supabase GoTrue auth endpoint
        if method == "POST" && path == "/auth/v1/token" {
            handle_auth(&mut stream, &headers, &query, &body, &state, use_keep_alive);
            if !use_keep_alive {
                return;
            }
            continue;
        }

        // 2. Sync endpoint (GET / POST /api/sync)
        if path == "/api/sync" {
            let fault = state.lock().unwrap().sync_faults.pop_front();
            if let Some(f) = fault {
                match f {
                    SyncFault::DropSocket => {
                        // Close immediately without writing any bytes.
                        return;
                    }
                    SyncFault::MalformedJson => {
                        write_http_response(
                            &mut stream,
                            200,
                            "application/json",
                            &[],
                            b"{not-valid-json",
                        );
                        return;
                    }
                    SyncFault::HttpStatus(code, msg) => {
                        write_json(&mut stream, code, &serde_json::json!({ "error": msg }));
                        return;
                    }
                    SyncFault::Auth401 => {
                        write_json(
                            &mut stream,
                            401,
                            &serde_json::json!({ "error": "JWT expired" }),
                        );
                        return;
                    }
                    SyncFault::Auth403 => {
                        write_json(
                            &mut stream,
                            403,
                            &serde_json::json!({ "error": "Not authenticated" }),
                        );
                        return;
                    }
                }
            }
            match method.as_str() {
                "GET" => handle_sync_get(
                    &mut stream,
                    &headers,
                    raw_query,
                    &query,
                    &state,
                    use_keep_alive,
                ),
                "POST" => handle_sync_post(&mut stream, &headers, &body, &state, use_keep_alive),
                _ => {
                    write_json(
                        &mut stream,
                        405,
                        &serde_json::json!({ "error": "Method Not Allowed" }),
                    );
                    return;
                }
            }
            if !use_keep_alive {
                return;
            }
            continue;
        }

        // 3. Storage list endpoint
        if method == "GET" && path == "/api/storage/list" {
            handle_storage_list(&mut stream, &headers, &query, &state);
            return;
        }

        // 4. Storage download ticket endpoint
        if method == "GET" && path == "/api/storage/download" {
            handle_storage_download_ticket(&mut stream, &headers, &query, &state);
            return;
        }

        // 5. CDN redirect hop
        if method == "GET" && path.starts_with("/cdn/redirect/") {
            let ticket = path.trim_start_matches("/cdn/redirect/");
            let location = format!("/cdn/blob/{ticket}");
            write_http_response(
                &mut stream,
                302,
                "text/plain",
                &[("Location", &location)],
                b"",
            );
            return;
        }

        // 6. CDN blob streaming endpoint
        if method == "GET" && path.starts_with("/cdn/blob/") {
            handle_cdn_blob(&mut stream, path, &state);
            return;
        }

        write_json(
            &mut stream,
            404,
            &serde_json::json!({ "error": "Not found" }),
        );
        return;
    }
}

fn handle_auth(
    stream: &mut TcpStream,
    headers: &[(String, String)],
    query: &HashMap<String, String>,
    body: &[u8],
    state: &Arc<Mutex<ServerState>>,
    keep_alive: bool,
) {
    let mut st = state.lock().unwrap();
    let apikey = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("apikey"))
        .map(|(_, v)| v.as_str());
    if apikey != Some(st.anon_key.as_str()) {
        write_json_conn(
            stream,
            401,
            &serde_json::json!({ "error": "invalid apikey" }),
            keep_alive,
        );
        return;
    }

    let grant = query.get("grant_type").map(String::as_str).unwrap_or("");
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            write_json_conn(
                stream,
                400,
                &serde_json::json!({ "error": "bad json" }),
                keep_alive,
            );
            return;
        }
    };

    match grant {
        "password" => {
            let email = parsed.get("email").and_then(|v| v.as_str()).unwrap_or("");
            let password = parsed
                .get("password")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let Some(user) = st.users.get(email).cloned() else {
                write_json_conn(
                    stream,
                    400,
                    &serde_json::json!({ "error": "invalid_grant" }),
                    keep_alive,
                );
                return;
            };
            if user.password != password {
                write_json_conn(
                    stream,
                    400,
                    &serde_json::json!({ "error": "invalid_grant" }),
                    keep_alive,
                );
                return;
            }
            st.token_seq += 1;
            let acc = format!("acc-{}", st.token_seq);
            let rt = format!("ref-{}", st.token_seq);
            st.valid_access_tokens
                .insert(acc.clone(), user.user_id.clone());
            st.valid_refresh_tokens.insert(rt.clone(), user.user_id);
            write_json_conn(
                stream,
                200,
                &serde_json::json!({
                    "access_token": acc,
                    "refresh_token": rt,
                    "expires_in": 3600,
                }),
                keep_alive,
            );
        }
        "refresh_token" => {
            st.refresh_calls += 1;
            if let Some(code) = st.refresh_status_override {
                write_json_conn(
                    stream,
                    code,
                    &serde_json::json!({ "error": format!("injected refresh {code}") }),
                    keep_alive,
                );
                return;
            }
            let rt = parsed
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Rotating refresh token: removing it ensures spending the same token twice fails!
            let Some(user_id) = st.valid_refresh_tokens.remove(rt) else {
                write_json_conn(
                    stream,
                    401,
                    &serde_json::json!({ "error": "invalid_grant", "error_description": "refresh token already used or revoked" }),
                    keep_alive,
                );
                return;
            };
            st.token_seq += 1;
            let acc = format!("acc-{}", st.token_seq);
            let next_rt = format!("ref-{}", st.token_seq);
            st.valid_access_tokens.insert(acc.clone(), user_id.clone());
            st.valid_refresh_tokens.insert(next_rt.clone(), user_id);
            write_json_conn(
                stream,
                200,
                &serde_json::json!({
                    "access_token": acc,
                    "refresh_token": next_rt,
                    "expires_in": 3600,
                }),
                keep_alive,
            );
        }
        _ => write_json_conn(
            stream,
            400,
            &serde_json::json!({ "error": "unsupported_grant_type" }),
            keep_alive,
        ),
    }
}

fn handle_sync_get(
    stream: &mut TcpStream,
    headers: &[(String, String)],
    raw_query: &str,
    query: &HashMap<String, String>,
    state: &Arc<Mutex<ServerState>>,
    keep_alive: bool,
) {
    let mut st = state.lock().unwrap();
    let ae = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    st.observed_accept_encoding.push(ae);

    let Some(user_id) = bearer_user(headers, &st) else {
        write_json_conn(
            stream,
            403,
            &serde_json::json!({ "error": "Not authenticated" }),
            keep_alive,
        );
        return;
    };
    st.recorded_pulls.push(raw_query.to_string());

    let Some(since_str) = query.get("since") else {
        write_json_conn(
            stream,
            400,
            &serde_json::json!({ "error": "\"since\" query parameter is required" }),
            keep_alive,
        );
        return;
    };
    let Ok(since_ms) = since_str.parse::<i64>() else {
        write_json_conn(
            stream,
            400,
            &serde_json::json!({ "error": "Invalid \"since\" timestamp" }),
            keep_alive,
        );
        return;
    };

    let sync_type = query.get("type").map(String::as_str);
    let book_filter = query.get("book").map(String::as_str);
    let limit: usize = query
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    let mut books_out: Vec<serde_json::Value> = Vec::new();
    let mut configs_out: Vec<serde_json::Value> = Vec::new();
    let mut notes_out: Vec<serde_json::Value> = Vec::new();

    if sync_type.is_none() || sync_type == Some("books") {
        let mut matched: Vec<StoredBook> = st
            .books
            .values()
            .filter(|b| b.user_id == user_id)
            .filter(|b| book_filter.is_none_or(|bf| b.book_hash == bf))
            .filter(|b| ts_ms(Some(&b.synced_at)) > since_ms)
            .cloned()
            .collect();

        if limit > 0 && sync_type == Some("books") {
            // Exact `fetchPagedBooks` (`sync.ts:383-408`): ascending by `synced_at`,
            // completed to the trailing `synced_at` millisecond tie.
            matched.sort_by(|a, b| {
                ts_ms(Some(&a.synced_at))
                    .cmp(&ts_ms(Some(&b.synced_at)))
                    .then_with(|| a.book_hash.cmp(&b.book_hash))
            });
            if matched.len() > limit {
                let boundary_synced = matched[limit - 1].synced_at.clone();
                let mut end = limit;
                while end < matched.len() && matched[end].synced_at == boundary_synced {
                    end += 1;
                }
                matched.truncate(end);
            }
        } else {
            matched.sort_by_key(|a| std::cmp::Reverse(ts_ms(Some(&a.synced_at))));
        }

        if matched.is_empty() && since_ms < 1000 {
            if st.enable_upstream_dummy_book {
                // Exact upstream `sync.ts:416-434` payload!
                let now = st.clock_ms;
                let dummy = "00000000000000000000000000000000";
                books_out.push(serde_json::json!({
                    "user_id": user_id,
                    "id": dummy,
                    "book_hash": dummy,
                    "deleted_at": now,
                    "updated_at": now,
                    "hash": dummy,
                    "title": "Dummy Book",
                    "format": "EPUB",
                    "author": "",
                    "createdAt": now,
                    "updatedAt": now,
                    "deletedAt": now,
                }));
            } else {
                let dummy = "00000000000000000000000000000000";
                books_out.push(serde_json::json!({
                    "user_id": user_id,
                    "book_hash": dummy,
                    "title": "Dummy Book",
                    "format": "EPUB",
                    "author": "",
                    "deleted_at": format_iso8601(st.clock_ms),
                    "updated_at": format_iso8601(st.clock_ms),
                }));
            }
        } else {
            books_out = matched.iter().map(StoredBook::to_db_json).collect();
        }
    }

    if sync_type.is_none() || sync_type == Some("configs") {
        let mut matched: Vec<StoredConfig> = st
            .configs
            .values()
            .filter(|c| c.user_id == user_id)
            .filter(|c| book_filter.is_none_or(|bf| c.book_hash == bf))
            .filter(|c| {
                ts_ms(Some(&c.updated_at)) > since_ms || ts_ms(c.deleted_at.as_deref()) > since_ms
            })
            .cloned()
            .collect();
        matched.sort_by_key(|a| std::cmp::Reverse(ts_ms(Some(&a.updated_at))));
        configs_out = matched.iter().map(StoredConfig::to_db_json).collect();
    }

    if sync_type.is_none() || sync_type == Some("notes") {
        let mut matched: Vec<StoredNote> = st
            .notes
            .values()
            .filter(|n| n.user_id == user_id)
            .filter(|n| book_filter.is_none_or(|bf| n.book_hash == bf))
            .filter(|n| {
                ts_ms(Some(&n.updated_at)) > since_ms || ts_ms(n.deleted_at.as_deref()) > since_ms
            })
            .cloned()
            .collect();
        matched.sort_by_key(|a| std::cmp::Reverse(ts_ms(Some(&a.updated_at))));
        notes_out = matched.iter().map(StoredNote::to_db_json).collect();
    }

    write_sync_json(
        stream,
        200,
        &serde_json::json!({
            "books": books_out,
            "configs": configs_out,
            "notes": notes_out,
            "statBooks": [],
            "statPages": [],
        }),
        headers,
        &mut st,
        keep_alive,
    );
}

fn handle_sync_post(
    stream: &mut TcpStream,
    headers: &[(String, String)],
    body: &[u8],
    state: &Arc<Mutex<ServerState>>,
    keep_alive: bool,
) {
    let mut st = state.lock().unwrap();
    let ae = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    st.observed_accept_encoding.push(ae);

    let Some(user_id) = bearer_user(headers, &st) else {
        write_json_conn(
            stream,
            403,
            &serde_json::json!({ "error": "Not authenticated" }),
            keep_alive,
        );
        return;
    };

    let payload: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            write_json(
                stream,
                400,
                &serde_json::json!({ "error": format!("invalid JSON: {e}") }),
            );
            return;
        }
    };
    st.recorded_pushes.push(payload.clone());

    let now_ms = st.clock_ms;
    let now_iso = format_iso8601(now_ms);

    let mut auth_books: Vec<serde_json::Value> = Vec::new();
    let mut auth_configs: Vec<StoredConfig> = Vec::new();
    let mut auth_notes: Vec<serde_json::Value> = Vec::new();

    // --- 1. Validate & upsert books[] (exact transformBookToDB semantics) ---
    if let Some(books) = payload.get("books").and_then(|v| v.as_array()) {
        for raw in books {
            let Some(obj) = raw.as_object() else {
                write_json(
                    stream,
                    500,
                    &serde_json::json!({ "error": "books[] element must be object" }),
                );
                return;
            };
            if let Err(err) = validate_no_snake_case(obj, "books") {
                write_json(stream, 500, &serde_json::json!({ "error": err }));
                return;
            }
            let Some(hash) = obj
                .get("hash")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                write_json(
                    stream,
                    500,
                    &serde_json::json!({ "error": "null value in column \"book_hash\" violates not-null constraint (missing camelCase 'hash')" }),
                );
                return;
            };
            let metadata = match validate_json_object_field(obj, "metadata", "books") {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let progress = match validate_progress_pair(obj.get("progress"), "books[].progress") {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let created_at = match parse_ts_field(obj.get("createdAt"), now_ms) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let updated_at = match parse_ts_field(obj.get("updatedAt"), now_ms) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let deleted_at = match parse_opt_ts_field(obj.get("deletedAt")) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            // Exact `transformBookToDB`: uploaded_at = uploadedAt ? ISO : null
            let uploaded_at = match parse_opt_ts_field(obj.get("uploadedAt")) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };

            let mut candidate = StoredBook {
                user_id: user_id.clone(),
                book_hash: hash.to_string(),
                meta_hash: obj
                    .get("metaHash")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                format: obj
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("EPUB")
                    .to_string(),
                title: obj
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source_title: obj
                    .get("sourceTitle")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                author: obj
                    .get("author")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                group_id: obj
                    .get("groupId")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                group_name: obj
                    .get("groupName")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                group_updated_at: parse_opt_ts_field(obj.get("groupUpdatedAt")).unwrap_or(None),
                tags: obj.get("tags").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                }),
                progress,
                reading_status: obj
                    .get("readingStatus")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                reading_status_updated_at: parse_opt_ts_field(obj.get("readingStatusUpdatedAt"))
                    .unwrap_or(None),
                cover_hash: obj
                    .get("coverHash")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                cover_updated_at: parse_opt_ts_field(obj.get("coverUpdatedAt")).unwrap_or(None),
                metadata,
                metadata_updated_at: parse_opt_ts_field(obj.get("metadataUpdatedAt"))
                    .unwrap_or(None),
                created_at,
                updated_at,
                deleted_at,
                uploaded_at,
                synced_at: now_iso.clone(),
            };

            let key = (user_id.clone(), hash.to_string());
            let authoritative = match st.books.get(&key).cloned() {
                None => {
                    // `sync.ts:726`: new insert stamps updated_at = now
                    candidate.updated_at = now_iso.clone();
                    st.books.insert(key, candidate.clone());
                    candidate
                }
                Some(server_row) => {
                    let client_newer = ts_ms(candidate.deleted_at.as_deref())
                        > ts_ms(server_row.deleted_at.as_deref())
                        || ts_ms(Some(&candidate.updated_at)) > ts_ms(Some(&server_row.updated_at));
                    if client_newer {
                        st.books.insert(key, candidate.clone());
                        candidate
                    } else {
                        server_row
                    }
                }
            };
            auth_books.push(authoritative.to_db_json());
        }
    }

    // --- 2. Validate & upsert configs[] (exact transformBookConfigToDB semantics) ---
    if let Some(configs) = payload.get("configs").and_then(|v| v.as_array()) {
        for raw in configs {
            let Some(obj) = raw.as_object() else {
                write_json(
                    stream,
                    500,
                    &serde_json::json!({ "error": "configs[] element must be object" }),
                );
                return;
            };
            if let Err(err) = validate_no_snake_case(obj, "configs") {
                write_json(stream, 500, &serde_json::json!({ "error": err }));
                return;
            }
            let Some(book_hash) = obj
                .get("bookHash")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                write_json(
                    stream,
                    500,
                    &serde_json::json!({ "error": "null value in column \"book_hash\" (missing camelCase 'bookHash')" }),
                );
                return;
            };
            let progress_pair =
                match validate_progress_pair(obj.get("progress"), "configs[].progress") {
                    Ok(v) => v,
                    Err(err) => {
                        write_json(stream, 500, &serde_json::json!({ "error": err }));
                        return;
                    }
                };
            let view_settings = match validate_json_object_field(obj, "viewSettings", "configs") {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let rsvp_position = match validate_json_object_field(obj, "rsvpPosition", "configs") {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let search_config = match validate_json_object_field(obj, "searchConfig", "configs") {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let updated_at = match parse_ts_field(obj.get("updatedAt"), now_ms) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let deleted_at = match parse_opt_ts_field(obj.get("deletedAt")) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };

            let mut candidate = StoredConfig {
                user_id: user_id.clone(),
                book_hash: book_hash.to_string(),
                meta_hash: obj
                    .get("metaHash")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                location: obj
                    .get("location")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                xpointer: obj
                    .get("xpointer")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                // `transformBookConfigToDB` runs `JSON.stringify(progress)`
                progress: progress_pair.map(|(a, b)| format!("[{a},{b}]")),
                rsvp_position,
                search_config,
                view_settings,
                updated_at,
                deleted_at,
            };

            let key = (user_id.clone(), book_hash.to_string());
            let authoritative = match st.configs.get(&key).cloned() {
                None => {
                    candidate.updated_at = now_iso.clone();
                    st.configs.insert(key, candidate.clone());
                    candidate
                }
                Some(server_row) => {
                    let client_newer = ts_ms(candidate.deleted_at.as_deref())
                        > ts_ms(server_row.deleted_at.as_deref())
                        || ts_ms(Some(&candidate.updated_at)) > ts_ms(Some(&server_row.updated_at));
                    if client_newer {
                        st.configs.insert(key, candidate.clone());
                        candidate
                    } else {
                        server_row
                    }
                }
            };
            auth_configs.push(authoritative);
        }
    }

    // --- 3. Validate & upsert notes[] (exact transformBookNoteToDB semantics) ---
    if let Some(notes) = payload.get("notes").and_then(|v| v.as_array()) {
        for raw in notes {
            let Some(obj) = raw.as_object() else {
                write_json(
                    stream,
                    500,
                    &serde_json::json!({ "error": "notes[] element must be object" }),
                );
                return;
            };
            if let Err(err) = validate_no_snake_case(obj, "notes") {
                write_json(stream, 500, &serde_json::json!({ "error": err }));
                return;
            }
            let Some(book_hash) = obj
                .get("bookHash")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                write_json(
                    stream,
                    500,
                    &serde_json::json!({ "error": "null value in column \"book_hash\" in notes[]" }),
                );
                return;
            };
            let Some(id) = obj
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                write_json(
                    stream,
                    500,
                    &serde_json::json!({ "error": "null value in column \"id\" in notes[]" }),
                );
                return;
            };
            let created_at = match parse_ts_field(obj.get("createdAt"), now_ms) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let updated_at = match parse_ts_field(obj.get("updatedAt"), now_ms) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };
            let deleted_at = match parse_opt_ts_field(obj.get("deletedAt")) {
                Ok(v) => v,
                Err(err) => {
                    write_json(stream, 500, &serde_json::json!({ "error": err }));
                    return;
                }
            };

            let mut candidate = StoredNote {
                user_id: user_id.clone(),
                book_hash: book_hash.to_string(),
                meta_hash: obj
                    .get("metaHash")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                id: id.to_string(),
                kind: obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("annotation")
                    .to_string(),
                cfi: obj.get("cfi").and_then(|v| v.as_str()).map(String::from),
                xpointer0: obj
                    .get("xpointer0")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                xpointer1: obj
                    .get("xpointer1")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                page: obj
                    .get("page")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| u32::try_from(n).ok()),
                text: obj.get("text").and_then(|v| v.as_str()).map(String::from),
                style: obj.get("style").and_then(|v| v.as_str()).map(String::from),
                color: obj.get("color").and_then(|v| v.as_str()).map(String::from),
                note: obj.get("note").and_then(|v| v.as_str()).map(String::from),
                global: obj.get("global").and_then(|v| v.as_bool()),
                created_at,
                updated_at,
                deleted_at,
            };

            let key = (user_id.clone(), book_hash.to_string(), id.to_string());
            let authoritative = match st.notes.get(&key).cloned() {
                None => {
                    candidate.updated_at = now_iso.clone();
                    st.notes.insert(key, candidate.clone());
                    candidate
                }
                Some(server_row) => {
                    let client_newer = ts_ms(candidate.deleted_at.as_deref())
                        > ts_ms(server_row.deleted_at.as_deref())
                        || ts_ms(Some(&candidate.updated_at)) > ts_ms(Some(&server_row.updated_at));
                    if client_newer {
                        st.notes.insert(key, candidate.clone());
                        candidate
                    } else {
                        server_row
                    }
                }
            };
            auth_notes.push(authoritative.to_db_json());
        }
    }

    // --- 4. Exact `sync.ts:893-933` piggyback: configs[].progress -> books.progress ---
    for cfg in &auth_configs {
        let Some(ref prog_str) = cfg.progress else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<[u32; 2]>(prog_str) else {
            continue;
        };
        let cfg_updated_ms = ts_ms(Some(&cfg.updated_at));
        if let Some(book) = st.books.get_mut(&(user_id.clone(), cfg.book_hash.clone())) {
            if ts_ms(Some(&book.updated_at)) < cfg_updated_ms {
                book.progress = Some((parsed[0], parsed[1]));
                book.updated_at = cfg.updated_at.clone();
                book.synced_at = now_iso.clone();
                // Crucially, `book.uploaded_at` is untouched!
            }
        }
    }

    let configs_json: Vec<serde_json::Value> =
        auth_configs.iter().map(StoredConfig::to_db_json).collect();

    write_sync_json(
        stream,
        200,
        &serde_json::json!({
            "books": auth_books,
            "configs": configs_json,
            "notes": auth_notes,
        }),
        headers,
        &mut st,
        keep_alive,
    );
}

fn handle_storage_list(
    stream: &mut TcpStream,
    headers: &[(String, String)],
    query: &HashMap<String, String>,
    state: &Arc<Mutex<ServerState>>,
) {
    let st = state.lock().unwrap();
    let Some(user_id) = bearer_user(headers, &st) else {
        write_json(
            stream,
            403,
            &serde_json::json!({ "error": "Not authenticated" }),
        );
        return;
    };

    let book_hash = query.get("bookHash").map(String::as_str);
    let page: usize = query
        .get("page")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let requested_size: usize = query
        .get("pageSize")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50)
        .max(1);
    let page_size = st
        .storage_list_page_size_cap
        .map_or(requested_size, |cap| requested_size.min(cap));

    let all: Vec<&StoredFile> = st
        .files
        .get(&user_id)
        .map(|v| {
            v.iter()
                .filter(|f| book_hash.is_none_or(|bh| f.book_hash.as_deref() == Some(bh)))
                .collect()
        })
        .unwrap_or_default();

    let total = all.len();
    let total_pages = if total == 0 {
        0
    } else {
        total.div_ceil(page_size)
    };
    let start = (page - 1).saturating_mul(page_size);
    let slice = if start < total {
        &all[start..(start + page_size).min(total)]
    } else {
        &[]
    };

    let files_json: Vec<serde_json::Value> = slice
        .iter()
        .map(|f| {
            serde_json::json!({
                "file_key": f.file_key,
                "file_size": f.data.len(),
                "book_hash": f.book_hash,
                "replica_kind": f.replica_kind,
                "replica_id": f.replica_id,
                "created_at": f.created_at,
                "updated_at": f.updated_at,
            })
        })
        .collect();

    write_json(
        stream,
        200,
        &serde_json::json!({
            "files": files_json,
            "total": total,
            "page": page,
            "pageSize": page_size,
            "totalPages": total_pages,
        }),
    );
}

fn handle_storage_download_ticket(
    stream: &mut TcpStream,
    headers: &[(String, String)],
    query: &HashMap<String, String>,
    state: &Arc<Mutex<ServerState>>,
) {
    let mut st = state.lock().unwrap();
    st.download_api_hits += 1;
    let Some(user_id) = bearer_user(headers, &st) else {
        write_json(
            stream,
            403,
            &serde_json::json!({ "error": "Not authenticated" }),
        );
        return;
    };

    if let Some(StorageFault::DownloadApiStatus(code, msg)) = st.storage_faults.front().cloned() {
        st.storage_faults.pop_front();
        write_json(stream, code, &serde_json::json!({ "error": msg }));
        return;
    }
    if let Some(StorageFault::SignedUrlOverride(url)) = st.storage_faults.front().cloned() {
        st.storage_faults.pop_front();
        write_json(stream, 200, &serde_json::json!({ "downloadUrl": url }));
        return;
    }

    let Some(file_key) = query.get("fileKey") else {
        write_json(
            stream,
            400,
            &serde_json::json!({ "error": "Missing or invalid fileKey" }),
        );
        return;
    };

    let data = st.files.get(&user_id).and_then(|files| {
        files
            .iter()
            .find(|f| f.file_key == *file_key)
            .map(|f| f.data.clone())
    });

    let Some(bytes) = data else {
        write_json(
            stream,
            404,
            &serde_json::json!({ "error": "File not found" }),
        );
        return;
    };

    st.ticket_seq += 1;
    let ticket_id = format!("tkt-{}", st.ticket_seq);
    st.tickets.insert(ticket_id.clone(), bytes);

    let port = st.port;
    let download_url = if st.use_cdn_redirect {
        format!("http://127.0.0.1:{port}/cdn/redirect/{ticket_id}")
    } else {
        format!("http://127.0.0.1:{port}/cdn/blob/{ticket_id}")
    };

    write_json(
        stream,
        200,
        &serde_json::json!({ "downloadUrl": download_url }),
    );
}

fn handle_cdn_blob(stream: &mut TcpStream, path: &str, state: &Arc<Mutex<ServerState>>) {
    let ticket_id = path.trim_start_matches("/cdn/blob/");
    let (data_opt, fault) = {
        let mut st = state.lock().unwrap();
        let data = st.tickets.get(ticket_id).cloned();
        let f = st.storage_faults.pop_front();
        (data, f)
    };

    if let Some(f) = fault {
        match f {
            StorageFault::ExpiredOnce403 => {
                write_http_response(stream, 403, "text/plain", &[], b"RequestExpired");
                return;
            }
            StorageFault::BlobStatus(code, msg) => {
                write_http_response(stream, code, "text/plain", &[], msg.as_bytes());
                return;
            }
            StorageFault::RedirectTo(loc) => {
                write_http_response(stream, 302, "text/plain", &[("Location", &loc)], b"");
                return;
            }
            StorageFault::CorruptBlob => {
                let bad = b"corrupted-epub-bytes-wrong-hash";
                write_http_response(stream, 200, "application/epub+zip", &[], bad);
                return;
            }
            StorageFault::SlowChunks {
                chunk_size,
                delay_ms,
            } => {
                if let Some(bytes) = data_opt {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    for chunk in bytes.chunks(chunk_size.max(1)) {
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                        let _ = stream.flush();
                        thread::sleep(Duration::from_millis(delay_ms));
                    }
                    return;
                }
            }
            _ => {}
        }
    }

    match data_opt {
        Some(bytes) => {
            write_http_response(stream, 200, "application/epub+zip", &[], &bytes);
        }
        None => {
            write_http_response(stream, 404, "text/plain", &[], b"ticket not found");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "aristotle-readest-e2e-{label}-{}-{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct BookSeed<'a> {
    user_id: &'a str,
    book_hash: &'a str,
    title: &'a str,
    author: &'a str,
    updated_ms: i64,
    synced_ms: i64,
    uploaded_at: Option<i64>,
    deleted_at: Option<i64>,
    progress: Option<(u32, u32)>,
}

fn make_stored_book(seed: BookSeed<'_>) -> StoredBook {
    let ts = format_iso8601(seed.updated_ms);
    StoredBook {
        user_id: seed.user_id.to_string(),
        book_hash: seed.book_hash.to_string(),
        meta_hash: None,
        format: "EPUB".to_string(),
        title: seed.title.to_string(),
        source_title: None,
        author: seed.author.to_string(),
        group_id: None,
        group_name: None,
        group_updated_at: None,
        tags: None,
        progress: seed.progress,
        reading_status: None,
        reading_status_updated_at: None,
        cover_hash: None,
        cover_updated_at: None,
        metadata: None,
        metadata_updated_at: None,
        created_at: ts.clone(),
        updated_at: ts,
        deleted_at: seed.deleted_at.map(format_iso8601),
        uploaded_at: seed.uploaded_at.map(format_iso8601),
        synced_at: format_iso8601(seed.synced_ms),
    }
}

fn new_client(server: &StrictReadestServer) -> SyncClient<ReadestTransport> {
    SyncClient::new(ReadestTransport::new(Policy::open()), server.sync_config())
}

// ---------------------------------------------------------------------------
// Test 0: Strict wire-contract self-check against upstream transform.ts
// ---------------------------------------------------------------------------

#[test]
fn strict_server_rejects_non_upstream_wire_payloads() {
    let server = StrictReadestServer::start();
    let client = new_client(&server);
    let session = client
        .login("reader@example.com", "correct-horse-battery")
        .expect("login");

    let transport = ReadestTransport::new(Policy::open());
    let sync_url = format!("{}/sync", server.api_base());

    let post_raw = |body: serde_json::Value| {
        let req = HttpRequest::new("POST", &sync_url)
            .with_header("Authorization", format!("Bearer {}", session.access_token))
            .with_header("Content-Type", "application/json")
            .with_body(serde_json::to_vec(&body).unwrap());
        transport.send(req).unwrap()
    };

    // 1. Snake_case keys in books[], configs[], notes[] MUST fail with HTTP 500
    for bad_payload in [
        serde_json::json!({ "books": [{ "book_hash": "abc", "title": "T", "author": "A", "format": "EPUB" }] }),
        serde_json::json!({ "books": [{ "hash": "abc", "title": "T", "author": "A", "format": "EPUB", "uploaded_at": "2026-11-10T10:00:00.000Z" }] }),
        serde_json::json!({ "configs": [{ "book_hash": "abc", "updated_at": "2026-11-10T10:00:00.000Z" }] }),
        serde_json::json!({ "notes": [{ "book_hash": "abc", "id": "n1", "type": "bookmark", "created_at": "2026-11-10T10:00:00.000Z" }] }),
        // 2. Stringified configs[].progress MUST fail with HTTP 500
        serde_json::json!({ "configs": [{ "bookHash": "abc", "progress": "[10, 100]" }] }),
        // 3. Double-stringified JSON object fields MUST fail with HTTP 500
        serde_json::json!({ "books": [{ "hash": "abc", "title": "T", "author": "A", "format": "EPUB", "metadata": "{\"desc\":\"x\"}" }] }),
        serde_json::json!({ "configs": [{ "bookHash": "abc", "viewSettings": "{\"fontSize\":16}" }] }),
        serde_json::json!({ "configs": [{ "bookHash": "abc", "rsvpPosition": "{\"index\":5}" }] }),
        serde_json::json!({ "configs": [{ "bookHash": "abc", "searchConfig": "{\"matchCase\":true}" }] }),
    ] {
        let resp = post_raw(bad_payload.clone());
        assert_eq!(
            resp.status, 500,
            "strict server MUST reject non-upstream payload {bad_payload}"
        );
    }

    server.clear_recorded();

    // 4. Real SyncClient::push MUST serialize compliant camelCase + native JSON arrays/objects
    let ts = format_iso8601(server.clock_ms());
    let push_req = SyncPushRequest {
        books: vec![DbBook {
            book_hash: "wirebook01".into(),
            meta_hash: Some("meta01".into()),
            format: "EPUB".into(),
            title: "Wire Test".into(),
            author: "Author".into(),
            metadata: Some(r#"{"description":"nested object"}"#.into()),
            uploaded_at: Some(ts.clone()),
            created_at: Some(ts.clone()),
            updated_at: Some(ts.clone()),
            ..Default::default()
        }],
        configs: vec![DbBookConfig {
            book_hash: "wirebook01".into(),
            xpointer: Some("/body/DocFragment[1]/p[1].0".into()),
            progress: Some("[12, 100]".into()),
            view_settings: Some(r#"{"fontSize":18}"#.into()),
            rsvp_position: Some(r#"{"wordIndex":4}"#.into()),
            search_config: Some(r#"{"matchCase":false}"#.into()),
            updated_at: Some(ts.clone()),
            ..Default::default()
        }],
        notes: vec![DbBookNote {
            book_hash: "wirebook01".into(),
            id: "note-wire-1".into(),
            kind: "annotation".into(),
            xpointer0: Some("/body/DocFragment[1]/p[1].0".into()),
            xpointer1: Some("/body/DocFragment[1]/p[1].12".into()),
            text: Some("wire check".into()),
            created_at: Some(ts.clone()),
            updated_at: Some(ts),
            ..Default::default()
        }],
        ..Default::default()
    };

    let push_resp = client
        .push(push_req, None)
        .expect("compliant push succeeds");
    assert_eq!(push_resp.books.len(), 1);
    assert_eq!(push_resp.configs.len(), 1);
    assert_eq!(push_resp.notes.len(), 1);
    // Response returned snake_case DB rows with stringified progress and deserialized cleanly
    assert_eq!(push_resp.configs[0].progress.as_deref(), Some("[12,100]"));

    let recorded = server.recorded_pushes();
    assert_eq!(recorded.len(), 1);
    let sent = &recorded[0];
    assert_eq!(sent["books"][0]["hash"], "wirebook01");
    assert!(sent["books"][0]["metadata"].is_object());
    assert_eq!(sent["configs"][0]["bookHash"], "wirebook01");
    assert_eq!(sent["configs"][0]["progress"], serde_json::json!([12, 100]));
    assert!(sent["configs"][0]["viewSettings"].is_object());
    assert!(sent["configs"][0]["rsvpPosition"].is_object());
    assert!(sent["configs"][0]["searchConfig"].is_object());
    assert_eq!(sent["notes"][0]["bookHash"], "wirebook01");
}

// ---------------------------------------------------------------------------
// Scenario 1: Login & Token Refresh
// ---------------------------------------------------------------------------

#[test]
fn scenario_1_login_and_token_refresh_lifecycle() {
    let server = StrictReadestServer::start();
    let client = new_client(&server);

    // 1. Invalid credentials yield AuthFailed
    let bad_err = client
        .login("reader@example.com", "wrong-password")
        .expect_err("wrong password must fail");
    assert_eq!(bad_err.kind(), SyncErrorKind::AuthFailed);
    assert!(client.session().is_none());

    // 2. Valid password login
    let s1 = client
        .login("reader@example.com", "correct-horse-battery")
        .expect("valid login");
    assert!(s1.access_token.starts_with("acc-"));
    assert!(s1.refresh_token.starts_with("ref-"));

    // 3. Server rejects access token with HTTP 401 -> auto refresh -> retry succeeds!
    server.push_sync_fault(SyncFault::Auth401);
    let pull_after_401 = client.pull(Some("notes"), 0).expect("retry after 401");
    assert!(pull_after_401.notes.is_empty());
    assert_eq!(server.refresh_calls(), 1);
    let s2 = client.session().expect("refreshed session");
    assert_ne!(s2.access_token, s1.access_token);
    assert_ne!(
        s2.refresh_token, s1.refresh_token,
        "rotating refresh token must be updated"
    );

    // 4. Server rejects with HTTP 403 (upstream sync.ts unauthenticated status) -> auto refresh -> retry succeeds!
    server.push_sync_fault(SyncFault::Auth403);
    client.pull(Some("notes"), 0).expect("retry after 403");
    assert_eq!(server.refresh_calls(), 2);

    // 5. Concurrent requests sharing SessionStore spend a rotating refresh token ONCE
    let shared_store = SessionStore::with_session(Some(Session::new(
        "expired-access",
        client.session().unwrap().refresh_token,
        0, // force proactive refresh
    )));
    let c1 = SyncClient::with_session_store(
        ReadestTransport::new(Policy::open()),
        server.sync_config(),
        shared_store.clone(),
    );
    let c2 = SyncClient::with_session_store(
        ReadestTransport::new(Policy::open()),
        server.sync_config(),
        shared_store.clone(),
    );
    let before_concurrent = server.refresh_calls();
    let t1 = thread::spawn(move || c1.pull(Some("notes"), 0));
    let t2 = thread::spawn(move || c2.pull(Some("notes"), 0));
    t1.join().unwrap().expect("thread 1 pull");
    t2.join().unwrap().expect("thread 2 pull");
    assert_eq!(
        server.refresh_calls() - before_concurrent,
        1,
        "shared SessionStore must spend rotating refresh token exactly once"
    );
    client.set_session(shared_store.current());

    // 6. Refresh returns HTTP 500 -> returns non-AuthExpired error AND preserves session!
    let pre_500 = client.session().unwrap();
    client.set_session(Some(Session::new(
        pre_500.access_token.clone(),
        pre_500.refresh_token.clone(),
        0,
    )));
    server.set_refresh_status(Some(500));
    let err_500 = client
        .pull(Some("notes"), 0)
        .expect_err("refresh 500 must fail");
    assert!(!matches!(err_500, SyncError::AuthExpired(_)), "{err_500:?}");
    let preserved = client
        .session()
        .expect("refresh 500 MUST preserve session and not sign user out");
    assert_eq!(preserved.refresh_token, pre_500.refresh_token);

    // 7. Refresh returns HTTP 401 -> returns AuthExpired and clears session!
    server.set_refresh_status(Some(401));
    let err_401 = client
        .pull(Some("notes"), 0)
        .expect_err("refresh 401 must fail with AuthExpired");
    assert!(matches!(err_401, SyncError::AuthExpired(_)), "{err_401:?}");
    assert_eq!(err_401.kind(), SyncErrorKind::AuthExpired);
    assert!(
        client.session().is_none(),
        "refresh 401 MUST clear dead session"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: Cloud Library Browse & Upload Preservation
// ---------------------------------------------------------------------------

#[test]
fn scenario_2_cloud_library_browse_and_upload_preservation() {
    let server = StrictReadestServer::start();
    let client = new_client(&server);
    client
        .login("reader@example.com", "correct-horse-battery")
        .expect("login");

    let base = server.clock_ms();
    // Seed 7 books:
    // - 5 active uploaded books (3 of them share the exact same `synced_at` ms to test page-boundary tie completion!)
    // - 1 un-uploaded book (`uploaded_at: None`)
    // - 1 deleted uploaded book (`deleted_at: Some(...)`)
    let uploaded_books = [
        (
            "aa1101",
            "Dune",
            "Frank Herbert",
            base + 1_000,
            base + 1_000,
        ),
        (
            "aa1102",
            "Hyperion",
            "Dan Simmons",
            base + 2_000,
            base + 2_000,
        ),
        (
            "aa1103",
            "Foundation",
            "Isaac Asimov",
            base + 3_000,
            base + 2_000, // same synced_at ms as Hyperion!
        ),
        (
            "aa1104",
            "Snow Crash",
            "Neal Stephenson",
            base + 4_000,
            base + 2_000, // same synced_at ms as Hyperion & Foundation!
        ),
        (
            "aa1105",
            "Neuromancer",
            "William Gibson",
            base + 5_000,
            base + 5_000,
        ),
    ];
    for (hash, title, author, upd, sync_ts) in uploaded_books {
        server.seed_book(make_stored_book(BookSeed {
            user_id: "usr-sim-01",
            book_hash: hash,
            title,
            author,
            updated_ms: upd,
            synced_ms: sync_ts,
            uploaded_at: Some(base),
            deleted_at: None,
            progress: Some((10, 100)),
        }));
    }
    // Un-uploaded book: must be excluded from cloud library
    server.seed_book(make_stored_book(BookSeed {
        user_id: "usr-sim-01",
        book_hash: "bb2201",
        title: "Local Only Draft",
        author: "Author",
        updated_ms: base + 6_000,
        synced_ms: base + 6_000,
        uploaded_at: None,
        deleted_at: None,
        progress: None,
    }));
    // Deleted uploaded book: must be excluded from cloud library
    server.seed_book(make_stored_book(BookSeed {
        user_id: "usr-sim-01",
        book_hash: "cc3301",
        title: "Deleted Book",
        author: "Author",
        updated_ms: base + 7_000,
        synced_ms: base + 7_000,
        uploaded_at: Some(base),
        deleted_at: Some(base + 7_000),
        progress: None,
    }));

    // Local library has AA1101 (uppercase to also test case-insensitive hash matching)
    let mut local_hashes = HashSet::new();
    local_hashes.insert("AA1101".to_string());

    // Fetch paged with small page_size=2 (splits directly across the 3-book synced_at tie!)
    let cloud = fetch_cloud_books_paged(&client, None, &local_hashes, 2)
        .expect("paginated cloud library fetch");
    assert_eq!(
        cloud.len(),
        5,
        "only uploaded_at.is_some() && deleted_at.is_none() books must appear"
    );
    // Sorted by updated_at DESC
    let titles: Vec<&str> = cloud.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(
        titles,
        vec![
            "Neuromancer",
            "Snow Crash",
            "Foundation",
            "Hyperion",
            "Dune"
        ]
    );
    let dune = cloud.iter().find(|b| b.book_hash == "aa1101").unwrap();
    assert!(dune.downloaded, "case-insensitive local hash must match");
    assert_eq!(dune.progress, Some((10, 100)));

    // Push config + note for Dune with EMPTY `books: []`
    let t_push = server.advance_clock_ms(10_000);
    let push_req = SyncPushRequest {
        books: vec![],
        configs: vec![DbBookConfig {
            book_hash: "aa1101".into(),
            xpointer: Some("/body/DocFragment[4]/p[2].0".into()),
            progress: Some("[42, 120]".into()),
            updated_at: Some(format_iso8601(t_push)),
            ..Default::default()
        }],
        notes: vec![DbBookNote {
            book_hash: "aa1101".into(),
            id: "dune-hl-1".into(),
            kind: "annotation".into(),
            xpointer0: Some("/body/DocFragment[4]/p[2].0".into()),
            xpointer1: Some("/body/DocFragment[4]/p[2].20".into()),
            text: Some("Fear is the mind-killer".into()),
            created_at: Some(format_iso8601(t_push)),
            updated_at: Some(format_iso8601(t_push)),
            ..Default::default()
        }],
        ..Default::default()
    };
    client
        .push(push_req, None)
        .expect("push configs+notes only");

    // Verify `uploaded_at` survived AND piggyback updated `books.progress` to [42, 120]!
    let stored_dune = server.get_book("usr-sim-01", "aa1101").unwrap();
    assert!(
        stored_dune.uploaded_at.is_some(),
        "empty books[] push MUST preserve uploaded_at"
    );
    assert_eq!(
        stored_dune.progress,
        Some((42, 120)),
        "server piggyback MUST update books.progress from configs[].progress"
    );

    let refreshed =
        fetch_cloud_books_paged(&client, None, &local_hashes, 2).expect("re-browse cloud library");
    assert_eq!(refreshed.len(), 5, "cloud library must not lose any books");
    // Dune's updated_at jumped to t_push via piggyback, so it is now first in recency!
    assert_eq!(refreshed[0].book_hash, "aa1101");
    assert_eq!(refreshed[0].progress, Some((42, 120)));

    // Adversarial verification: pushing a book row WITHOUT uploaded_at wipes uploaded_at on real Readest!
    let t_wipe = server.advance_clock_ms(5_000);
    client
        .push(
            SyncPushRequest {
                books: vec![DbBook {
                    book_hash: "aa1102".into(),
                    title: "Hyperion".into(),
                    author: "Dan Simmons".into(),
                    format: "EPUB".into(),
                    uploaded_at: None, // simulates a broken client pushing book metadata without uploadedAt
                    updated_at: Some(format_iso8601(t_wipe)),
                    ..Default::default()
                }],
                ..Default::default()
            },
            None,
        )
        .expect("push book without uploadedAt");
    let after_wipe = fetch_cloud_books_paged(&client, None, &local_hashes, 2).unwrap();
    assert!(
        !after_wipe.iter().any(|b| b.book_hash == "aa1102"),
        "strict server confirms transformBookToDB nulls uploaded_at when omitted"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: Cloud Book Download + Streaming + Cancellation + Faults
// ---------------------------------------------------------------------------

#[test]
fn scenario_3_cloud_book_download_streaming_cancel_and_faults() {
    let server = StrictReadestServer::start();
    let client = new_client(&server);
    let session = client
        .login("reader@example.com", "correct-horse-battery")
        .expect("login");

    // Create a ~192 KiB synthetic EPUB so streaming invokes multiple 64 KiB read chunks
    let epub_bytes: Vec<u8> = (0..(192 * 1024))
        .map(|i| ((i * 31 + 17) % 251) as u8)
        .collect();
    let book_hash = partial_md5(&mut Cursor::new(epub_bytes.clone())).unwrap();
    let real_file_key = format!("usr-sim-01/Readest/Books/{book_hash}/Original Source Name.epub");
    let ts = format_iso8601(server.clock_ms());

    // Populate multi-page `/api/storage/list` with cover, nav sidecar, replica, and the real EPUB on page 2
    {
        let mut st = server.state.lock().unwrap();
        st.storage_list_page_size_cap = Some(2);
        st.use_cdn_redirect = true;
    }
    server.add_storage_file(
        "usr-sim-01",
        StoredFile {
            file_key: format!("usr-sim-01/Readest/Books/{book_hash}/cover.png"),
            book_hash: Some(book_hash.clone()),
            replica_kind: None,
            replica_id: None,
            created_at: ts.clone(),
            updated_at: Some(ts.clone()),
            data: b"PNG".to_vec(),
        },
    );
    server.add_storage_file(
        "usr-sim-01",
        StoredFile {
            file_key: format!("usr-sim-01/Readest/Books/{book_hash}/nav.json"),
            book_hash: Some(book_hash.clone()),
            replica_kind: None,
            replica_id: None,
            created_at: ts.clone(),
            updated_at: Some(ts.clone()),
            data: b"{}".to_vec(),
        },
    );
    server.add_storage_file(
        "usr-sim-01",
        StoredFile {
            file_key: format!("usr-sim-01/Readest/Books/{book_hash}/Replicas/ios/copy.epub"),
            book_hash: Some(book_hash.clone()),
            replica_kind: Some("ios".into()),
            replica_id: Some("rep-1".into()),
            created_at: ts.clone(),
            updated_at: Some(ts.clone()),
            data: b"replica-not-main".to_vec(),
        },
    );
    server.add_storage_file(
        "usr-sim-01",
        StoredFile {
            file_key: real_file_key.clone(),
            book_hash: Some(book_hash.clone()),
            replica_kind: None,
            replica_id: None,
            created_at: ts.clone(),
            updated_at: Some(ts),
            data: epub_bytes.clone(),
        },
    );

    let book = CloudBook {
        book_hash: book_hash.clone(),
        title: "Streaming Novel".into(),
        source_title: None,
        author: "Tester".into(),
        format: "EPUB".into(),
        progress: None,
        cover_hash: None,
        uploaded_at: Some(format_iso8601(server.clock_ms())),
        deleted_at: None,
        // Intentionally different from `real_file_key` so we prove `/api/storage/list` resolution won!
        file_key: format!("usr-sim-01/Readest/Books/{book_hash}/wrong-fallback.epub"),
        downloaded: false,
        meta_hash: None,
        updated_at: None,
    };

    // 3a. Happy path: storage list page 1+2 -> signed download URL -> 302 CDN redirect ->
    //     expired ticket 403 -> automatic ticket renewal -> streaming download + exact hash check!
    server.push_storage_fault(StorageFault::ExpiredOnce403);
    let dir_ok = scratch_dir("cloud-dl-ok");
    let mut progress_events: Vec<(u64, Option<u64>)> = Vec::new();
    let final_path = download_book(
        Policy::open(),
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &book,
        &dir_ok,
        MAX_BOOK,
        &AtomicBool::new(false),
        &mut |bytes, total| progress_events.push((bytes, total)),
    )
    .expect("streaming download after redirect & ticket renewal");

    assert_eq!(fs::read(&final_path).unwrap(), epub_bytes);
    assert!(
        !progress_events.is_empty(),
        "progress callbacks must be emitted"
    );
    for w in progress_events.windows(2) {
        assert!(
            w[1].0 >= w[0].0,
            "progress bytes must be monotonically non-decreasing: {:?}",
            progress_events
        );
    }
    assert_eq!(
        progress_events.last().copied(),
        Some((epub_bytes.len() as u64, Some(epub_bytes.len() as u64)))
    );
    let entries: Vec<String> = fs::read_dir(&dir_ok)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        entries,
        vec!["Streaming Novel.epub".to_string()],
        "no .part file may remain after rename"
    );

    // 3b. Deterministic mid-stream cancellation leaves ZERO files on disk
    server.push_storage_fault(StorageFault::SlowChunks {
        chunk_size: 16 * 1024,
        delay_ms: 10,
    });
    let dir_cancel = scratch_dir("cloud-dl-cancel");
    let cancel = AtomicBool::new(false);
    let mut chunks_seen = 0usize;
    let cancel_err = download_book(
        Policy::open(),
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &book,
        &dir_cancel,
        MAX_BOOK,
        &cancel,
        &mut |bytes, _| {
            chunks_seen += 1;
            if bytes > 0 {
                cancel.store(true, Ordering::SeqCst);
            }
        },
    )
    .expect_err("cancelled download must return Cancelled");
    assert!(
        matches!(cancel_err, DownloadError::Http(HttpError::Cancelled)),
        "{cancel_err:?}"
    );
    assert!(chunks_seen >= 1);
    assert_eq!(
        fs::read_dir(&dir_cancel).unwrap().count(),
        0,
        "cancellation MUST remove partial .part file and leave no corrupt file"
    );

    // 3c. HTTP 404 from storage download ticket endpoint
    server.push_storage_fault(StorageFault::DownloadApiStatus(
        404,
        "File not found".into(),
    ));
    let dir_faults = scratch_dir("cloud-dl-faults");
    let err_404 = download_book(
        Policy::open(),
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &book,
        &dir_faults,
        MAX_BOOK,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .expect_err("404 ticket");
    assert!(
        matches!(
            err_404,
            DownloadError::Cloud(CloudError::HttpStatus(404, _))
        ),
        "{err_404:?}"
    );
    assert_eq!(fs::read_dir(&dir_faults).unwrap().count(), 0);

    // 3d. HTTP 500 from CDN blob endpoint
    server.push_storage_fault(StorageFault::BlobStatus(500, "R2 internal error".into()));
    let err_500 = download_book(
        Policy::open(),
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &book,
        &dir_faults,
        MAX_BOOK,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .expect_err("500 blob");
    assert!(
        matches!(err_500, DownloadError::Http(HttpError::Status(500, _))),
        "{err_500:?}"
    );
    assert_eq!(fs::read_dir(&dir_faults).unwrap().count(), 0);

    // 3e. Corrupted bytes fail hash check and clean up .part
    server.push_storage_fault(StorageFault::CorruptBlob);
    let err_hash = download_book(
        Policy::open(),
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &book,
        &dir_faults,
        MAX_BOOK,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .expect_err("corrupt blob");
    assert!(
        matches!(
            err_hash,
            DownloadError::Cloud(CloudError::HashMismatch { .. })
        ),
        "{err_hash:?}"
    );
    assert_eq!(fs::read_dir(&dir_faults).unwrap().count(), 0);

    // 3f. Insecure public plain-HTTP signed URL is refused before connecting
    server.push_storage_fault(StorageFault::SignedUrlOverride(
        "http://public-cdn.example.com/evil.epub".into(),
    ));
    let err_insecure = download_book(
        Policy::open(),
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &book,
        &dir_faults,
        MAX_BOOK,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .expect_err("insecure url");
    assert!(matches!(err_insecure, DownloadError::InsecureUrl));

    // 3g. Scoped policy blocks redirect to an unallowed host:port cleanly
    let mut scoped_policy = Policy {
        scoped: true,
        allowed: parse_allow(&format!("127.0.0.1:{}", server.port)),
    };
    scoped_policy.allow(&Url::parse(&format!("http://127.0.0.1:{}/", server.port)).unwrap());
    server.push_storage_fault(StorageFault::RedirectTo(
        "http://127.0.0.1:1/blocked-cdn.epub".into(),
    ));
    let err_policy = download_book(
        scoped_policy,
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &book,
        &dir_faults,
        MAX_BOOK,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .expect_err("policy block on redirect");
    assert!(
        matches!(err_policy, DownloadError::Http(HttpError::NotAllowed(_))),
        "{err_policy:?}"
    );
    assert_eq!(fs::read_dir(&dir_faults).unwrap().count(), 0);

    // 3h. Unsafe book_hash refused before any HTTP request
    let hits_before = server.state.lock().unwrap().download_api_hits;
    let mut bad_book = book.clone();
    bad_book.book_hash = "../escape".into();
    let err_unsafe = download_book(
        Policy::open(),
        &server.api_base(),
        &session.access_token,
        "usr-sim-01",
        &bad_book,
        &dir_faults,
        MAX_BOOK,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .expect_err("unsafe hash");
    assert!(matches!(err_unsafe, DownloadError::UnsafeHash));
    assert_eq!(server.state.lock().unwrap().download_api_hits, hits_before);

    let _ = fs::remove_dir_all(&dir_ok);
    let _ = fs::remove_dir_all(&dir_cancel);
    let _ = fs::remove_dir_all(&dir_faults);
}

// ---------------------------------------------------------------------------
// Scenario 4: Auto Sync Progress, Bookmarks, Highlights & Clock Skew
// ---------------------------------------------------------------------------

#[test]
fn scenario_4_auto_sync_progress_bookmarks_highlights_and_clock_skew() {
    let server = StrictReadestServer::start();
    let book_hash = "epub4444444444444444444444444444";
    let t0 = server.clock_ms();

    // 4a. clean_xpointer strips `xpointer:` and rejects non-crengine locators
    assert_eq!(
        clean_xpointer("xpointer:/body/DocFragment[3]/body/p[7]/text().12"),
        "/body/DocFragment[3]/body/p[7]/text().12"
    );
    assert_eq!(
        clean_xpointer("  xpointer: /body/DocFragment[1]/p[1].0  "),
        "/body/DocFragment[1]/p[1].0"
    );
    for invalid in [
        "plato:3:120",
        "epub:3:120",
        "kindle:4500",
        "page:12",
        "fraction:0.42",
        "txt2:xpointer:/body/DocFragment[1]/p[1].0",
        "xpointer:plato:3:120",
        "xpointer:kindle:99",
        "xpointer:page:5",
        "",
    ] {
        assert_eq!(
            clean_xpointer(invalid),
            "",
            "must reject non-crengine locator {invalid:?}"
        );
    }

    // Seed initial uploaded book on server
    server.seed_book(make_stored_book(BookSeed {
        user_id: "usr-sim-01",
        book_hash,
        title: "Auto Sync Novel",
        author: "Author",
        updated_ms: t0,
        synced_ms: t0,
        uploaded_at: Some(t0),
        deleted_at: None,
        progress: Some((1, 200)),
    }));

    // Create a Kindle client whose wall clock is **+10 minutes AHEAD** of server!
    let skewed_clock = Arc::new(AtomicI64::new(t0 + 10 * 60 * 1000));
    let clock_for_client = Arc::clone(&skewed_clock);
    let kindle =
        new_client(&server).with_time_provider(move || clock_for_client.load(Ordering::Relaxed));
    let phone = new_client(&server);

    kindle
        .login("reader@example.com", "correct-horse-battery")
        .expect("kindle login");
    phone
        .login("reader@example.com", "correct-horse-battery")
        .expect("phone login");

    // 4b. Kindle creates highlight + bookmark with `xpointer:` prefix & a note with `plato:` locator
    let mut kindle_notes = vec![
        LocalNote {
            id: "kindle-hl-1".into(),
            kind: NoteType::Annotation,
            position: Some((
                "xpointer:/body/DocFragment[2]/body/p[4]/text().0".into(),
                "xpointer:/body/DocFragment[2]/body/p[4]/text().42".into(),
            )),
            text: Some("Original kindle highlight".into()),
            note: Some("First thought".into()),
            color: Some("yellow".into()),
            style: Some("highlight".into()),
            page: Some("18".into()),
            location: Some((18, 200)),
            chapter: Some("Chapter 2".into()),
            source: "aristotle".into(),
            created_at: t0,
            updated_at: t0,
        },
        LocalNote {
            id: "kindle-bm-1".into(),
            kind: NoteType::Bookmark,
            position: Some((
                "xpointer:/body/DocFragment[2]/body/p[9]/text().0".into(),
                String::new(),
            )),
            text: None,
            note: None,
            color: None,
            style: None,
            page: Some("19".into()),
            location: Some((19, 200)),
            chapter: Some("Chapter 2".into()),
            source: "aristotle".into(),
            created_at: t0,
            updated_at: t0,
        },
    ];
    let bad_locator_ann = Annotation {
        id: Some("bad-loc-note".into()),
        kind: NoteType::Annotation,
        position: Some(("plato:2:40".into(), "kindle:900".into())),
        color: Some("blue".into()),
        style: Some("highlight".into()),
        text: Some("Text without crengine xp".into()),
        note: None,
        created_at: Some(t0),
        updated_at: Some(t0),
        deleted_at: None,
    };

    server.clear_recorded();
    let push_notes: Vec<DbBookNote> = kindle_notes
        .iter()
        .map(|n| n.to_db_book_note(book_hash))
        .chain(std::iter::once(
            bad_locator_ann.to_db_book_note(book_hash, "bad-loc-note"),
        ))
        .collect();
    kindle
        .push(
            SyncPushRequest {
                notes: push_notes,
                ..Default::default()
            },
            None,
        )
        .expect("kindle push notes");

    // Inspect raw wire JSON received by server: pure `/body/...` XPointers (no `xpointer:` prefix!)
    // and camelCase keys, and non-crengine locators omitted!
    let pushes = server.recorded_pushes();
    assert_eq!(pushes.len(), 1);
    let wire_notes = pushes[0]["notes"].as_array().unwrap();
    let w_hl = wire_notes
        .iter()
        .find(|n| n["id"] == "kindle-hl-1")
        .unwrap();
    assert_eq!(w_hl["bookHash"], book_hash);
    assert_eq!(w_hl["xpointer0"], "/body/DocFragment[2]/body/p[4]/text().0");
    assert_eq!(
        w_hl["xpointer1"],
        "/body/DocFragment[2]/body/p[4]/text().42"
    );
    assert!(!w_hl["xpointer0"].as_str().unwrap().starts_with("xpointer:"));
    let w_bm = wire_notes
        .iter()
        .find(|n| n["id"] == "kindle-bm-1")
        .unwrap();
    assert_eq!(w_bm["type"], "bookmark");
    assert_eq!(w_bm["xpointer0"], "/body/DocFragment[2]/body/p[9]/text().0");
    assert!(w_bm.get("xpointer1").is_none());
    let w_bad = wire_notes
        .iter()
        .find(|n| n["id"] == "bad-loc-note")
        .unwrap();
    assert!(
        w_bad.get("xpointer0").is_none() && w_bad.get("xpointer1").is_none(),
        "non-crengine locators must NEVER be pushed as xpointer0/1"
    );

    // 4c. Pull with skewed +10m client clock -> cursors MUST track server timestamps, NOT client clock!
    let mut settings =
        ReadestSettings::after_login("reader@example.com", &kindle.session().unwrap(), None);
    let (_, books_cur) = kindle
        .pull_all_books_with_cursor(settings.cursor(CursorKind::Books), 50)
        .expect("initial books pull");
    let notes_pull = kindle
        .pull(Some("notes"), settings.cursor(CursorKind::Notes))
        .expect("initial notes pull");
    settings.advance_cursor(CursorKind::Books, books_cur);
    settings.advance_cursor(CursorKind::Notes, notes_pull.notes_cursor());
    assert_eq!(
        settings.cursor(CursorKind::Books),
        t0,
        "books cursor must equal server synced_at, never +10m skewed client clock"
    );
    assert_eq!(
        settings.cursor(CursorKind::Notes),
        t0,
        "notes cursor must equal server updated_at, never +10m skewed client clock"
    );

    // 4d. Phone edits Kindle's `source == "aristotle"` highlight AND deletes Kindle's bookmark
    //     AND advances progress at server time `t0 + 5_000` (still 9m55s BEHIND Kindle's skewed clock!)
    let t1 = server.advance_clock_ms(5_000);
    assert!(
        t1 < kindle.now_ms(),
        "sanity check: server t1 is behind Kindle's +10m skewed clock"
    );
    phone
        .push(
            SyncPushRequest {
                configs: vec![DbBookConfig {
                    book_hash: book_hash.into(),
                    xpointer: Some("/body/DocFragment[6]/body/p[1]/text().0".into()),
                    location: Some("epubcfi(/6/12!/4/2/1:0)".into()),
                    progress: Some("[60, 200]".into()),
                    updated_at: Some(format_iso8601(t1)),
                    ..Default::default()
                }],
                notes: vec![
                    DbBookNote {
                        book_hash: book_hash.into(),
                        id: "kindle-hl-1".into(),
                        kind: "annotation".into(),
                        xpointer0: Some("/body/DocFragment[2]/body/p[4]/text().0".into()),
                        xpointer1: Some("/body/DocFragment[2]/body/p[4]/text().42".into()),
                        text: Some("Edited on phone".into()),
                        note: Some("Updated commentary".into()),
                        color: Some("green".into()),
                        style: Some("underline".into()),
                        created_at: Some(format_iso8601(t0)),
                        updated_at: Some(format_iso8601(t1)),
                        deleted_at: None,
                        ..Default::default()
                    },
                    DbBookNote {
                        book_hash: book_hash.into(),
                        id: "kindle-bm-1".into(),
                        kind: "bookmark".into(),
                        updated_at: Some(format_iso8601(t1)),
                        deleted_at: Some(format_iso8601(t1)),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            None,
        )
        .expect("phone push remote edit + delete + progress");

    // Kindle pulls using stored cursors (must send `since={t0}`, NOT skewed clock!)
    server.clear_recorded();
    let pulled_cfgs = kindle
        .pull(Some("configs"), settings.cursor(CursorKind::Configs))
        .expect("kindle pull configs");
    let pulled_notes = kindle
        .pull(Some("notes"), settings.cursor(CursorKind::Notes))
        .expect("kindle pull notes");
    let pulls = server.recorded_pulls();
    assert!(
        pulls.iter().any(|q| q.contains(&format!("since={t0}"))),
        "notes pull must query server cursor t0={t0}, got {pulls:?}"
    );

    // Advance cursors from server response only
    settings.advance_cursor(CursorKind::Configs, pulled_cfgs.configs_cursor());
    settings.advance_cursor(CursorKind::Notes, pulled_notes.notes_cursor());
    assert_eq!(settings.cursor(CursorKind::Configs), t1);
    assert_eq!(settings.cursor(CursorKind::Notes), t1);

    // Verify remote progress resolution on open book
    assert_eq!(pulled_cfgs.configs.len(), 1);
    assert_eq!(
        plan_progress_update(t0, &pulled_cfgs.configs[0], true, false),
        ProgressPlan::Offer(PositionResolution::NavigateXPointer(
            "/body/DocFragment[6]/body/p[1]/text().0".into()
        ))
    );
    // CFI-only config must offer NeedsManualPosition, never jump by progress fraction
    let cfi_only = DbBookConfig {
        xpointer: None,
        ..pulled_cfgs.configs[0].clone()
    };
    assert_eq!(
        plan_progress_update(t0, &cfi_only, true, false),
        ProgressPlan::Offer(PositionResolution::NeedsManualPosition)
    );

    // Verify `merge_pulled_notes` applies BOTH the remote edit AND remote deletion tombstone
    // on Kindle-created (`source == "aristotle"`) notes!
    let merged = merge_pulled_notes(book_hash, &kindle_notes, &[], &pulled_notes.notes);
    assert_eq!(
        merged.notes.len(),
        1,
        "kindle-bm-1 (source=aristotle) MUST be removed by remote tombstone"
    );
    assert_eq!(merged.tombstones.len(), 1);
    assert_eq!(merged.tombstones[0].id, "kindle-bm-1");

    let updated_hl = &merged.notes[0];
    assert_eq!(updated_hl.id, "kindle-hl-1");
    assert_eq!(updated_hl.text.as_deref(), Some("Edited on phone"));
    assert_eq!(updated_hl.note.as_deref(), Some("Updated commentary"));
    assert_eq!(updated_hl.color.as_deref(), Some("green"));
    assert_eq!(updated_hl.style.as_deref(), Some("underline"));
    assert_eq!(
        updated_hl.source, "aristotle",
        "must preserve original syncable source tag"
    );
    assert_eq!(
        updated_hl.page.as_deref(),
        Some("18"),
        "must keep local-only page metadata"
    );
    assert_eq!(updated_hl.chapter.as_deref(), Some("Chapter 2"));
    // Because clean_xpointer matched, original prefixed position is preserved cleanly
    assert_eq!(
        updated_hl.position,
        Some((
            "xpointer:/body/DocFragment[2]/body/p[4]/text().0".into(),
            "xpointer:/body/DocFragment[2]/body/p[4]/text().42".into(),
        ))
    );

    // An empty pull must NOT move cursor forward to skewed client clock!
    let empty_pull = kindle
        .pull(Some("notes"), settings.cursor(CursorKind::Notes))
        .expect("empty pull");
    assert!(empty_pull.notes.is_empty());
    assert_eq!(empty_pull.notes_cursor(), None);
    settings.advance_cursor(CursorKind::Notes, empty_pull.notes_cursor());
    assert_eq!(settings.cursor(CursorKind::Notes), t1);

    // Update local list to verify clean state
    kindle_notes = merged.notes;
    assert_eq!(kindle_notes.len(), 1);
}

// ---------------------------------------------------------------------------
// Scenario 5: Manual Sync Push & Pull + Multi-Entry Progress Consistency
// ---------------------------------------------------------------------------

struct SimulatedDevice {
    client: SyncClient<ReadestTransport>,
    settings: ReadestSettings,
    xpointer: Option<String>,
    progress: Option<(u32, u32)>,
    progress_updated_ms: i64,
    notes: Vec<LocalNote>,
    tombstones: Vec<LocalTombstone>,
}

impl SimulatedDevice {
    fn new(server: &StrictReadestServer) -> Self {
        let client = new_client(server);
        let session = client
            .login("reader@example.com", "correct-horse-battery")
            .expect("device login");
        let settings = ReadestSettings::after_login("reader@example.com", &session, None);
        Self {
            client,
            settings,
            xpointer: None,
            progress: None,
            progress_updated_ms: 0,
            notes: Vec::new(),
            tombstones: Vec::new(),
        }
    }

    fn push_book_state(&self, book_hash: &str) {
        let mut notes: Vec<DbBookNote> = self
            .notes
            .iter()
            .map(|n| n.to_db_book_note(book_hash))
            .collect();
        notes.extend(
            self.tombstones
                .iter()
                .map(|t| tombstone_to_db_book_note(book_hash, t)),
        );
        let configs = if self.progress_updated_ms > 0 {
            vec![DbBookConfig {
                book_hash: book_hash.into(),
                xpointer: self.xpointer.clone(),
                progress: self.progress.map(|(a, b)| format!("[{a}, {b}]")),
                updated_at: Some(format_iso8601(self.progress_updated_ms)),
                ..Default::default()
            }]
        } else {
            vec![]
        };
        // Never push books[] during progress/note sync so `uploaded_at` is preserved!
        self.client
            .push(
                SyncPushRequest {
                    books: vec![],
                    configs,
                    notes,
                    ..Default::default()
                },
                None,
            )
            .expect("device push");
    }

    fn pull_and_merge(&mut self, book_hash: &str) -> Vec<DbBook> {
        let (books, b_cur) = self
            .client
            .pull_all_books_with_cursor(self.settings.cursor(CursorKind::Books), 50)
            .expect("pull books");
        let cfg_resp = self
            .client
            .pull(Some("configs"), self.settings.cursor(CursorKind::Configs))
            .expect("pull configs");
        let note_resp = self
            .client
            .pull(Some("notes"), self.settings.cursor(CursorKind::Notes))
            .expect("pull notes");

        self.settings.advance_cursor(CursorKind::Books, b_cur);
        self.settings
            .advance_cursor(CursorKind::Configs, cfg_resp.configs_cursor());
        self.settings
            .advance_cursor(CursorKind::Notes, note_resp.notes_cursor());

        for cfg in &cfg_resp.configs {
            if cfg.book_hash == book_hash {
                if let ProgressPlan::Offer(PositionResolution::NavigateXPointer(xp)) =
                    plan_progress_update(self.progress_updated_ms, cfg, true, false)
                {
                    let m = cfg.to_model();
                    self.xpointer = Some(xp);
                    self.progress = m.progress;
                    self.progress_updated_ms = m.updated_at;
                }
            }
        }

        let merged = merge_pulled_notes(book_hash, &self.notes, &self.tombstones, &note_resp.notes);
        self.notes = merged.notes;
        self.tombstones = merged.tombstones;
        books
    }
}

#[test]
fn scenario_5_manual_sync_and_two_device_convergence() {
    let server = StrictReadestServer::start();
    let book_hash = "conv5555555555555555555555555555";
    let t0 = server.clock_ms();

    server.seed_book(make_stored_book(BookSeed {
        user_id: "usr-sim-01",
        book_hash,
        title: "Convergence Classic",
        author: "Author",
        updated_ms: t0,
        synced_ms: t0,
        uploaded_at: Some(t0),
        deleted_at: None,
        progress: Some((1, 100)),
    }));

    let mut dev_a = SimulatedDevice::new(&server);
    let mut dev_b = SimulatedDevice::new(&server);

    // 1. Device A reads to page 45/100 and adds a highlight + bookmark (source="aristotle")
    let t1 = server.advance_clock_ms(10_000);
    dev_a.xpointer = Some("/body/DocFragment[4]/body/p[5]/text().0".into());
    dev_a.progress = Some((45, 100));
    dev_a.progress_updated_ms = t1;
    dev_a.notes.push(LocalNote {
        id: "hl-a1".into(),
        kind: NoteType::Annotation,
        position: Some((
            "/body/DocFragment[4]/body/p[5]/text().0".into(),
            "/body/DocFragment[4]/body/p[5]/text().30".into(),
        )),
        text: Some("Device A passage".into()),
        note: Some("Note from A".into()),
        color: Some("yellow".into()),
        style: Some("highlight".into()),
        page: Some("45".into()),
        location: Some((45, 100)),
        chapter: Some("Ch 4".into()),
        source: "aristotle".into(),
        created_at: t1,
        updated_at: t1,
    });
    dev_a.notes.push(LocalNote {
        id: "bm-a1".into(),
        kind: NoteType::Bookmark,
        position: Some((
            "/body/DocFragment[4]/body/p[8]/text().0".into(),
            String::new(),
        )),
        text: None,
        note: None,
        color: None,
        style: None,
        page: Some("45".into()),
        location: Some((45, 100)),
        chapter: Some("Ch 4".into()),
        source: "aristotle".into(),
        created_at: t1,
        updated_at: t1,
    });
    dev_a.push_book_state(book_hash);

    // Verify multi-entry progress consistency immediately after Device A's config push:
    // Both GET /api/sync?type=books and GET /api/sync?type=configs MUST agree on [45, 100]!
    let books_check = dev_a.client.pull(Some("books"), 0).expect("books check");
    let configs_check = dev_a
        .client
        .pull(Some("configs"), 0)
        .expect("configs check");
    assert_eq!(books_check.books[0].progress, Some((45, 100)));
    assert_eq!(
        configs_check.configs[0].to_model().progress,
        Some((45, 100))
    );

    // 2. Device B pulls -> matches Device A's 45/100 + highlight + bookmark
    let b_books = dev_b.pull_and_merge(book_hash);
    assert_eq!(b_books.len(), 1);
    assert_eq!(b_books[0].progress, Some((45, 100)));
    assert_eq!(dev_b.progress, Some((45, 100)));
    assert_eq!(
        dev_b.xpointer.as_deref(),
        Some("/body/DocFragment[4]/body/p[5]/text().0")
    );
    assert_eq!(dev_b.notes.len(), 2);

    // 3. Device B advances to 80/100, deletes hl-a1, adds hl-b2, and pushes
    let t2 = server.advance_clock_ms(20_000);
    dev_b.xpointer = Some("/body/DocFragment[8]/body/p[2]/text().0".into());
    dev_b.progress = Some((80, 100));
    dev_b.progress_updated_ms = t2;
    dev_b.notes.retain(|n| n.id != "hl-a1");
    dev_b.tombstones.push(LocalTombstone {
        id: "hl-a1".into(),
        deleted_at: t2,
    });
    dev_b.notes.push(LocalNote {
        id: "hl-b2".into(),
        kind: NoteType::Annotation,
        position: Some((
            "/body/DocFragment[8]/body/p[2]/text().0".into(),
            "/body/DocFragment[8]/body/p[2]/text().18".into(),
        )),
        text: Some("Device B climax".into()),
        note: Some("Great ending".into()),
        color: Some("pink".into()),
        style: Some("highlight".into()),
        page: None,
        location: None,
        chapter: None,
        source: READEST_SOURCE.into(),
        created_at: t2,
        updated_at: t2,
    });
    dev_b.push_book_state(book_hash);

    // 4. Device A pulls -> converges 100% to 80/100, hl-a1 gone, bm-a1 + hl-b2 present!
    let a_books = dev_a.pull_and_merge(book_hash);
    assert_eq!(a_books.len(), 1);
    assert_eq!(a_books[0].progress, Some((80, 100)));
    assert_eq!(dev_a.progress, Some((80, 100)));
    assert_eq!(
        dev_a.xpointer.as_deref(),
        Some("/body/DocFragment[8]/body/p[2]/text().0")
    );

    let ids_a: HashSet<&str> = dev_a.notes.iter().map(|n| n.id.as_str()).collect();
    let ids_b: HashSet<&str> = dev_b.notes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids_a, HashSet::from(["bm-a1", "hl-b2"]));
    assert_eq!(ids_a, ids_b);
    assert!(dev_a.tombstones.iter().any(|t| t.id == "hl-a1"));

    // 5. Two more ping-pong rounds: zero duplication, zero resurrection!
    for _ in 0..2 {
        server.advance_clock_ms(1_000);
        dev_a.push_book_state(book_hash);
        dev_b.pull_and_merge(book_hash);
        server.advance_clock_ms(1_000);
        dev_b.push_book_state(book_hash);
        dev_a.pull_and_merge(book_hash);
        assert_eq!(dev_a.notes.len(), 2);
        assert_eq!(dev_b.notes.len(), 2);
        assert_eq!(dev_a.progress, Some((80, 100)));
        assert_eq!(dev_b.progress, Some((80, 100)));
    }
}

// ---------------------------------------------------------------------------
// Scenario 6: Adversarial Fault Injection & High-Volume Speed
// ---------------------------------------------------------------------------

#[test]
fn scenario_6_adversarial_faults_and_large_scale_performance() {
    let server = StrictReadestServer::start();
    let client = new_client(&server);
    let session = client
        .login("reader@example.com", "correct-horse-battery")
        .expect("login");

    // 6a. HTTP 429 rate limit
    server.push_sync_fault(SyncFault::HttpStatus(429, "Too Many Requests".into()));
    let err_429 = client.pull(Some("notes"), 0).expect_err("429");
    assert!(matches!(err_429, SyncError::HttpStatus(429, _)));
    assert_eq!(err_429.kind(), SyncErrorKind::Server);
    assert_eq!(
        client.session().unwrap().access_token,
        session.access_token,
        "429 must preserve session"
    );

    // 6b. HTTP 500 transient error -> session preserved -> immediate recovery works
    server.push_sync_fault(SyncFault::HttpStatus(500, "Database timeout".into()));
    let err_500 = client.pull(Some("notes"), 0).expect_err("500");
    assert!(matches!(err_500, SyncError::HttpStatus(500, _)));
    assert_eq!(err_500.kind(), SyncErrorKind::Server);
    client
        .pull(Some("notes"), 0)
        .expect("immediate retry after transient 500 succeeds");

    // 6c. Malformed JSON response
    server.push_sync_fault(SyncFault::MalformedJson);
    let err_json = client.pull(Some("notes"), 0).expect_err("malformed json");
    assert!(matches!(err_json, SyncError::Deserialization(_)));
    assert_eq!(err_json.kind(), SyncErrorKind::Server);

    // 6d-1. Single dropped socket mid-response (e.g. Cloudflare edge drop on 104.21.4.16)
    //       MUST transparently recover via automatic retry and succeed.
    server.push_sync_fault(SyncFault::DropSocket);
    let recovered = client
        .pull(Some("notes"), 0)
        .expect("single dropped socket must recover automatically via retry");
    assert!(recovered.notes.is_empty());
    assert_eq!(
        client.session().unwrap().access_token,
        session.access_token,
        "recovered dropped socket preserves session"
    );

    // 6d-2. Three consecutive dropped sockets exhaust all 3 attempts (initial + 2 retries),
    //       returning Err(SyncError::Transport(_)) cleanly while preserving session.
    server.push_sync_fault(SyncFault::DropSocket);
    server.push_sync_fault(SyncFault::DropSocket);
    server.push_sync_fault(SyncFault::DropSocket);
    let err_drop = client
        .pull(Some("notes"), 0)
        .expect_err("three consecutive dropped sockets must exhaust retries");
    assert!(matches!(err_drop, SyncError::Transport(_)), "{err_drop:?}");
    assert_eq!(err_drop.kind(), SyncErrorKind::Network);
    assert_eq!(
        client.session().unwrap().access_token,
        session.access_token,
        "exhausted dropped sockets preserve session"
    );

    // 6e. Connection refused (unbound loopback port)
    let dead_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = dead_listener.local_addr().unwrap().port();
    drop(dead_listener);
    let dead_client = SyncClient::new(
        ReadestTransport::with_timeout(Policy::open(), 2),
        SyncConfig {
            api_base: format!("http://127.0.0.1:{dead_port}/api"),
            supabase_url: format!("http://127.0.0.1:{dead_port}"),
            supabase_anon_key: SyncConfig::default().supabase_anon_key,
        },
    );
    dead_client.set_session(Some(Session::new("a", "r", i64::MAX)));
    let err_refused = dead_client
        .pull(Some("notes"), 0)
        .expect_err("connection refused");
    assert!(
        matches!(err_refused, SyncError::Transport(_)),
        "{err_refused:?}"
    );
    assert_eq!(err_refused.kind(), SyncErrorKind::Network);

    // 6f. Large multi-page sync: 150 uploaded books (with multi-row same-ms ties) + 150 notes
    let start = Instant::now();
    let base = server.clock_ms();
    for i in 0..150usize {
        // Group every 5 books onto the same `synced_at` millisecond to stress page boundary tie-expansion!
        let sync_ts = base + ((i / 5) as i64 + 1) * 1_000;
        let upd_ts = base + (i as i64 + 1) * 100;
        server.seed_book(make_stored_book(BookSeed {
            user_id: "usr-sim-01",
            book_hash: &format!("bulkbook{i:04}"),
            title: &format!("Bulk Book {i:03}"),
            author: "Bulk Author",
            updated_ms: upd_ts,
            synced_ms: sync_ts,
            uploaded_at: Some(base),
            deleted_at: None,
            progress: Some(((i % 99 + 1) as u32, 100)),
        }));
    }

    // Push 150 notes -> SyncClient::push MUST automatically chunk into 2 batches (100 + 50)!
    server.clear_recorded();
    let t_notes = server.advance_clock_ms(60_000);
    let bulk_notes: Vec<DbBookNote> = (0..150usize)
        .map(|i| DbBookNote {
            book_hash: format!("bulkbook{:04}", i % 150),
            id: format!("bulk-note-{i:04}"),
            kind: if i % 3 == 0 {
                "bookmark".into()
            } else {
                "annotation".into()
            },
            xpointer0: Some(format!("/body/DocFragment[1]/p[{i}].0")),
            xpointer1: if i % 3 == 0 {
                None
            } else {
                Some(format!("/body/DocFragment[1]/p[{i}].12"))
            },
            text: Some(format!("Highlight #{i}")),
            created_at: Some(format_iso8601(t_notes)),
            updated_at: Some(format_iso8601(t_notes)),
            ..Default::default()
        })
        .collect();
    let push_resp = client
        .push(
            SyncPushRequest {
                notes: bulk_notes,
                ..Default::default()
            },
            None,
        )
        .expect("bulk 150-note push");
    assert_eq!(push_resp.notes.len(), 150);
    assert_eq!(
        server.recorded_pushes().len(),
        2,
        "150 notes must split cleanly into 2 BATCH_SIZE=100 requests"
    );

    // Pull all 150 books across multiple pages (page_size=37 splits across 5-row synced_at ties!)
    let (pulled_books, books_cursor) = client
        .pull_all_books_with_cursor(0, 37)
        .expect("multi-page 150 book pull");
    assert_eq!(pulled_books.len(), 150);
    let unique_hashes: HashSet<&str> = pulled_books.iter().map(|b| b.book_hash.as_str()).collect();
    assert_eq!(unique_hashes.len(), 150, "zero duplicates or skipped rows");
    assert!(books_cursor.is_some());

    // Browse cloud library across pages
    let cloud_books =
        fetch_cloud_books_paged(&client, None, &HashSet::new(), 37).expect("cloud browse 150");
    assert_eq!(cloud_books.len(), 150);

    // Pull all 150 notes back
    let pulled_notes = client.pull(Some("notes"), 0).expect("pull 150 notes");
    assert_eq!(pulled_notes.notes.len(), 150);

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "150-book + 150-note full loopback sync took too long: {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Adversarial Upstream Quirk: Empty account dummy book from real sync.ts:416-434
// ---------------------------------------------------------------------------

#[test]
fn adversarial_empty_account_upstream_dummy_book_contract() {
    let server = StrictReadestServer::start();
    server.state.lock().unwrap().enable_upstream_dummy_book = true;

    let client = new_client(&server);
    client
        .login("reader@example.com", "correct-horse-battery")
        .expect("login");

    // On a brand-new Readest account with 0 books, real upstream `sync.ts:416-434`
    // emits a dummy book containing BOTH snake_case and camelCase keys AND integer ms timestamps:
    //   { book_hash, hash, updated_at: 1793..., updatedAt: 1793..., deleted_at: 1793..., deletedAt: 1793... }
    let pulled = client
        .pull(Some("books"), 0)
        .expect("brand-new account initial pull MUST handle upstream sync.ts:416-434 dummy book");
    assert!(
        pulled.books.is_empty(),
        "dummy book 00000000000000000000000000000000 must be filtered out"
    );
}

// ---------------------------------------------------------------------------
// Scenario 7: Keep-Alive Connection Pooling, Gzip Compression & Retry Speed
// ---------------------------------------------------------------------------

#[test]
fn scenario_7_keepalive_pooling_gzip_and_retry_performance() {
    let server = StrictReadestServer::start();
    server.enable_keep_alive(true);
    server.enable_gzip_sync(true);

    let base = server.clock_ms();
    let book_hash = "keepalivebook0001";
    server.seed_book(make_stored_book(BookSeed {
        user_id: "usr-sim-01",
        book_hash,
        title: "Keep-Alive & Gzip Verification",
        author: "Readest QA",
        updated_ms: base + 1_000,
        synced_ms: base + 1_000,
        uploaded_at: Some(base),
        deleted_at: None,
        progress: Some((12, 100)),
    }));
    server.seed_config(StoredConfig {
        user_id: "usr-sim-01".into(),
        book_hash: book_hash.into(),
        meta_hash: None,
        location: Some("epubcfi(/6/4!/4/2/1:0)".into()),
        xpointer: Some("/body/DocFragment[2]/body/p[1]/text().0".into()),
        progress: Some("[12,100]".into()),
        rsvp_position: None,
        search_config: None,
        view_settings: Some(r#"{"fontSize":18}"#.into()),
        updated_at: format_iso8601(base + 1_000),
        deleted_at: None,
    });
    server.seed_note(StoredNote {
        user_id: "usr-sim-01".into(),
        book_hash: book_hash.into(),
        meta_hash: None,
        id: "seed-note-ka-1".into(),
        kind: "annotation".into(),
        cfi: None,
        xpointer0: Some("/body/DocFragment[2]/body/p[1]/text().0".into()),
        xpointer1: Some("/body/DocFragment[2]/body/p[1]/text().16".into()),
        page: Some(12),
        text: Some("Seeded gzip highlight".into()),
        style: Some("highlight".into()),
        color: Some("yellow".into()),
        note: Some("Should arrive over Keep-Alive + gzip".into()),
        global: None,
        created_at: format_iso8601(base + 1_000),
        updated_at: format_iso8601(base + 1_000),
        deleted_at: None,
    });

    // Dedicated scoped Policy for this server's loopback port so the shared
    // client pool slot is completely isolated from other parallel tests.
    let policy = Policy {
        scoped: true,
        allowed: parse_allow(&format!("127.0.0.1:{}", server.port)),
    };

    // 7a. Client #1 logs in via a fresh ReadestTransport::new(policy).
    let client_1 = SyncClient::new(ReadestTransport::new(policy.clone()), server.sync_config());
    let session = client_1
        .login("reader@example.com", "correct-horse-battery")
        .expect("login over keep-alive");

    // 7b. Client #2 (a SEPARATE ReadestTransport::new + SyncClient) runs a full
    //     sync round-trip: pull books + pull configs + pull notes + push.
    let sync_start = Instant::now();
    let client_2 = SyncClient::new(ReadestTransport::new(policy.clone()), server.sync_config());
    client_2.set_session(Some(session.clone()));

    let pulled_books = client_2
        .pull(Some("books"), 0)
        .expect("gzip pull books over keep-alive");
    assert_eq!(pulled_books.books.len(), 1);
    assert_eq!(pulled_books.books[0].book_hash, book_hash);
    assert_eq!(
        pulled_books.books[0].title,
        "Keep-Alive & Gzip Verification"
    );

    let pulled_configs = client_2
        .pull(Some("configs"), 0)
        .expect("gzip pull configs over keep-alive");
    assert_eq!(pulled_configs.configs.len(), 1);
    assert_eq!(
        pulled_configs.configs[0].xpointer.as_deref(),
        Some("/body/DocFragment[2]/body/p[1]/text().0")
    );

    let pulled_notes = client_2
        .pull(Some("notes"), 0)
        .expect("gzip pull notes over keep-alive");
    assert_eq!(pulled_notes.notes.len(), 1);
    assert_eq!(pulled_notes.notes[0].id, "seed-note-ka-1");
    assert_eq!(
        pulled_notes.notes[0].text.as_deref(),
        Some("Seeded gzip highlight")
    );

    let t_push = server.advance_clock_ms(10_000);
    let push_resp = client_2
        .push(
            SyncPushRequest {
                books: vec![],
                configs: vec![DbBookConfig {
                    book_hash: book_hash.into(),
                    xpointer: Some("/body/DocFragment[3]/body/p[4]/text().0".into()),
                    progress: Some("[34,100]".into()),
                    updated_at: Some(format_iso8601(t_push)),
                    ..Default::default()
                }],
                notes: vec![DbBookNote {
                    book_hash: book_hash.into(),
                    id: "pushed-note-ka-2".into(),
                    kind: "bookmark".into(),
                    xpointer0: Some("/body/DocFragment[3]/body/p[4]/text().0".into()),
                    text: Some("Pushed over reused Keep-Alive socket".into()),
                    created_at: Some(format_iso8601(t_push)),
                    updated_at: Some(format_iso8601(t_push)),
                    ..Default::default()
                }],
                stat_books: vec![],
                stat_pages: vec![],
            },
            None,
        )
        .expect("gzip push over keep-alive");
    assert_eq!(push_resp.configs.len(), 1);
    assert_eq!(push_resp.notes.len(), 1);
    assert_eq!(push_resp.notes[0].id, "pushed-note-ka-2");

    // 7c. Client #3 (a THIRD separate ReadestTransport::new + SyncClient) pulls
    //     again and confirms both notes are visible — STILL on the same TCP socket!
    let client_3 = SyncClient::new(ReadestTransport::new(policy.clone()), server.sync_config());
    client_3.set_session(Some(session.clone()));
    let verify_notes = client_3
        .pull(Some("notes"), 0)
        .expect("third client pull notes over same pooled socket");
    let sync_elapsed = sync_start.elapsed();
    assert_eq!(verify_notes.notes.len(), 2);
    assert!(verify_notes.notes.iter().any(|n| n.id == "seed-note-ka-1"));
    assert!(verify_notes
        .notes
        .iter()
        .any(|n| n.id == "pushed-note-ka-2"));

    // Verify Keep-Alive connection pooling: 6 HTTP requests across 3 separate
    // ReadestTransport::new / SyncClient instances reused EXACTLY 1 TCP connection!
    assert_eq!(
        server.total_http_requests(),
        6,
        "expected 6 total HTTP requests (login + 4 sync pulls + 1 sync push)"
    );
    assert_eq!(
        server.accepted_tcp_connections(),
        1,
        "multiple sequential ReadestTransport::new / SyncClient calls on the same host MUST reuse 1 Keep-Alive TCP connection"
    );

    // Verify transparent gzip negotiation & decompression on every /api/sync call.
    let accept_encodings = server.observed_accept_encoding();
    assert_eq!(accept_encodings.len(), 5, "all 5 /api/sync calls recorded");
    for (idx, ae) in accept_encodings.iter().enumerate() {
        assert!(
            ae.to_ascii_lowercase().contains("gzip"),
            "/api/sync request #{idx} missing gzip in Accept-Encoding: {ae:?}"
        );
    }
    assert_eq!(
        server.gzip_responses_sent(),
        5,
        "every /api/sync response must have been gzip-compressed and cleanly decompressed"
    );
    assert!(
        sync_elapsed < Duration::from_secs(2),
        "full Keep-Alive + gzip sync round-trip should be fast, took {sync_elapsed:?}"
    );

    // 7d. Mid-session dropped Keep-Alive socket recovers transparently via retry
    //     on a fresh connection without losing data or session.
    let retry_start = Instant::now();
    server.push_sync_fault(SyncFault::DropSocket);
    let after_retry = client_3
        .pull(Some("notes"), 0)
        .expect("single dropped Keep-Alive socket must recover via automatic retry");
    let retry_elapsed = retry_start.elapsed();
    assert_eq!(after_retry.notes.len(), 2);
    assert_eq!(
        server.accepted_tcp_connections(),
        2,
        "retry after dropped socket should dial exactly 1 fresh TCP connection"
    );
    assert_eq!(server.gzip_responses_sent(), 6);
    assert!(
        retry_elapsed < Duration::from_secs(2),
        "automatic retry recovery should complete promptly, took {retry_elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8: Unified Keep-Alive pool across timeouts, single-RTT pulls,
// retry socket retention, and concurrent differing-cursor full sync
// ---------------------------------------------------------------------------

#[test]
fn scenario_8_unified_pool_across_timeouts_single_rtt_and_retry_keeps_warm_socket() {
    let server = StrictReadestServer::start();
    server.enable_keep_alive(true);
    server.enable_gzip_sync(true);

    let t0 = server.clock_ms();
    let book_hash = "face1234feed5678face1234feed5678";
    server.seed_book(make_stored_book(BookSeed {
        user_id: "usr-sim-01",
        book_hash,
        title: "Unified Pool Speed Test",
        author: "Perf",
        updated_ms: t0 + 1_000,
        synced_ms: t0 + 1_000,
        uploaded_at: Some(t0),
        deleted_at: None,
        progress: Some((15, 100)),
    }));
    server.seed_config(StoredConfig {
        user_id: "usr-sim-01".into(),
        book_hash: book_hash.into(),
        meta_hash: None,
        location: None,
        xpointer: Some("/body/DocFragment[4]/body/p[2]/text().0".into()),
        progress: Some("[15,100]".into()),
        rsvp_position: None,
        search_config: None,
        view_settings: None,
        updated_at: format_iso8601(t0 + 1_000),
        deleted_at: None,
    });
    server.seed_note(StoredNote {
        user_id: "usr-sim-01".into(),
        book_hash: book_hash.into(),
        meta_hash: None,
        id: "speed-note-1".into(),
        kind: "annotation".into(),
        cfi: None,
        xpointer0: Some("/body/DocFragment[4]/body/p[2]/text().0".into()),
        xpointer1: Some("/body/DocFragment[4]/body/p[2]/text().12".into()),
        page: Some(15),
        text: Some("Fast highlight".into()),
        style: Some("highlight".into()),
        color: Some("yellow".into()),
        note: Some("note body".into()),
        global: None,
        created_at: format_iso8601(t0 + 1_000),
        updated_at: format_iso8601(t0 + 1_000),
        deleted_at: None,
    });

    let policy = Policy {
        scoped: true,
        allowed: parse_allow(&format!("127.0.0.1:{}", server.port)),
    };

    // 1. Default transport (`new`) logs in.
    let c_default = SyncClient::new(ReadestTransport::new(policy.clone()), server.sync_config());
    let session = c_default
        .login("reader@example.com", "correct-horse-battery")
        .expect("login");

    // 2. Full-sync transport (`with_timeout(30)`) does single-RTT untyped pull
    //    (matching readest_sync_all equal-cursor fast path) over the SAME socket.
    let c_full = SyncClient::new(
        ReadestTransport::with_timeout(policy.clone(), 30),
        server.sync_config(),
    );
    c_full.set_session(Some(session.clone()));
    let all = c_full
        .pull_internal(None, 0, None, None)
        .expect("single-RTT full pull");
    assert_eq!(all.books.len(), 1);
    assert_eq!(all.configs.len(), 1);
    assert_eq!(all.notes.len(), 1);

    // 3. Reader manual pull (`new`) fetches BOTH configs and notes in 1 GET.
    let one_book = c_default
        .pull_internal(None, 0, Some(1), Some(book_hash))
        .expect("single-RTT manual pull");
    assert_eq!(one_book.configs.len(), 1);
    assert_eq!(one_book.notes.len(), 1);

    // 4. Short-timeout inline push transport (`with_timeout(5)`) STILL reuses
    //    the EXACT SAME Keep-Alive socket from the unified Policy agent pool!
    let c_inline = SyncClient::new(
        ReadestTransport::with_timeout(policy.clone(), 5),
        server.sync_config(),
    );
    c_inline.set_session(Some(session.clone()));
    let t1 = server.advance_clock_ms(5_000);
    c_inline
        .push(
            SyncPushRequest {
                books: vec![],
                configs: vec![DbBookConfig {
                    book_hash: book_hash.into(),
                    xpointer: Some("/body/DocFragment[5]/body/p[1]/text().0".into()),
                    progress: Some("[20,100]".into()),
                    updated_at: Some(format_iso8601(t1)),
                    ..Default::default()
                }],
                notes: vec![],
                stat_books: vec![],
                stat_pages: vec![],
            },
            None,
        )
        .expect("inline push over shared socket");

    assert_eq!(server.total_http_requests(), 4);
    assert_eq!(
        server.accepted_tcp_connections(),
        1,
        "new(), with_timeout(30), and with_timeout(5) for the same Policy MUST share 1 Keep-Alive TCP socket"
    );

    // 5. After a dropped-socket retry, the retry's fresh connection MUST stay
    //    warm in the shared pool so the subsequent request dials 0 new sockets.
    server.push_sync_fault(SyncFault::DropSocket);
    let recovered = c_default
        .pull_internal(None, 0, Some(1), Some(book_hash))
        .expect("retry recovery");
    assert_eq!(recovered.configs.len(), 1);
    assert_eq!(server.accepted_tcp_connections(), 2);

    let reused_after_retry = c_full
        .pull_internal(None, 0, Some(1), Some(book_hash))
        .expect("subsequent request reuses retry's warm socket");
    assert_eq!(reused_after_retry.notes.len(), 1);
    assert_eq!(
        server.accepted_tcp_connections(),
        2,
        "healthy socket established during retry must stay warm in the shared pool"
    );

    // 6. Parallel differing-cursor full sync via std::thread::scope works
    //    concurrently on one shared SyncClient without error or token race.
    std::thread::scope(|scope| {
        let h_books = scope.spawn(|| c_full.pull_all_books_with_cursor(0, 100));
        let h_configs = scope.spawn(|| c_full.pull_internal(Some("configs"), t0, None, None));
        let h_notes = scope.spawn(|| c_full.pull_internal(Some("notes"), 0, None, None));
        let (books, _) = h_books.join().unwrap().expect("parallel books");
        let configs = h_configs.join().unwrap().expect("parallel configs");
        let notes = h_notes.join().unwrap().expect("parallel notes");
        assert_eq!(books.len(), 1);
        assert_eq!(configs.configs.len(), 1);
        assert_eq!(notes.notes.len(), 1);
    });
}
