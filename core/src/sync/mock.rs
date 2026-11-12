//! In-memory mock Readest server implementing `HttpTransport` for local and integration testing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::sync::merge::{merge_book_config, merge_book_rows, RowWinner};
use crate::sync::records::{
    DbBook, DbBookConfig, DbBookNote, StatBookRecord, StatPageRecord, SyncPullResponse,
    SyncPushRequest, SyncPushResponse,
};
use crate::sync::time::{format_iso8601, parse_iso8601};
use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};

struct MockUser {
    password: String,
    user_id: String,
}

#[derive(Clone)]
struct MockFileEntry {
    data: Vec<u8>,
    deleted: bool,
    book_hash: Option<String>,
    replica_kind: Option<String>,
    replica_id: Option<String>,
    created_at: String,
    updated_at: Option<String>,
}

#[derive(Clone)]
struct MockTicketInfo {
    user_id: String,
    file_key: String,
    expires_at_ms: i64,
}

#[derive(Default)]
struct InnerState {
    users: HashMap<String, MockUser>,
    anon_key: String,
    valid_access_tokens: HashMap<String, String>, // token -> user_id
    valid_refresh_tokens: HashMap<String, String>, // token -> user_id
    books: HashMap<(String, String), DbBook>,     // (user_id, book_hash) -> DbBook
    configs: HashMap<(String, String), DbBookConfig>, // (user_id, book_hash) -> DbBookConfig
    notes: HashMap<(String, String, String), DbBookNote>, // (user_id, book_hash, id) -> DbBookNote
    stat_books: HashMap<(String, String), StatBookRecord>,
    stat_pages: HashMap<(String, String, u32, i64), StatPageRecord>, // (user_id, book_hash, page, start_time) -> StatPageRecord
    files: HashMap<(String, String), MockFileEntry>, // (user_id, file_key) -> MockFileEntry
    signed_download_tickets: HashMap<String, MockTicketInfo>, // ticket_id -> MockTicketInfo
    download_base: String,
    next_ticket_id: usize,
    clock_ms: i64,
    token_expiry_sec: i64,
    inject_network_error: bool,
    inject_auth_expired: bool,
    inject_sync_auth_failures: usize,
    refresh_status_override: Option<u16>,
    /// How many token refreshes were asked for, successful or not.
    refresh_count: usize,
}

/// In-memory Readest synchronization mock server.
#[derive(Clone)]
pub struct MockServer {
    state: Arc<Mutex<InnerState>>,
}

impl Default for MockServer {
    fn default() -> Self {
        Self::new()
    }
}

impl MockServer {
    /// Create a new mock server with default user `test@example.com` / `password123`.
    pub fn new() -> Self {
        let mut inner = InnerState {
            clock_ms: 1_793_880_000_000, // 2026-11-05T12:00:00.000Z
            token_expiry_sec: 3600,
            anon_key: crate::sync::auth::SyncConfig::default().supabase_anon_key,
            download_base: "https://mock.storage.local/download".to_string(),
            ..Default::default()
        };

        inner.users.insert(
            "test@example.com".to_string(),
            MockUser {
                password: "password123".to_string(),
                user_id: "usr-01".to_string(),
            },
        );

        // Pre-create initial valid token for convenience
        inner
            .valid_access_tokens
            .insert("test-access-token".to_string(), "usr-01".to_string());
        inner
            .valid_refresh_tokens
            .insert("test-refresh-token".to_string(), "usr-01".to_string());

        Self {
            state: Arc::new(Mutex::new(inner)),
        }
    }

    /// Add a user with given credentials.
    pub fn add_user(&self, email: &str, password: &str, user_id: &str) {
        let mut state = self.state.lock().unwrap();
        state.users.insert(
            email.to_string(),
            MockUser {
                password: password.to_string(),
                user_id: user_id.to_string(),
            },
        );
    }

    /// Advance or set server clock.
    pub fn set_clock_ms(&self, ms: i64) {
        let mut state = self.state.lock().unwrap();
        state.clock_ms = ms;
    }

    /// Get current server clock in milliseconds.
    pub fn clock_ms(&self) -> i64 {
        let state = self.state.lock().unwrap();
        state.clock_ms
    }

    /// Advance server clock by delta milliseconds.
    pub fn advance_clock_ms(&self, delta: i64) {
        let mut state = self.state.lock().unwrap();
        state.clock_ms += delta;
    }

    /// Inject or clear network transport errors.
    pub fn set_inject_network_error(&self, inject: bool) {
        let mut state = self.state.lock().unwrap();
        state.inject_network_error = inject;
    }

    /// Inject or clear token invalidation / expiration.
    pub fn set_inject_auth_expired(&self, inject: bool) {
        let mut state = self.state.lock().unwrap();
        state.inject_auth_expired = inject;
    }

    /// Inject a number of authentication failures for `/api/sync` requests.
    pub fn inject_sync_auth_failures(&self, count: usize) {
        let mut state = self.state.lock().unwrap();
        state.inject_sync_auth_failures = count;
    }

    /// Set an explicit HTTP status code for refresh token requests (e.g. 401, 500, 429).
    pub fn set_inject_refresh_status(&self, status: Option<u16>) {
        let mut state = self.state.lock().unwrap();
        state.refresh_status_override = status;
    }

    /// How many token refreshes were asked for, successful or not.
    ///
    /// The mock rotates the refresh token like GoTrue does: a token that was
    /// spent is refused. A test counts the refreshes to show that two
    /// requests of one account spend the token once.
    pub fn refresh_count(&self) -> usize {
        self.state.lock().unwrap().refresh_count
    }

    /// Set acceptable anonymous key.
    pub fn set_anon_key(&self, key: &str) {
        let mut state = self.state.lock().unwrap();
        state.anon_key = key.to_string();
    }

    /// Set the base used to build signed download URLs.
    ///
    /// The ticket is appended as `{base}/{ticket}`. Defaults to
    /// `https://mock.storage.local/download`; point it at a LAN address so a
    /// device can resolve the URL instead of a domain only this process knows.
    pub fn set_download_base(&self, base: &str) {
        let mut state = self.state.lock().unwrap();
        state.download_base = base.trim_end_matches('/').to_string();
    }

    /// Insert a pre-existing note into the server database.
    pub fn insert_note(&self, user_id: &str, note: DbBookNote) {
        let mut state = self.state.lock().unwrap();
        state.notes.insert(
            (user_id.to_string(), note.book_hash.clone(), note.id.clone()),
            note,
        );
    }

    /// Insert a pre-existing book into the server database.
    pub fn insert_book(&self, user_id: &str, book: DbBook) {
        let mut state = self.state.lock().unwrap();
        state
            .books
            .insert((user_id.to_string(), book.book_hash.clone()), book);
    }

    /// Insert a storage file for user under specified `file_key`.
    pub fn insert_file(&self, user_id: &str, file_key: &str, data: Vec<u8>) {
        let (book_hash, replica_kind, replica_id) = infer_file_key_metadata(file_key);
        self.insert_file_with_metadata(
            user_id,
            file_key,
            data,
            book_hash.as_deref(),
            replica_kind.as_deref(),
            replica_id.as_deref(),
        );
    }

    /// Insert a storage file for user with explicit database metadata.
    pub fn insert_file_with_metadata(
        &self,
        user_id: &str,
        file_key: &str,
        data: Vec<u8>,
        book_hash: Option<&str>,
        replica_kind: Option<&str>,
        replica_id: Option<&str>,
    ) {
        let mut state = self.state.lock().unwrap();
        let ts = format_iso8601(state.clock_ms);
        state.files.insert(
            (user_id.to_string(), file_key.to_string()),
            MockFileEntry {
                data,
                deleted: false,
                book_hash: book_hash.map(|s| s.to_string()),
                replica_kind: replica_kind.map(|s| s.to_string()),
                replica_id: replica_id.map(|s| s.to_string()),
                created_at: ts.clone(),
                updated_at: Some(ts),
            },
        );
    }

    /// Mark an existing storage file as deleted.
    pub fn mark_file_deleted(&self, user_id: &str, file_key: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state
            .files
            .get_mut(&(user_id.to_string(), file_key.to_string()))
        {
            entry.deleted = true;
        }
    }

    /// Retrieve raw binary data of a stored file, if present and not deleted.
    pub fn get_file_data(&self, user_id: &str, file_key: &str) -> Option<Vec<u8>> {
        let state = self.state.lock().unwrap();
        state
            .files
            .get(&(user_id.to_string(), file_key.to_string()))
            .and_then(|e| {
                if e.deleted {
                    None
                } else {
                    Some(e.data.clone())
                }
            })
    }

    /// Insert a book along with its cloud storage file binary.
    ///
    /// Derives the standard `file_key` (`{user_id}/Readest/Books/{book_hash}/{book_hash}.{ext}`)
    /// and populates `uploaded_at` and `user_id` if not already set.
    pub fn insert_book_with_file(&self, user_id: &str, mut book: DbBook, data: Vec<u8>) -> String {
        let ext = if book.format.is_empty() {
            "epub".to_string()
        } else {
            book.format.to_ascii_lowercase()
        };
        let file_key = format!(
            "{user_id}/Readest/Books/{}/{}.{ext}",
            book.book_hash, book.book_hash
        );
        book.user_id = Some(user_id.to_string());
        if book.uploaded_at.is_none() {
            let clock_ms = self.clock_ms();
            book.uploaded_at = Some(format_iso8601(clock_ms));
        }
        let b_hash = book.book_hash.clone();
        self.insert_book(user_id, book);
        self.insert_file_with_metadata(user_id, &file_key, data, Some(&b_hash), None, None);
        file_key
    }

    /// Handle an HTTP request and generate response.
    pub fn handle_request(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let inject_net = {
            let state = self.state.lock().unwrap();
            state.inject_network_error
        };

        if inject_net {
            return Err(TransportError::Network(
                "simulated network loss".to_string(),
            ));
        }

        let path_and_query = match req.url.find("://") {
            Some(idx) => {
                let rest = &req.url[idx + 3..];
                match rest.find('/') {
                    Some(slash) => &rest[slash..],
                    None => "/",
                }
            }
            None => req.url.as_str(),
        };

        let path = match path_and_query.find('?') {
            Some(q) => &path_and_query[..q],
            None => path_and_query,
        };

        let query = match path_and_query.find('?') {
            Some(q) => &path_and_query[q + 1..],
            None => "",
        };

        // GoTrue Auth token endpoint
        if path == "/auth/v1/token" && req.method == "POST" {
            let apikey_hdr = req.headers.iter().find_map(|(k, v)| {
                if k.eq_ignore_ascii_case("apikey") {
                    Some(v.as_str())
                } else {
                    None
                }
            });
            let valid_anon = {
                let state = self.state.lock().unwrap();
                state.anon_key.clone()
            };
            if apikey_hdr.is_none() || apikey_hdr != Some(valid_anon.as_str()) {
                return Ok(HttpResponse::new(
                    401,
                    b"{\"message\":\"Invalid API key\"}".to_vec(),
                ));
            }
            return self.handle_auth_token(query, &req.body);
        }

        // Storage presigned download endpoint (public signed URL, no Bearer auth)
        if (path.starts_with("/download/")
            || req.url.starts_with("https://mock.storage.local/download/"))
            && req.method == "GET"
        {
            let ticket_id = if let Some(stripped) =
                req.url.strip_prefix("https://mock.storage.local/download/")
            {
                stripped
            } else if let Some(stripped) = path.strip_prefix("/download/") {
                stripped
            } else {
                path
            };
            let clean_ticket_id = match ticket_id.find('?') {
                Some(idx) => &ticket_id[..idx],
                None => ticket_id,
            };
            return self.handle_storage_download_blob(clean_ticket_id);
        }

        // Sync API endpoints require Bearer auth (returns 403 on failure per upstream sync.ts:275, 655)
        let user_id = {
            let mut state = self.state.lock().unwrap();
            if state.inject_auth_expired {
                return Ok(HttpResponse::new(
                    403,
                    b"{\"error\":\"Not authenticated\"}".to_vec(),
                ));
            }
            if state.inject_sync_auth_failures > 0 {
                state.inject_sync_auth_failures -= 1;
                return Ok(HttpResponse::new(
                    403,
                    b"{\"error\":\"Not authenticated\"}".to_vec(),
                ));
            }
            match self.extract_bearer_token(&req) {
                Some(token) => match state.valid_access_tokens.get(&token) {
                    Some(uid) => uid.clone(),
                    None => {
                        return Ok(HttpResponse::new(
                            403,
                            b"{\"error\":\"Not authenticated\"}".to_vec(),
                        ));
                    }
                },
                None => {
                    return Ok(HttpResponse::new(
                        403,
                        b"{\"error\":\"Not authenticated\"}".to_vec(),
                    ));
                }
            }
        };

        if path == "/api/sync" {
            if req.method == "GET" {
                return self.handle_sync_get(&user_id, query);
            } else if req.method == "POST" {
                return self.handle_sync_post(&user_id, &req.body);
            }
        }

        if path == "/api/storage/list" {
            if req.method == "GET" {
                return self.handle_storage_list(&user_id, query);
            } else {
                return Ok(HttpResponse::new(
                    405,
                    b"{\"error\":\"Method not allowed\"}".to_vec(),
                ));
            }
        }

        if path == "/api/storage/download" {
            if req.method == "GET" {
                return self.handle_storage_download_ticket(&user_id, query);
            } else {
                return Ok(HttpResponse::new(
                    405,
                    b"{\"error\":\"Method not allowed\"}".to_vec(),
                ));
            }
        }

        Ok(HttpResponse::new(
            404,
            b"{\"error\":\"not_found\"}".to_vec(),
        ))
    }

    fn extract_bearer_token(&self, req: &HttpRequest) -> Option<String> {
        for (k, v) in &req.headers {
            if k.eq_ignore_ascii_case("authorization") {
                let v = v.trim();
                if let Some(token) = v.strip_prefix("Bearer ") {
                    return Some(token.trim().to_string());
                }
            }
        }
        None
    }

    fn handle_auth_token(&self, query: &str, body: &[u8]) -> Result<HttpResponse, TransportError> {
        let grant_type = parse_query(query).get("grant_type").cloned();
        let mut state = self.state.lock().unwrap();

        match grant_type.as_deref() {
            Some("password") => {
                #[derive(serde::Deserialize)]
                struct LoginBody {
                    email: String,
                    password: String,
                }
                let login: LoginBody = match serde_json::from_slice(body) {
                    Ok(b) => b,
                    Err(_) => {
                        return Ok(HttpResponse::new(
                            400,
                            b"{\"error\":\"invalid_request\"}".to_vec(),
                        ))
                    }
                };

                let user_match = state.users.get(&login.email).and_then(|u| {
                    if u.password == login.password {
                        Some(u.user_id.clone())
                    } else {
                        None
                    }
                });

                if let Some(user_id) = user_match {
                    state.clock_ms += 10;
                    let token_suffix = state.clock_ms;
                    let access_token = format!("mock-acc-{token_suffix}");
                    let refresh_token = format!("mock-ref-{token_suffix}");

                    state
                        .valid_access_tokens
                        .insert(access_token.clone(), user_id.clone());
                    state
                        .valid_refresh_tokens
                        .insert(refresh_token.clone(), user_id.clone());

                    let resp_json = serde_json::json!({
                        "access_token": access_token,
                        "refresh_token": refresh_token,
                        "expires_in": state.token_expiry_sec,
                        "token_type": "bearer",
                        "user": {
                            "id": user_id,
                            "email": login.email
                        }
                    });
                    return Ok(
                        HttpResponse::new(200, serde_json::to_vec(&resp_json).unwrap())
                            .with_header("Content-Type", "application/json"),
                    );
                }

                Ok(HttpResponse::new(
                    400,
                    b"{\"error\":\"invalid_grant\",\"error_description\":\"Invalid login credentials\"}".to_vec(),
                ))
            }
            Some("refresh_token") => {
                if let Some(status) = state.refresh_status_override {
                    return Ok(HttpResponse::new(
                        status,
                        format!("{{\"error\":\"injected_status_{status}\"}}").into_bytes(),
                    ));
                }

                #[derive(serde::Deserialize)]
                struct RefreshBody {
                    refresh_token: String,
                }
                let refresh: RefreshBody = match serde_json::from_slice(body) {
                    Ok(b) => b,
                    Err(_) => {
                        return Ok(HttpResponse::new(
                            400,
                            b"{\"error\":\"invalid_request\"}".to_vec(),
                        ))
                    }
                };

                state.refresh_count += 1;

                if let Some(user_id) = state.valid_refresh_tokens.remove(&refresh.refresh_token) {
                    state.clock_ms += 10;
                    let token_suffix = state.clock_ms;
                    let new_acc = format!("mock-acc-{token_suffix}");
                    let new_ref = format!("mock-ref-{token_suffix}");

                    state
                        .valid_access_tokens
                        .insert(new_acc.clone(), user_id.clone());
                    state
                        .valid_refresh_tokens
                        .insert(new_ref.clone(), user_id.clone());

                    let resp_json = serde_json::json!({
                        "access_token": new_acc,
                        "refresh_token": new_ref,
                        "expires_in": state.token_expiry_sec,
                        "token_type": "bearer"
                    });
                    return Ok(
                        HttpResponse::new(200, serde_json::to_vec(&resp_json).unwrap())
                            .with_header("Content-Type", "application/json"),
                    );
                }

                Ok(HttpResponse::new(
                    401,
                    b"{\"error\":\"invalid_grant\",\"error_description\":\"Invalid refresh token\"}".to_vec(),
                ))
            }
            _ => Ok(HttpResponse::new(
                400,
                b"{\"error\":\"unsupported_grant_type\"}".to_vec(),
            )),
        }
    }

    fn handle_sync_get(&self, user_id: &str, query: &str) -> Result<HttpResponse, TransportError> {
        let params = parse_query(query);
        let since_ms: i64 = match params.get("since").and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => {
                return Ok(HttpResponse::new(
                    400,
                    b"{\"error\":\"bad_request\",\"message\":\"since required\"}".to_vec(),
                ))
            }
        };

        if let Some(lim_str) = params.get("limit") {
            if lim_str == "0" {
                return Ok(HttpResponse::new(
                    400,
                    b"{\"error\":\"bad_request\",\"message\":\"limit must be > 0\"}".to_vec(),
                ));
            }
        }

        let req_type = params.get("type").map(|s| s.as_str());
        let limit = params.get("limit").and_then(|s| s.parse::<usize>().ok());
        let book_filter = params.get("book").map(|s| s.as_str());
        let meta_hash_filter = params.get("meta_hash").map(|s| s.as_str());

        let matches_filter = |rec_book: &str, rec_meta: Option<&str>| {
            if let (Some(bf), Some(mf)) = (book_filter, meta_hash_filter) {
                rec_book == bf || (rec_meta.is_some() && rec_meta == Some(mf))
            } else if let Some(bf) = book_filter {
                rec_book == bf
            } else if let Some(mf) = meta_hash_filter {
                rec_meta.is_some() && rec_meta == Some(mf)
            } else {
                true
            }
        };

        let state = self.state.lock().unwrap();
        let mut response = SyncPullResponse::default();

        // 1. Books
        if req_type.is_none() || req_type == Some("books") {
            let mut matching_books: Vec<DbBook> = state
                .books
                .iter()
                .filter(|((uid, b_hash), b)| {
                    uid == user_id && matches_filter(b_hash.as_str(), b.meta_hash.as_deref()) && {
                        let synced_ms = b
                            .synced_at
                            .as_deref()
                            .and_then(|s| parse_iso8601(s).ok())
                            .unwrap_or_else(|| {
                                b.updated_at
                                    .as_deref()
                                    .and_then(|s| parse_iso8601(s).ok())
                                    .unwrap_or(0)
                            });
                        synced_ms > since_ms
                    }
                })
                .map(|(_, b)| b.clone())
                .collect();

            // Sort by synced_at ascending
            matching_books.sort_by(|a, b| {
                let a_ms = a
                    .synced_at
                    .as_deref()
                    .and_then(|s| parse_iso8601(s).ok())
                    .unwrap_or(0);
                let b_ms = b
                    .synced_at
                    .as_deref()
                    .and_then(|s| parse_iso8601(s).ok())
                    .unwrap_or(0);
                a_ms.cmp(&b_ms).then_with(|| a.book_hash.cmp(&b.book_hash))
            });

            // Initial pull dummy book injection (contract §1.4 & pagination_boundary.json)
            if since_ms < 1000 && matching_books.is_empty() {
                let user_total_books = state
                    .books
                    .iter()
                    .filter(|((uid, _), _)| uid == user_id)
                    .count();
                if user_total_books == 0 {
                    matching_books.push(DbBook {
                        user_id: Some(user_id.to_string()),
                        book_hash: "00000000000000000000000000000000".to_string(),
                        title: "Dummy Book".to_string(),
                        format: "EPUB".to_string(),
                        author: String::new(),
                        deleted_at: Some(format_iso8601(state.clock_ms)),
                        ..Default::default()
                    });
                }
            }

            // Same-millisecond boundary expansion pagination
            if let Some(lim) = limit {
                if matching_books.len() > lim {
                    let cutoff_synced = matching_books[lim - 1].synced_at.clone();
                    let mut expanded_count = lim;
                    while expanded_count < matching_books.len()
                        && matching_books[expanded_count].synced_at == cutoff_synced
                    {
                        expanded_count += 1;
                    }
                    matching_books.truncate(expanded_count);
                }
            }

            response.books = matching_books;
        }

        // 2. Configs
        if req_type.is_none() || req_type == Some("configs") {
            let matching_configs: Vec<DbBookConfig> = state
                .configs
                .iter()
                .filter(|((uid, b_hash), c)| {
                    uid == user_id && matches_filter(b_hash.as_str(), None) && {
                        let u_ms = c
                            .updated_at
                            .as_deref()
                            .and_then(|s| parse_iso8601(s).ok())
                            .unwrap_or(0);
                        let d_ms = c
                            .deleted_at
                            .as_deref()
                            .and_then(|s| parse_iso8601(s).ok())
                            .unwrap_or(0);
                        u_ms > since_ms || d_ms > since_ms
                    }
                })
                .map(|(_, c)| c.clone())
                .collect();
            response.configs = matching_configs;
        }

        // 3. Notes
        if req_type.is_none() || req_type == Some("notes") {
            let matching_notes: Vec<DbBookNote> = state
                .notes
                .iter()
                .filter(|((uid, b_hash, _), n)| {
                    uid == user_id && matches_filter(b_hash.as_str(), None) && {
                        let u_ms = n
                            .updated_at
                            .as_deref()
                            .and_then(|s| parse_iso8601(s).ok())
                            .unwrap_or(0);
                        let d_ms = n
                            .deleted_at
                            .as_deref()
                            .and_then(|s| parse_iso8601(s).ok())
                            .unwrap_or(0);
                        u_ms > since_ms || d_ms > since_ms
                    }
                })
                .map(|(_, n)| n.clone())
                .collect();
            response.notes = matching_notes;
        }

        // 4. Stats
        if req_type.is_none() || req_type == Some("stats") {
            let matching_books: Vec<StatBookRecord> = state
                .stat_books
                .iter()
                .filter(|((uid, b_hash), b)| {
                    uid == user_id && matches_filter(b_hash.as_str(), None) && {
                        let u_ms = b
                            .updated_at
                            .as_deref()
                            .and_then(|s| parse_iso8601(s).ok())
                            .unwrap_or(0);
                        u_ms > since_ms
                    }
                })
                .map(|(_, b)| b.clone())
                .collect();
            response.stat_books = matching_books;

            let mut matching_pages: Vec<StatPageRecord> = state
                .stat_pages
                .iter()
                .filter(|((uid, b_hash, _, _), p)| {
                    uid == user_id && matches_filter(b_hash.as_str(), None) && {
                        let u_ms = p
                            .updated_at
                            .as_deref()
                            .and_then(|s| parse_iso8601(s).ok())
                            .unwrap_or(0);
                        u_ms > since_ms
                    }
                })
                .map(|(_, p)| p.clone())
                .collect();

            if req_type == Some("stats") {
                matching_pages.sort_by(|a, b| {
                    let a_ms = a
                        .updated_at
                        .as_deref()
                        .and_then(|s| parse_iso8601(s).ok())
                        .unwrap_or(0);
                    let b_ms = b
                        .updated_at
                        .as_deref()
                        .and_then(|s| parse_iso8601(s).ok())
                        .unwrap_or(0);
                    a_ms.cmp(&b_ms)
                });
                if let Some(lim) = limit {
                    if matching_pages.len() > lim {
                        let cutoff_updated = matching_pages[lim - 1].updated_at.clone();
                        let mut expanded_count = lim;
                        while expanded_count < matching_pages.len()
                            && matching_pages[expanded_count].updated_at == cutoff_updated
                        {
                            expanded_count += 1;
                        }
                        matching_pages.truncate(expanded_count);
                    }
                }
            }
            response.stat_pages = matching_pages;
        }

        let body = serde_json::to_vec(&response).unwrap();
        Ok(HttpResponse::new(200, body)
            .with_header("Content-Type", "application/json")
            .with_header("Cache-Control", "no-store")
            .with_header("Pragma", "no-cache"))
    }

    fn handle_sync_post(&self, user_id: &str, body: &[u8]) -> Result<HttpResponse, TransportError> {
        let push_req: SyncPushRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => {
                return Ok(HttpResponse::new(
                    400,
                    format!("{{\"error\":\"bad_request\",\"message\":\"{e}\"}}").into_bytes(),
                ))
            }
        };

        let mut state = self.state.lock().unwrap();
        state.clock_ms += 100;
        let now_str = format_iso8601(state.clock_ms);

        let mut authoritative_books = Vec::new();
        let mut authoritative_configs = Vec::new();
        let mut authoritative_notes = Vec::new();

        // 1. Process books
        for incoming in push_req.books {
            let key = (user_id.to_string(), incoming.book_hash.clone());
            let auth_book = match state.books.get(&key) {
                Some(existing) => {
                    let (_, mut merged) = merge_book_rows(&incoming, existing);
                    merged.synced_at = Some(now_str.clone());
                    merged
                }
                None => {
                    let mut b = incoming;
                    b.user_id = Some(user_id.to_string());
                    b.synced_at = Some(now_str.clone());
                    if b.created_at.is_none() {
                        b.created_at = Some(now_str.clone());
                    }
                    b.updated_at = Some(now_str.clone()); // Upstream sync.ts:726-727: dbRec.updated_at = new Date().toISOString()
                    b
                }
            };
            state.books.insert(key, auth_book.clone());
            authoritative_books.push(auth_book);
        }

        // 2. Process configs & piggyback progress to books
        for incoming in push_req.configs {
            let key = (user_id.to_string(), incoming.book_hash.clone());
            let auth_config = match state.configs.get(&key) {
                Some(existing) => {
                    let (win, merged) = merge_book_config(&incoming, existing);
                    if win == RowWinner::Local {
                        merged
                    } else {
                        existing.clone()
                    }
                }
                None => {
                    let mut c = incoming;
                    c.user_id = Some(user_id.to_string());
                    if c.created_at.is_none() {
                        c.created_at = Some(now_str.clone());
                    }
                    c.updated_at = Some(now_str.clone()); // Upstream sync.ts:726-727: dbRec.updated_at = new Date().toISOString()
                    c
                }
            };

            // Piggyback progress to books table (contract §2.4 & sync.ts:925)
            if let Some(ref p_str) = auth_config.progress {
                let trimmed = p_str.trim();
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    let parts: Vec<&str> = trimmed[1..trimmed.len() - 1].split(',').collect();
                    if parts.len() == 2 {
                        if let (Ok(cur), Ok(tot)) = (
                            parts[0].trim().parse::<u32>(),
                            parts[1].trim().parse::<u32>(),
                        ) {
                            let book_key = (user_id.to_string(), auth_config.book_hash.clone());
                            if let Some(book) = state.books.get_mut(&book_key) {
                                let is_txt = crate::sync::policy::is_txt_format(&book.format);
                                if !is_txt {
                                    let book_u = book
                                        .updated_at
                                        .as_deref()
                                        .and_then(|s| parse_iso8601(s).ok())
                                        .unwrap_or(0);
                                    let conf_u = auth_config
                                        .updated_at
                                        .as_deref()
                                        .and_then(|s| parse_iso8601(s).ok())
                                        .unwrap_or(0);
                                    if book_u < conf_u {
                                        book.progress = Some((cur, tot));
                                        book.updated_at = auth_config.updated_at.clone();
                                        book.synced_at = Some(now_str.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }

            state.configs.insert(key, auth_config.clone());
            authoritative_configs.push(auth_config);
        }

        // 3. Process notes
        for incoming in push_req.notes {
            let key = (
                user_id.to_string(),
                incoming.book_hash.clone(),
                incoming.id.clone(),
            );
            let auth_note = match state.notes.get(&key) {
                Some(existing) => {
                    let merged = crate::sync::merge::merge_notes(
                        &[incoming],
                        std::slice::from_ref(existing),
                    );
                    merged.into_iter().next().unwrap()
                }
                None => {
                    let mut n = incoming;
                    n.user_id = Some(user_id.to_string());
                    if n.created_at.is_none() {
                        n.created_at = Some(now_str.clone());
                    }
                    n.updated_at = Some(now_str.clone()); // Upstream sync.ts:726-727: dbRec.updated_at = new Date().toISOString()
                    n
                }
            };
            state.notes.insert(key, auth_note.clone());
            authoritative_notes.push(auth_note);
        }

        // 4. Process stats
        for mut sb in push_req.stat_books {
            sb.user_id = Some(user_id.to_string());
            sb.updated_at = Some(now_str.clone());
            let key = (user_id.to_string(), sb.book_hash.clone());
            state.stat_books.insert(key, sb);
        }

        for mut page in push_req.stat_pages {
            page.user_id = Some(user_id.to_string());
            page.updated_at = Some(now_str.clone());
            let key = (
                user_id.to_string(),
                page.book_hash.clone(),
                page.page,
                page.start_time,
            );
            match state.stat_pages.get_mut(&key) {
                Some(existing) => {
                    if page.duration > existing.duration {
                        *existing = page;
                    }
                }
                None => {
                    state.stat_pages.insert(key, page);
                }
            }
        }

        let push_resp = SyncPushResponse {
            books: authoritative_books,
            configs: authoritative_configs,
            notes: authoritative_notes,
        };

        let resp_bytes = serde_json::to_vec(&push_resp).unwrap();
        Ok(HttpResponse::new(200, resp_bytes)
            .with_header("Content-Type", "application/json")
            .with_header("Cache-Control", "no-store")
            .with_header("Pragma", "no-cache"))
    }

    fn handle_storage_list(
        &self,
        user_id: &str,
        query: &str,
    ) -> Result<HttpResponse, TransportError> {
        let qmap = parse_query(query);
        let page = qmap
            .get("page")
            .and_then(|p| p.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let page_size = qmap
            .get("pageSize")
            .and_then(|p| p.parse::<usize>().ok())
            .unwrap_or(50)
            .clamp(1, 1000);
        let book_hash_filter = qmap.get("bookHash").map(|s| s.as_str());

        let state = self.state.lock().unwrap();

        let mut matched_files: Vec<serde_json::Value> = state
            .files
            .iter()
            .filter(|((uid, _), entry)| uid == user_id && !entry.deleted)
            .filter(|((_, file_key), entry)| {
                if let Some(target_hash) = book_hash_filter {
                    if let Some(bh) = &entry.book_hash {
                        bh == target_hash
                    } else {
                        file_key.contains(target_hash)
                    }
                } else {
                    true
                }
            })
            .map(|((_, file_key), entry)| {
                serde_json::json!({
                    "file_key": file_key,
                    "file_size": entry.data.len(),
                    "book_hash": entry.book_hash,
                    "replica_kind": entry.replica_kind,
                    "replica_id": entry.replica_id,
                    "created_at": entry.created_at,
                    "updated_at": entry.updated_at,
                })
            })
            .collect();

        // Sort by created_at desc (matching list.ts default)
        matched_files.sort_by(|a, b| {
            let a_ca = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            let b_ca = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            b_ca.cmp(a_ca)
        });

        let total = matched_files.len();
        let total_pages = total.div_ceil(page_size);

        let from = (page - 1) * page_size;
        let paged_files: Vec<serde_json::Value> = if from >= total {
            Vec::new()
        } else {
            matched_files
                .into_iter()
                .skip(from)
                .take(page_size)
                .collect()
        };

        let body = serde_json::to_vec(&serde_json::json!({
            "files": paged_files,
            "total": total,
            "page": page,
            "pageSize": page_size,
            "totalPages": total_pages,
        }))
        .unwrap();

        Ok(HttpResponse::new(200, body).with_header("Content-Type", "application/json"))
    }

    fn handle_storage_download_ticket(
        &self,
        user_id: &str,
        query: &str,
    ) -> Result<HttpResponse, TransportError> {
        let file_key_param = query
            .split('&')
            .find_map(|part| part.strip_prefix("fileKey=").map(url_decode));

        let file_key = match file_key_param {
            Some(k) if !k.trim().is_empty() => k,
            _ => {
                return Ok(HttpResponse::new(
                    400,
                    b"{\"error\":\"Missing or invalid fileKey\"}".to_vec(),
                ));
            }
        };

        let mut state = self.state.lock().unwrap();
        let file_entry = state.files.get(&(user_id.to_string(), file_key.clone()));
        match file_entry {
            Some(entry) if !entry.deleted => {
                state.next_ticket_id += 1;
                let ticket_id = format!("tkt-{}", state.next_ticket_id);
                let expires_at_ms = state.clock_ms + 1800 * 1000;
                state.signed_download_tickets.insert(
                    ticket_id.clone(),
                    MockTicketInfo {
                        user_id: user_id.to_string(),
                        file_key,
                        expires_at_ms,
                    },
                );
                let download_url = format!("{}/{ticket_id}", state.download_base);
                let body = serde_json::to_vec(&serde_json::json!({
                    "downloadUrl": download_url,
                }))
                .unwrap();
                Ok(HttpResponse::new(200, body).with_header("Content-Type", "application/json"))
            }
            _ => Ok(HttpResponse::new(
                404,
                b"{\"error\":\"File not found\"}".to_vec(),
            )),
        }
    }

    fn handle_storage_download_blob(
        &self,
        ticket_id: &str,
    ) -> Result<HttpResponse, TransportError> {
        let state = self.state.lock().unwrap();
        let ticket = match state.signed_download_tickets.get(ticket_id) {
            Some(t) => t.clone(),
            None => {
                return Ok(HttpResponse::new(
                    404,
                    b"{\"error\":\"Download ticket not found\"}".to_vec(),
                ));
            }
        };

        if state.clock_ms >= ticket.expires_at_ms {
            return Ok(HttpResponse::new(
                403,
                b"{\"error\":\"Download link expired\"}".to_vec(),
            ));
        }

        match state.files.get(&(ticket.user_id, ticket.file_key)) {
            Some(entry) if !entry.deleted => Ok(HttpResponse::new(200, entry.data.clone())
                .with_header("Content-Type", "application/octet-stream")),
            _ => Ok(HttpResponse::new(
                404,
                b"{\"error\":\"File not found\"}".to_vec(),
            )),
        }
    }
}

impl HttpTransport for MockServer {
    fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.handle_request(req)
    }
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in q.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

fn url_decode(input: &str) -> String {
    let mut result = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                result.push(val);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn infer_file_key_metadata(file_key: &str) -> (Option<String>, Option<String>, Option<String>) {
    let parts: Vec<&str> = file_key.split('/').collect();
    if let Some(pos) = parts.iter().position(|&p| p == "Replicas") {
        if pos + 2 < parts.len() {
            let kind = parts[pos + 1].to_string();
            let replica_id = parts[pos + 2].to_string();
            return (None, Some(kind), Some(replica_id));
        }
    }
    if let Some(pos) = parts.iter().position(|&p| p == "Books") {
        if pos + 1 < parts.len() {
            let hash = parts[pos + 1].to_string();
            return (Some(hash), None, None);
        }
    }
    (None, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixture_pagination_boundary() {
        let json_str = include_str!("../../../tests/fixtures/sync/pagination_boundary.json");
        let root: serde_json::Value =
            serde_json::from_str(json_str).expect("parse pagination_boundary.json");

        for case in root["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let endpoint = case["endpoint"].as_str().unwrap();
            let user_id = "usr-01";
            let server = MockServer::new();

            if let Some(records) = case["server_db_records"].as_array() {
                for rec_val in records {
                    let mut book: DbBook = serde_json::from_value(rec_val.clone()).unwrap();
                    if book.user_id.is_none() {
                        book.user_id = Some(user_id.to_string());
                    }
                    server.insert_book(user_id, book);
                }
            }

            let path = if let Some(p) = endpoint.strip_prefix("GET ") {
                p
            } else {
                endpoint
            };
            let url = format!("https://web.readest.com{path}");
            let req = HttpRequest::new("GET", &url)
                .with_header("Authorization", "Bearer test-access-token");
            let resp = server.send(req).expect("mock request success");
            assert_eq!(resp.status, 200, "case {name} HTTP status mismatch");

            let pull_resp: SyncPullResponse = serde_json::from_slice(&resp.body).unwrap();

            if let Some(exp_count) = case["expected"]
                .get("page_records_count")
                .and_then(|v| v.as_u64())
            {
                assert_eq!(
                    pull_resp.books.len(),
                    exp_count as usize,
                    "case {name} page_records_count mismatch"
                );
            }
            if let Some(exp_hashes) = case["expected"]
                .get("returned_hashes")
                .and_then(|v| v.as_array())
            {
                let actual_hashes: Vec<&str> = pull_resp
                    .books
                    .iter()
                    .map(|b| b.book_hash.as_str())
                    .collect();
                let exp_hashes_vec: Vec<&str> =
                    exp_hashes.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(
                    actual_hashes, exp_hashes_vec,
                    "case {name} returned_hashes mismatch"
                );
            }
            if let Some(exp_next_cursor) = case["expected"]
                .get("next_cursor_synced_at")
                .and_then(|v| v.as_str())
            {
                let last_synced = pull_resp
                    .books
                    .last()
                    .and_then(|b| b.synced_at.as_deref())
                    .unwrap_or("");
                assert_eq!(
                    last_synced, exp_next_cursor,
                    "case {name} next_cursor_synced_at mismatch"
                );
            }
            if let Some(books_count) = case["expected"].get("books_count").and_then(|v| v.as_u64())
            {
                assert_eq!(
                    pull_resp.books.len(),
                    books_count as usize,
                    "case {name} books_count mismatch"
                );
            }
            if let Some(first_book_val) = case["expected"].get("first_book") {
                let actual_first = &pull_resp.books[0];
                if let Some(exp_uid) = first_book_val.get("user_id").and_then(|v| v.as_str()) {
                    assert_eq!(
                        actual_first.user_id.as_deref(),
                        Some(exp_uid),
                        "case {name} first_book user_id mismatch"
                    );
                }
                if let Some(exp_hash) = first_book_val.get("book_hash").and_then(|v| v.as_str()) {
                    assert_eq!(
                        &actual_first.book_hash, exp_hash,
                        "case {name} first_book book_hash mismatch"
                    );
                }
                if let Some(exp_title) = first_book_val.get("title").and_then(|v| v.as_str()) {
                    assert_eq!(
                        &actual_first.title, exp_title,
                        "case {name} first_book title mismatch"
                    );
                }
                if let Some(exp_format) = first_book_val.get("format").and_then(|v| v.as_str()) {
                    assert_eq!(
                        actual_first.format.as_str(),
                        exp_format,
                        "case {name} first_book format mismatch"
                    );
                }
                if let Some(exp_author) = first_book_val.get("author").and_then(|v| v.as_str()) {
                    assert_eq!(
                        &actual_first.author, exp_author,
                        "case {name} first_book author mismatch"
                    );
                }
            }
        }
    }

    #[test]
    fn mock_validates_apikey() {
        let server = MockServer::new();
        // Request without apikey
        let req_no_key = HttpRequest::new(
            "POST",
            "https://web.readest.com/auth/v1/token?grant_type=password",
        )
        .with_body(b"{\"email\":\"test@example.com\",\"password\":\"password123\"}".to_vec());
        let resp = server.send(req_no_key).unwrap();
        assert_eq!(resp.status, 401);

        // Request with invalid apikey
        let req_bad_key = HttpRequest::new(
            "POST",
            "https://web.readest.com/auth/v1/token?grant_type=password",
        )
        .with_header("apikey", "wrong-anon-key")
        .with_body(b"{\"email\":\"test@example.com\",\"password\":\"password123\"}".to_vec());
        let resp_bad = server.send(req_bad_key).unwrap();
        assert_eq!(resp_bad.status, 401);

        // Request with valid apikey
        let req_good = HttpRequest::new(
            "POST",
            "https://web.readest.com/auth/v1/token?grant_type=password",
        )
        .with_header(
            "apikey",
            &crate::sync::auth::SyncConfig::default().supabase_anon_key,
        )
        .with_body(b"{\"email\":\"test@example.com\",\"password\":\"password123\"}".to_vec());
        let resp_good = server.send(req_good).unwrap();
        assert_eq!(resp_good.status, 200);
    }

    #[test]
    fn mock_rotates_refresh_token() {
        let server = MockServer::new();
        let anon_key = crate::sync::auth::SyncConfig::default().supabase_anon_key;

        let refresh_body = serde_json::to_vec(&serde_json::json!({
            "refresh_token": "test-refresh-token"
        }))
        .unwrap();

        let req1 = HttpRequest::new(
            "POST",
            "https://web.readest.com/auth/v1/token?grant_type=refresh_token",
        )
        .with_header("apikey", &anon_key)
        .with_body(refresh_body.clone());

        let resp1 = server.send(req1).unwrap();
        assert_eq!(resp1.status, 200);
        let resp_json: serde_json::Value = serde_json::from_slice(&resp1.body).unwrap();
        let new_ref = resp_json["refresh_token"].as_str().unwrap().to_string();

        // Old token must now be invalid!
        let req_old = HttpRequest::new(
            "POST",
            "https://web.readest.com/auth/v1/token?grant_type=refresh_token",
        )
        .with_header("apikey", &anon_key)
        .with_body(refresh_body);
        let resp_old = server.send(req_old).unwrap();
        assert_eq!(resp_old.status, 401);

        // New token works
        let req_new = HttpRequest::new(
            "POST",
            "https://web.readest.com/auth/v1/token?grant_type=refresh_token",
        )
        .with_header("apikey", &anon_key)
        .with_body(serde_json::to_vec(&serde_json::json!({ "refresh_token": new_ref })).unwrap());
        let resp_new = server.send(req_new).unwrap();
        assert_eq!(resp_new.status, 200);
    }

    #[test]
    fn mock_sync_unauthenticated_returns_403() {
        let server = MockServer::new();
        let req = HttpRequest::new("GET", "https://web.readest.com/api/sync?since=0");
        let resp = server.send(req).unwrap();
        assert_eq!(resp.status, 403);
    }

    #[test]
    fn mock_limit_zero_returns_400() {
        let server = MockServer::new();
        let req = HttpRequest::new(
            "GET",
            "https://web.readest.com/api/sync?since=0&type=books&limit=0",
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp = server.send(req).unwrap();
        assert_eq!(resp.status, 400);
    }

    #[test]
    fn mock_meta_hash_filtering() {
        let server = MockServer::new();
        server.insert_book(
            "usr-01",
            DbBook {
                book_hash: "hash-01".to_string(),
                meta_hash: Some("meta-999".to_string()),
                title: "Book with meta".to_string(),
                synced_at: Some("2026-11-05T12:00:00.000Z".to_string()),
                ..Default::default()
            },
        );

        // Query by meta_hash
        let req = HttpRequest::new(
            "GET",
            "https://web.readest.com/api/sync?since=0&type=books&meta_hash=meta-999",
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp = server.send(req).unwrap();
        assert_eq!(resp.status, 200);
        let pull_resp: SyncPullResponse = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(pull_resp.books.len(), 1);
        assert_eq!(pull_resp.books[0].book_hash, "hash-01");
    }

    #[test]
    fn mock_storage_download_ticket_and_blob_lifecycle() {
        let server = MockServer::new();
        let user_id = "usr-01";
        let file_key = "usr-01/Readest/Books/book-abc/book-abc.epub";
        let content = b"Sample EPUB content bytes";

        server.insert_file(user_id, file_key, content.to_vec());

        // 1. Unauthenticated request -> 403
        let unauth_req = HttpRequest::new(
            "GET",
            format!("https://web.readest.com/api/storage/download?fileKey={file_key}"),
        );
        let unauth_resp = server.send(unauth_req).unwrap();
        assert_eq!(unauth_resp.status, 403);

        // 2. Missing fileKey -> 400
        let req_no_key = HttpRequest::new(
            "GET",
            "https://web.readest.com/api/storage/download?fileKey=",
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp_no_key = server.send(req_no_key).unwrap();
        assert_eq!(resp_no_key.status, 400);

        // 3. Nonexistent fileKey -> 404
        let req_missing = HttpRequest::new(
            "GET",
            "https://web.readest.com/api/storage/download?fileKey=nonexistent",
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp_missing = server.send(req_missing).unwrap();
        assert_eq!(resp_missing.status, 404);

        // 4. Valid fileKey -> 200 with downloadUrl
        let req_valid = HttpRequest::new(
            "GET",
            format!("https://web.readest.com/api/storage/download?fileKey={file_key}"),
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp_valid = server.send(req_valid).unwrap();
        assert_eq!(resp_valid.status, 200);

        let body_json: serde_json::Value = serde_json::from_slice(&resp_valid.body).unwrap();
        let download_url = body_json["downloadUrl"].as_str().unwrap();
        assert!(download_url.starts_with("https://mock.storage.local/download/"));

        // 5. Download blob via signed URL (public, no auth header needed)
        let blob_req = HttpRequest::new("GET", download_url);
        let blob_resp = server.send(blob_req).unwrap();
        assert_eq!(blob_resp.status, 200);
        assert_eq!(blob_resp.body, content);

        // 6. Deleted file -> ticket request returns 404
        server.mark_file_deleted(user_id, file_key);
        let req_deleted = HttpRequest::new(
            "GET",
            format!("https://web.readest.com/api/storage/download?fileKey={file_key}"),
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp_deleted = server.send(req_deleted).unwrap();
        assert_eq!(resp_deleted.status, 404);
    }

    #[test]
    fn mock_download_base_shapes_the_signed_url() {
        let server = MockServer::new();
        server.set_download_base("http://192.168.1.20:8090/download/");
        let file_key = "usr-01/Readest/Books/book-abc/book-abc.epub";
        let content = b"Sample EPUB content bytes";
        server.insert_file("usr-01", file_key, content.to_vec());

        let req = HttpRequest::new(
            "GET",
            format!("https://web.readest.com/api/storage/download?fileKey={file_key}"),
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp = server.send(req).unwrap();
        assert_eq!(resp.status, 200);

        let body_json: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        let download_url = body_json["downloadUrl"].as_str().unwrap();
        assert!(download_url.starts_with("http://192.168.1.20:8090/download/"));

        let blob_resp = server.send(HttpRequest::new("GET", download_url)).unwrap();
        assert_eq!(blob_resp.status, 200);
        assert_eq!(blob_resp.body, content);
    }

    #[test]
    fn mock_storage_list_endpoint() {
        let server = MockServer::new();
        let user_id = "usr-01";

        server.insert_file(
            user_id,
            "usr-01/Readest/Books/hash1/MyBook.epub",
            b"Epub content".to_vec(),
        );
        server.insert_file(
            user_id,
            "usr-01/Readest/Books/hash1/cover.png",
            b"Cover png".to_vec(),
        );
        server.insert_file(
            user_id,
            "usr-01/Readest/Books/hash2/OtherBook.pdf",
            b"Pdf content".to_vec(),
        );

        // 1. Query files for hash1
        let req = HttpRequest::new(
            "GET",
            "https://web.readest.com/api/storage/list?bookHash=hash1",
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp = server.send(req).unwrap();
        assert_eq!(resp.status, 200);

        let json: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(json["total"], 2);
        let files = json["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .any(|f| f["file_key"] == "usr-01/Readest/Books/hash1/MyBook.epub"));
        assert!(files
            .iter()
            .any(|f| f["file_key"] == "usr-01/Readest/Books/hash1/cover.png"));

        // 2. Query with pagination
        let req_paged = HttpRequest::new(
            "GET",
            "https://web.readest.com/api/storage/list?pageSize=1&page=1",
        )
        .with_header("Authorization", "Bearer test-access-token");
        let resp_paged = server.send(req_paged).unwrap();
        assert_eq!(resp_paged.status, 200);
        let paged_json: serde_json::Value = serde_json::from_slice(&resp_paged.body).unwrap();
        assert_eq!(paged_json["total"], 3);
        assert_eq!(paged_json["page"], 1);
        assert_eq!(paged_json["pageSize"], 1);
        assert_eq!(paged_json["totalPages"], 3);
        assert_eq!(paged_json["files"].as_array().unwrap().len(), 1);
    }
}
