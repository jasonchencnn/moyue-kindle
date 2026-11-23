// SPDX-License-Identifier: AGPL-3.0-or-later
//! The network settings on disk, and reading them from files the reader
//! writes by hand.
//!
//! Everything lives in one file, `accounts.json`, owner-only (mode 0600) in
//! a folder only the app's user owns and may write ([`private_dir`]): on
//! the device `/var/local/aristotle`, root's, on the ext3 partition, never
//! the FAT userstore, which has no permissions and is readable by anyone
//! with a USB cable. The sync password is not stored, only its MD5 key
//! (which still works as a password for that one server). Catalog
//! passwords are stored as given, because Basic and Digest need them.
//!
//! Until the on-screen keyboard lands, settings come from two files next to
//! the app, `sync.json` and `catalogs.json` (formats in `docs/library.md`).
//! They are read when they change, and their passwords are removed from
//! them once stored.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use inkread_core::jianguoyun::{JianguoyunConfig, DEFAULT_WEBDAV_BASE};

use crate::auth::{md5_hex, Credentials};
use crate::kosync::{self, Account};
use crate::policy::entry_for;
use crate::url::Url;

pub const FILE: &str = "accounts.json";
/// Allowlist tags (`# aristotle:<feature>`).
pub const SYNC_FEATURE: &str = "sync";
pub const READEST_FEATURE: &str = "readest";
pub const OPDS_FEATURE: &str = "opds";
pub const JIANGUOYUN_FEATURE: &str = "jianguoyun";

fn yes() -> bool {
    true
}

/// Which record type a full-sync cursor belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    Books,
    Configs,
    Notes,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadestSettings {
    pub email: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_queue: Vec<String>,
    /// Push the open book when it is closed, when the device sleeps, and when
    /// the reader quits. Missing in older settings and true then: that is what
    /// this client did before the switch existed.
    #[serde(default = "yes")]
    pub auto_push: bool,
    /// The newest server timestamp seen for each record type (`sync-contract`
    /// §1.2): the `since` cursor of the next full sync. Kept apart per type
    /// because the server filters `books` by `synced_at` but `configs` and
    /// `notes` by `updated_at`/`deleted_at`; one shared cursor would either
    /// skip rows or re-pull them. `None` falls back to `last_sync_time`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_books: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_configs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_notes: Option<i64>,
}

impl std::fmt::Debug for ReadestSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadestSettings")
            .field("email", &self.email)
            .field("refresh_token", &"[REDACTED]")
            .field("access_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("last_sync_time", &self.last_sync_time)
            .field("pending_queue", &self.pending_queue)
            .field("auto_push", &self.auto_push)
            .field("cursor_books", &self.cursor_books)
            .field("cursor_configs", &self.cursor_configs)
            .field("cursor_notes", &self.cursor_notes)
            .finish()
    }
}

impl ReadestSettings {
    /// The settings a successful sign-in leaves behind.
    ///
    /// A re-login keeps what belongs to this device and this account's
    /// history: the automatic-push choice, the sync cursors and the books
    /// whose push failed.
    pub fn after_login(
        email: &str,
        session: &inkread_core::sync::Session,
        old: Option<&ReadestSettings>,
    ) -> Self {
        Self {
            email: email.to_string(),
            refresh_token: session.refresh_token.clone(),
            access_token: Some(session.access_token.clone()),
            expires_at: session.expires_at,
            last_sync_time: old.and_then(|o| o.last_sync_time),
            pending_queue: old.map(|o| o.pending_queue.clone()).unwrap_or_default(),
            auto_push: old.is_none_or(|o| o.auto_push),
            cursor_books: old.and_then(|o| o.cursor_books),
            cursor_configs: old.and_then(|o| o.cursor_configs),
            cursor_notes: old.and_then(|o| o.cursor_notes),
        }
    }

    pub fn to_session(&self) -> inkread_core::sync::Session {
        let access_token = self.access_token.clone().unwrap_or_default();
        // Only the refresh token is stored: with no access token in memory
        // (a fresh start) the session must look expired so the first request
        // refreshes instead of sending an empty Bearer header.
        let expires_at = if access_token.is_empty() {
            0
        } else {
            self.expires_at
        };
        inkread_core::sync::Session::new(access_token, self.refresh_token.clone(), expires_at)
    }

    /// Whether closing, sleeping or quitting should push the open book.
    pub fn auto_push_enabled(&self) -> bool {
        self.auto_push
    }

    /// Turn the automatic push on or off; callers save the settings.
    pub fn set_auto_push(&mut self, on: bool) {
        self.auto_push = on;
    }

    /// The `since` cursor for one record type, falling back to the last full
    /// sync for settings written before the per-type cursors existed.
    pub fn cursor(&self, kind: CursorKind) -> i64 {
        let own = match kind {
            CursorKind::Books => self.cursor_books,
            CursorKind::Configs => self.cursor_configs,
            CursorKind::Notes => self.cursor_notes,
        };
        own.or(self.last_sync_time).unwrap_or(0)
    }

    /// Remember the newest server timestamp of a pulled record type.
    pub fn advance_cursor(&mut self, kind: CursorKind, observed: Option<i64>) {
        let Some(ms) = observed else { return };
        let slot = match kind {
            CursorKind::Books => &mut self.cursor_books,
            CursorKind::Configs => &mut self.cursor_configs,
            CursorKind::Notes => &mut self.cursor_notes,
        };
        *slot = Some(slot.map_or(ms, |old| old.max(ms)));
    }

    /// Remember that `book` still has changes to push.
    pub fn enqueue_push(&mut self, book: &str) {
        if !self.pending_queue.iter().any(|b| b == book) {
            self.pending_queue.push(book.to_string());
        }
    }

    /// Forget `book` after its push went through.
    pub fn dequeue_push(&mut self, book: &str) {
        self.pending_queue.retain(|b| b != book);
    }

    /// How many books still have changes to push.
    pub fn pending_pushes(&self) -> usize {
        self.pending_queue.len()
    }

    pub fn update_session(&mut self, session: &inkread_core::sync::Session) {
        self.access_token = Some(session.access_token.clone());
        self.refresh_token = session.refresh_token.clone();
        self.expires_at = session.expires_at;
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSettings {
    pub server: String,
    pub username: String,
    /// `md5(password)`; empty until a password was given.
    #[serde(default)]
    pub key: String,
    /// Pull on open, push on close.
    #[serde(default = "yes")]
    pub auto: bool,
}

impl std::fmt::Debug for SyncSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncSettings")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("auto", &self.auto)
            .finish_non_exhaustive()
    }
}

impl SyncSettings {
    /// The account, once server, user and key are all there.
    pub fn account(&self) -> Option<Account> {
        (!self.server.is_empty() && !self.username.is_empty() && !self.key.is_empty()).then(|| {
            Account {
                server: self.server.clone(),
                username: self.username.clone(),
                key: self.key.clone(),
            }
        })
    }
}

/// The Jianguoyun (Nutstore) WebDAV account and its sync bookkeeping
/// (`sync-contract.md` §15.1, §15.4).
///
/// The password is the third-party application password from Nutstore's own
/// security page, never the account password. It is stored as given because
/// WebDAV Basic authentication needs it on every request, and
/// [`fmt::Debug`] redacts it with `[REDACTED]`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JianguoyunSettings {
    /// WebDAV endpoint; empty means the official `dav.jianguoyun.com`.
    #[serde(default)]
    pub server_url: String,
    /// Nutstore account e-mail.
    #[serde(default)]
    pub username: String,
    /// Third-party application password, never the login password.
    #[serde(default)]
    pub password: String,
    /// Decoded sub-directory under the endpoint, e.g. `书库`.
    #[serde(default)]
    pub remote_dir: String,
    /// Push the open book's progress when it is closed or the device sleeps.
    #[serde(default = "yes")]
    pub auto_push: bool,
    /// The newest successful sync, in milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_time: Option<i64>,
    /// Books whose push failed and must be retried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_queue: Vec<String>,
}

impl std::fmt::Debug for JianguoyunSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JianguoyunSettings")
            .field("server_url", &self.server_url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("remote_dir", &self.remote_dir)
            .field("auto_push", &self.auto_push)
            .field("last_sync_time", &self.last_sync_time)
            .field("pending_queue", &self.pending_queue)
            .finish()
    }
}

impl JianguoyunSettings {
    /// A signed-in account with nothing remembered yet.
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
            auto_push: true,
            last_sync_time: None,
            pending_queue: Vec::new(),
        }
    }

    /// The official endpoint with no remote sub-directory.
    pub fn official(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::new(DEFAULT_WEBDAV_BASE, username, password, "")
    }

    /// The platform-independent client configuration these settings name.
    ///
    /// An empty `server_url` falls back to [`DEFAULT_WEBDAV_BASE`], so a
    /// `jianguoyun.json` that only carries the account still works.
    pub fn to_core_config(&self) -> JianguoyunConfig {
        let server_url = match self.server_url.trim() {
            "" => DEFAULT_WEBDAV_BASE,
            url => url,
        };
        JianguoyunConfig::new(
            server_url,
            self.username.trim(),
            self.password.clone(),
            self.remote_dir.trim(),
        )
    }

    /// Remember that `book` still has changes to push.
    pub fn enqueue_push(&mut self, book: &str) {
        if !self.pending_queue.iter().any(|pending| pending == book) {
            self.pending_queue.push(book.to_string());
        }
    }

    /// Forget `book` after its push went through.
    pub fn dequeue_push(&mut self, book: &str) {
        self.pending_queue.retain(|pending| pending != book);
    }

    /// How many books still have changes to push.
    pub fn pending_pushes(&self) -> usize {
        self.pending_queue.len()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSettings {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl std::fmt::Debug for CatalogSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogSettings")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl CatalogSettings {
    pub fn credentials(&self) -> Option<Credentials> {
        Some(Credentials {
            username: self.username.clone().filter(|u| !u.is_empty())?,
            password: self.password.clone().unwrap_or_default(),
        })
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadestEndpoints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supabase_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supabase_anon_key: Option<String>,
}

impl std::fmt::Debug for ReadestEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadestEndpoints")
            .field("api_base", &self.api_base)
            .field("supabase_url", &self.supabase_url)
            .field(
                "supabase_anon_key",
                &self.supabase_anon_key.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl ReadestEndpoints {
    /// No endpoint has been set: the defaults apply.
    pub fn is_empty(&self) -> bool {
        self.api_base.is_none() && self.supabase_url.is_none() && self.supabase_anon_key.is_none()
    }
}

/// Which Readest endpoint a typed value is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadestEndpointField {
    ApiBase,
    SupabaseUrl,
    AnonKey,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accounts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readest: Option<ReadestSettings>,
    /// The Jianguoyun (Nutstore) account, when the reader set one up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jianguoyun: Option<JianguoyunSettings>,
    /// The Readest endpoints, independent of the account so they survive a
    /// sign-out and can be set before the first sign-in. This is the only
    /// source; `readest_config()` falls back to the defaults for the parts
    /// left empty.
    #[serde(default, skip_serializing_if = "ReadestEndpoints::is_empty")]
    pub readest_endpoints: ReadestEndpoints,
    #[serde(default)]
    pub catalogs: Vec<CatalogSettings>,
    /// This reader's KOSync device ID, made once.
    #[serde(default)]
    pub device_id: String,
    /// MD5 of each import file as last read, so a file is read once per
    /// change.
    #[serde(default)]
    pub imported: BTreeMap<String, String>,
}

/// What reading an import file did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Imported {
    /// There is no such file.
    Missing,
    /// Read before and unchanged since.
    Unchanged,
    Loaded,
}

#[derive(Debug)]
pub enum ImportError {
    Io(io::Error),
    Json(String),
    /// A field the settings cannot use, such as a bad server address.
    Invalid(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Io(e) => e.fmt(f),
            ImportError::Json(e) => write!(f, "not valid JSON: {e}"),
            ImportError::Invalid(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        ImportError::Io(e)
    }
}

#[derive(Deserialize, Serialize)]
struct SyncFile {
    server: String,
    username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto: Option<bool>,
}

#[derive(Deserialize, Serialize)]
struct CatalogsFile {
    catalogs: Vec<CatalogSettings>,
}

/// `jianguoyun.json`, the file the reader writes by hand (or copies over USB)
/// until the on-screen keyboard is not the only way in.
#[derive(Deserialize, Serialize)]
struct JianguoyunFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_url: Option<String>,
    username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_push: Option<bool>,
}

/// Make sure `dir` can hold secrets: create it if missing, and refuse it
/// unless it is a real directory (not a symlink) owned by this process's
/// user with no permission bits for group or others.
///
/// On the device the app runs as root, and the folder is made mode 000 as
/// the agent's install makes `/var/local/aristotle`: the stock framework
/// runs `chgrp -R` and `chmod -R g=u` over `/var/local` on every start
/// (`device/README.md`, "Security"), so the group would get whatever the
/// owner has, and root needs no bits. Elsewhere (tests, a developer's
/// folder) it is made 0700.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if !dir.exists() {
        let mode = if me == 0 { 0o000 } else { 0o700 };
        fs::DirBuilder::new()
            .recursive(true)
            .mode(mode)
            .create(dir)?;
    }
    let m = fs::symlink_metadata(dir)?;
    let bad = |why: &str| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: {why}; not storing passwords there", dir.display()),
        ))
    };
    if !m.file_type().is_dir() {
        return bad("not a directory");
    }
    if m.uid() != me {
        return bad("owned by another user");
    }
    if m.mode() & 0o077 != 0 {
        return bad("open to other users");
    }
    Ok(())
}

/// Write `data` to `path` owner-only, through a synced temporary file.
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = path.with_extension("json.tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    // A file that existed with wider permissions keeps them through the
    // open; narrow them (a no-op on FAT).
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    fs::rename(&tmp, path)
}

fn valid_url(s: &str, what: &str) -> Result<Url, ImportError> {
    Url::parse(s).map_err(|_| {
        ImportError::Invalid(format!(
            "{what}: {s:?} is not an http:// or https:// address"
        ))
    })
}

impl Accounts {
    /// Read `accounts.json`, moving endpoints that older archives kept inside
    /// `readest` into [`Accounts::readest_endpoints`].
    pub fn load(dir: &Path) -> Accounts {
        let Ok(bytes) = fs::read(dir.join(FILE)) else {
            return Accounts::default();
        };
        let Ok(mut accounts) = serde_json::from_slice::<Accounts>(&bytes) else {
            return Accounts::default();
        };
        if accounts.readest_endpoints.is_empty() {
            #[derive(Deserialize)]
            struct Legacy {
                #[serde(default)]
                readest: Option<ReadestEndpoints>,
            }
            if let Ok(legacy) = serde_json::from_slice::<Legacy>(&bytes) {
                accounts.readest_endpoints = legacy.readest.unwrap_or_default();
            }
        }
        accounts.migrate_legacy_official_endpoints();
        accounts
    }

    /// Treat the former official authentication hosts as "not set".
    ///
    /// Before 2026-11-11 the official Supabase project was reached directly at
    /// `https://vbsyxfusjjqdxkjqlysc.supabase.co`; that host now resets the
    /// connection on some networks, and the official client reaches GoTrue
    /// through `https://auth-cf.readest.com` instead (`docs/sync-contract.md`
    /// §10.3). A device that saved the old default explicitly (an early
    /// archive, or the settings screen before the switch) would keep dialling
    /// the dead host forever and report a network failure on every request that
    /// needs a token refresh, while a request with a live token still worked.
    ///
    /// Only the two hosts that *were* the official default are rewritten; a
    /// self-hosted endpoint is left exactly as the user typed it.
    fn migrate_legacy_official_endpoints(&mut self) {
        const LEGACY_SUPABASE: [&str; 2] = [
            "https://vbsyxfusjjqdxkjqlysc.supabase.co",
            "https://vbsyxfusjjqdxkjqlysc.supabase.co/",
        ];
        if self
            .readest_endpoints
            .supabase_url
            .as_deref()
            .is_some_and(|u| LEGACY_SUPABASE.contains(&u.trim()))
        {
            self.readest_endpoints.supabase_url = None;
        }
    }

    /// Save into `dir`, which [`private_dir`] must accept.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        private_dir(dir)?;
        let data = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_private(&dir.join(FILE), &data)
    }

    /// The Readest connection settings: the configured endpoints, with the
    /// official ones filling whatever is left empty. Every Readest path
    /// (login, token refresh, sync, cloud, status screen, allowlist) reads
    /// its configuration here.
    pub fn readest_config(&self) -> inkread_core::sync::SyncConfig {
        let def = inkread_core::sync::SyncConfig::default();
        inkread_core::sync::SyncConfig {
            api_base: self
                .readest_endpoints
                .api_base
                .clone()
                .unwrap_or(def.api_base),
            supabase_url: self
                .readest_endpoints
                .supabase_url
                .clone()
                .unwrap_or(def.supabase_url),
            supabase_anon_key: self
                .readest_endpoints
                .supabase_anon_key
                .clone()
                .unwrap_or(def.supabase_anon_key),
        }
    }

    /// Store one typed Readest endpoint, whether or not an account is signed
    /// in. Empty text clears the field so the official default applies again,
    /// and so does a value equal to that default: the stored endpoints then
    /// stay empty, which is what "restore defaults" leaves behind.
    ///
    /// The two addresses must be http(s) and pass
    /// [`crate::endpoint::is_allowed`]; anything else is refused with
    /// [`ImportError::Invalid`] and the old value stays.
    pub fn set_readest_endpoint(
        &mut self,
        field: ReadestEndpointField,
        value: &str,
    ) -> Result<(), ImportError> {
        let value = value.trim();
        let defaults = inkread_core::sync::SyncConfig::default();
        let store = |slot: &mut Option<String>, default: String, what: &str| {
            if value.is_empty() || value == default {
                *slot = None;
                return Ok(());
            }
            let url = Url::parse(value).map_err(|_| {
                ImportError::Invalid(format!(
                    "{what}: {value:?} is not an http:// or https:// address"
                ))
            })?;
            if !crate::endpoint::is_allowed(&url) {
                return Err(ImportError::Invalid(format!(
                    "{what}: {value:?} may not be used as a Readest endpoint"
                )));
            }
            *slot = Some(value.to_string());
            Ok(())
        };
        match field {
            ReadestEndpointField::ApiBase => {
                let slot = &mut self.readest_endpoints.api_base;
                let default = defaults.api_base;
                store(slot, default, "API base")
            }
            ReadestEndpointField::SupabaseUrl => {
                let slot = &mut self.readest_endpoints.supabase_url;
                let default = defaults.supabase_url;
                store(slot, default, "Supabase URL")
            }
            ReadestEndpointField::AnonKey => {
                self.readest_endpoints.supabase_anon_key = (!value.is_empty()
                    && value != defaults.supabase_anon_key)
                    .then(|| value.to_string());
                Ok(())
            }
        }
    }

    /// Forget every custom Readest endpoint; the official defaults apply.
    pub fn reset_readest_endpoints(&mut self) {
        self.readest_endpoints = ReadestEndpoints::default();
    }

    /// The KOSync device ID: 32 upper-case hex digits, as KOReader makes
    /// them, generated on first use.
    pub fn device_id(&mut self) -> String {
        if self.device_id.len() != 32 {
            self.device_id = crate::auth::random_hex().to_ascii_uppercase();
        }
        self.device_id.clone()
    }

    /// The `net-allow` entries these settings need, with their feature.
    pub fn allow_entries(&self) -> BTreeSet<(String, &'static str)> {
        let mut out = BTreeSet::new();
        if let Some(e) = self
            .sync
            .as_ref()
            .and_then(|s| Url::parse(&s.server).ok())
            .and_then(|u| entry_for(&u))
        {
            out.insert((e, SYNC_FEATURE));
        }
        // Readest endpoints are kept without an account too, so an edit made
        // before the first sign-in must reach the allowlist as well.
        if self.readest.is_some() || !self.readest_endpoints.is_empty() {
            out.extend(self.readest_allow_entries());
        }
        // The Nutstore endpoint is registered only once the reader set the
        // account up; the login page asks for `jianguoyun_allow_entries`
        // before that, so a fresh `scoped` device can reach the official host.
        if self.jianguoyun.is_some() {
            out.extend(self.jianguoyun_allow_entries());
        }
        for c in &self.catalogs {
            if let Some(e) = Url::parse(&c.url).ok().and_then(|u| entry_for(&u)) {
                out.insert((e, OPDS_FEATURE));
            }
        }
        out
    }

    /// The allowlist entries Readest needs: the configured endpoints, or the
    /// official ones before the first sign-in so the login request is allowed.
    pub fn readest_allow_entries(&self) -> BTreeSet<(String, &'static str)> {
        let mut out = BTreeSet::new();
        let config = self.readest_config();
        for endpoint in [&config.api_base, &config.supabase_url] {
            if let Some(e) = Url::parse(endpoint)
                .ok()
                .filter(crate::endpoint::is_allowed)
                .and_then(|u| entry_for(&u))
            {
                out.insert((e, READEST_FEATURE));
            }
        }
        out
    }

    /// The allowlist entry the Nutstore endpoint needs, with
    /// [`JIANGUOYUN_FEATURE`].
    ///
    /// Without an account this is the official `dav.jianguoyun.com:443`, so
    /// the first sign-in on a `scoped` device is not refused before the reader
    /// has saved anything. A custom endpoint that plain HTTP outside loopback
    /// and the private IPv4 ranges would leak the app password is not
    /// registered.
    pub fn jianguoyun_allow_entries(&self) -> BTreeSet<(String, &'static str)> {
        let config = match &self.jianguoyun {
            Some(settings) => settings.to_core_config(),
            None => JianguoyunConfig::default(),
        };
        let mut out = BTreeSet::new();
        if let Some(e) = config
            .normalized_base_url()
            .ok()
            .and_then(|url| Url::parse(&url).ok())
            .filter(crate::endpoint::is_allowed)
            .and_then(|url| entry_for(&url))
        {
            out.insert((e, JIANGUOYUN_FEATURE));
        }
        out
    }

    /// Set the sync server, user or password from typed text (the
    /// keyboard's `TextInput`). A new server or user forgets the key.
    pub fn set_sync_field(&mut self, field: SyncField, value: &str) -> Result<(), ImportError> {
        let value = value.trim();
        let s = self.sync.get_or_insert_with(|| SyncSettings {
            server: String::new(),
            username: String::new(),
            key: String::new(),
            auto: true,
        });
        match field {
            SyncField::Server => {
                let url = valid_url(value, "server")?;
                let url = url.to_string().trim_end_matches('/').to_string();
                if url != s.server {
                    s.server = url;
                    s.key.clear();
                }
            }
            SyncField::Username => {
                if value != s.username {
                    s.username = value.to_string();
                    s.key.clear();
                }
            }
            SyncField::Password => s.key = kosync::key(value),
        }
        Ok(())
    }

    /// Add a catalog by address (typed); its name is the host until the
    /// first feed gives a title.
    pub fn add_catalog(&mut self, url: &str) -> Result<usize, ImportError> {
        let u = valid_url(url.trim(), "catalog")?;
        if let Some(i) = self.catalogs.iter().position(|c| c.url == u.to_string()) {
            return Ok(i);
        }
        self.catalogs.push(CatalogSettings {
            name: u.host().to_string(),
            url: u.to_string(),
            username: None,
            password: None,
        });
        Ok(self.catalogs.len() - 1)
    }

    /// Read `sync.json` if it changed (or always, with `force`).
    pub fn import_sync(&mut self, file: &Path, force: bool) -> Result<Imported, ImportError> {
        let Some(bytes) = read_if_changed(&self.imported, file, force)? else {
            return Ok(if file.exists() {
                Imported::Unchanged
            } else {
                Imported::Missing
            });
        };
        let mut f: SyncFile =
            serde_json::from_slice(&bytes).map_err(|e| ImportError::Json(e.to_string()))?;
        let url = valid_url(&f.server, "server")?;
        let server = url.to_string().trim_end_matches('/').to_string();
        let username = f.username.trim().to_string();
        if username.is_empty() {
            return Err(ImportError::Invalid("username is empty".into()));
        }
        let old = self.sync.take();
        let same = old
            .as_ref()
            .is_some_and(|o| o.server == server && o.username == username);
        let key = match &f.password {
            Some(p) => kosync::key(p),
            None if same => old.as_ref().map(|o| o.key.clone()).unwrap_or_default(),
            None => String::new(),
        };
        self.sync = Some(SyncSettings {
            server,
            username,
            key,
            auto: f.auto.or(old.map(|o| o.auto)).unwrap_or(true),
        });
        let mut stored = bytes;
        if f.password.take().is_some() {
            stored = pretty(&f)?;
            write_private(file, &stored)?;
        }
        self.imported.insert(file_key(file), md5_hex(&stored));
        Ok(Imported::Loaded)
    }

    /// Read `catalogs.json` if it changed (or always, with `force`).
    /// Catalogs are matched by name: a listed one replaces ours (keeping
    /// our password when the file has none for the same address and user),
    /// a new one is added, and ours that the file doesn't list stay.
    pub fn import_catalogs(&mut self, file: &Path, force: bool) -> Result<Imported, ImportError> {
        let Some(bytes) = read_if_changed(&self.imported, file, force)? else {
            return Ok(if file.exists() {
                Imported::Unchanged
            } else {
                Imported::Missing
            });
        };
        let mut f: CatalogsFile =
            serde_json::from_slice(&bytes).map_err(|e| ImportError::Json(e.to_string()))?;
        let mut had_passwords = false;
        for c in &mut f.catalogs {
            let url = valid_url(&c.url, &c.name)?;
            let mut new = CatalogSettings {
                name: if c.name.trim().is_empty() {
                    url.host().to_string()
                } else {
                    c.name.trim().to_string()
                },
                url: url.to_string(),
                username: c.username.clone().filter(|u| !u.trim().is_empty()),
                password: c.password.take(),
            };
            had_passwords |= new.password.is_some();
            let at = self
                .catalogs
                .iter()
                .position(|o| o.name.to_lowercase() == new.name.to_lowercase());
            match at {
                Some(i) => {
                    let o = &self.catalogs[i];
                    if new.password.is_none() && o.url == new.url && o.username == new.username {
                        new.password = o.password.clone();
                    }
                    self.catalogs[i] = new;
                }
                None => self.catalogs.push(new),
            }
        }
        let mut stored = bytes;
        if had_passwords {
            stored = pretty(&f)?;
            write_private(file, &stored)?;
        }
        self.imported.insert(file_key(file), md5_hex(&stored));
        Ok(Imported::Loaded)
    }

    /// Read `jianguoyun.json` if it changed (or always, with `force`).
    ///
    /// The file carries `server_url` (optional; the official endpoint when
    /// absent), `username`, `password`, `remote_dir` (optional) and `auto_push`
    /// (optional). The password is stored in `accounts.json` and then erased
    /// from the import file, exactly like `sync.json` and `catalogs.json`; a
    /// later read of the rewritten file (no password) keeps the stored one as
    /// long as the server and the account did not change.
    pub fn import_jianguoyun(&mut self, file: &Path, force: bool) -> Result<Imported, ImportError> {
        let Some(bytes) = read_if_changed(&self.imported, file, force)? else {
            return Ok(if file.exists() {
                Imported::Unchanged
            } else {
                Imported::Missing
            });
        };
        let mut f: JianguoyunFile =
            serde_json::from_slice(&bytes).map_err(|e| ImportError::Json(e.to_string()))?;
        let server_url = match f.server_url.as_deref().map(str::trim) {
            Some("") | None => DEFAULT_WEBDAV_BASE.trim_end_matches('/').to_string(),
            Some(url) => valid_url(url, "server_url")?
                .to_string()
                .trim_end_matches('/')
                .to_string(),
        };
        let username = f.username.trim().to_string();
        if username.is_empty() {
            return Err(ImportError::Invalid("username is empty".into()));
        }
        let remote_dir = f.remote_dir.as_deref().unwrap_or("").trim().to_string();
        // Validating the pair before storing keeps a `..` or a plain-HTTP
        // public endpoint out of the settings instead of failing every request
        // later, when the reader has no idea which field is wrong.
        let config = JianguoyunConfig::new(&server_url, &username, "", &remote_dir);
        let normalized = config
            .normalized_base_url()
            .map_err(|e| ImportError::Invalid(e.to_string()))?;
        let normalized_url = valid_url(normalized.trim_end_matches('/'), "server_url")?;
        if !crate::endpoint::is_allowed(&normalized_url) {
            return Err(ImportError::Invalid(format!(
                "server_url: {server_url:?} may not be used for Jianguoyun"
            )));
        }
        let old = self.jianguoyun.take();
        let same = old.as_ref().is_some_and(|o| {
            canonical_server_url(&o.server_url) == server_url && o.username == username
        });
        let password = match f
            .password
            .as_deref()
            .filter(|password| !password.is_empty())
        {
            Some(password) => password.to_string(),
            None if same => old.as_ref().map(|o| o.password.clone()).unwrap_or_default(),
            None => String::new(),
        };
        self.jianguoyun = Some(JianguoyunSettings {
            server_url: server_url.clone(),
            username,
            password,
            remote_dir,
            auto_push: f
                .auto_push
                .or(old.as_ref().map(|o| o.auto_push))
                .unwrap_or(true),
            last_sync_time: old.as_ref().and_then(|o| o.last_sync_time),
            pending_queue: old.map(|o| o.pending_queue).unwrap_or_default(),
        });
        let mut stored = bytes;
        if f.password.take().is_some() {
            // The rewritten file keeps the endpoint the reader left out: a
            // bare account still names the official server afterwards.
            f.server_url = Some(server_url);
            stored = pretty(&f)?;
            write_private(file, &stored)?;
        }
        self.imported.insert(file_key(file), md5_hex(&stored));
        Ok(Imported::Loaded)
    }
}

/// Which sync setting a line of typed text is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncField {
    Server,
    Username,
    Password,
}

fn file_key(file: &Path) -> String {
    file.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// The form of a WebDAV endpoint used to decide whether two accounts are the
/// same: the parsed URL without a trailing slash, so a saved default with `/`
/// and an import file without it still match.
fn canonical_server_url(raw: &str) -> String {
    let trimmed = raw.trim();
    match Url::parse(trimmed) {
        Ok(url) => url.to_string().trim_end_matches('/').to_string(),
        Err(_) => trimmed.trim_end_matches('/').to_string(),
    }
}

fn read_if_changed(
    seen: &BTreeMap<String, String>,
    file: &Path,
    force: bool,
) -> Result<Option<Vec<u8>>, ImportError> {
    let bytes = match fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !force && seen.get(&file_key(file)) == Some(&md5_hex(&bytes)) {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn pretty<T: Serialize>(v: &T) -> Result<Vec<u8>, ImportError> {
    let mut b = serde_json::to_vec_pretty(v).map_err(|e| ImportError::Json(e.to_string()))?;
    b.push(b'\n');
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn testdir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir()
            .join("aristotle-net-tests")
            .join(format!("{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        // Owner-only, as private_dir wants a folder for secrets.
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    /// A signed-in settings value with nothing remembered yet.
    fn blank() -> ReadestSettings {
        ReadestSettings::after_login(
            "reader@example.com",
            &inkread_core::sync::Session::new("", "", 0),
            None,
        )
    }

    #[test]
    fn sync_json_is_read_once_and_loses_its_password() {
        let dir = testdir("sync-import");
        let file = dir.join("sync.json");
        fs::write(
            &file,
            r#"{"server": "http://192.168.15.201:17200/", "username": "anna", "password": "test"}"#,
        )
        .unwrap();
        let mut a = Accounts::default();
        assert_eq!(a.import_sync(&file, false).unwrap(), Imported::Loaded);
        let s = a.sync.clone().unwrap();
        assert_eq!(s.server, "http://192.168.15.201:17200");
        assert_eq!(s.key, "098f6bcd4621d373cade4e832627b4f6");
        assert!(s.auto);
        let left = fs::read_to_string(&file).unwrap();
        assert!(
            !left.contains("password") && left.contains("anna"),
            "{left}"
        );
        // The rewritten file counts as read; the key stays.
        assert_eq!(a.import_sync(&file, false).unwrap(), Imported::Unchanged);
        assert_eq!(a.import_sync(&file, true).unwrap(), Imported::Loaded);
        assert_eq!(a.sync.as_ref().unwrap().key, s.key);
        // Another user without a password: no key.
        fs::write(
            &file,
            r#"{"server": "http://h:1", "username": "jan", "auto": false}"#,
        )
        .unwrap();
        a.import_sync(&file, false).unwrap();
        let s = a.sync.as_ref().unwrap();
        assert_eq!((s.key.as_str(), s.auto), ("", false));
        assert!(s.account().is_none());
        fs::write(&file, r#"{"server": "ftp://h", "username": "jan"}"#).unwrap();
        assert!(matches!(
            a.import_sync(&file, false),
            Err(ImportError::Invalid(_))
        ));
        fs::write(&file, "{").unwrap();
        assert!(matches!(
            a.import_sync(&file, false),
            Err(ImportError::Json(_))
        ));
        assert_eq!(
            a.import_sync(&dir.join("none.json"), true).unwrap(),
            Imported::Missing
        );
    }

    #[test]
    fn catalogs_merge_by_name_and_keep_stored_passwords() {
        let dir = testdir("catalogs-import");
        let file = dir.join("catalogs.json");
        fs::write(
            &file,
            r#"{"catalogs": [
                {"name": "Calibre", "url": "http://192.168.15.201:8080/opds", "username": "anna", "password": "lalka"},
                {"name": "", "url": "https://m.gutenberg.org/ebooks.opds/"}
            ]}"#,
        )
        .unwrap();
        let mut a = Accounts::default();
        a.add_catalog("http://books.lan/feed").unwrap();
        assert_eq!(a.import_catalogs(&file, false).unwrap(), Imported::Loaded);
        let names: Vec<&str> = a.catalogs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["books.lan", "Calibre", "m.gutenberg.org"]);
        assert_eq!(a.catalogs[1].credentials().unwrap().password, "lalka");
        assert!(!format!("{:?}", a.catalogs[1]).contains("lalka"));
        let left = fs::read_to_string(&file).unwrap();
        assert!(!left.contains("lalka") && left.contains("anna"));
        // Re-reading the stripped file keeps the password.
        a.import_catalogs(&file, true).unwrap();
        assert_eq!(a.catalogs[1].password.as_deref(), Some("lalka"));
        assert_eq!(a.catalogs.len(), 3);
        let entries: Vec<String> = a
            .allow_entries()
            .into_iter()
            .map(|(e, f)| format!("{e} {f}"))
            .collect();
        assert_eq!(
            entries,
            [
                "192.168.15.201:8080 opds",
                "books.lan:80 opds",
                "m.gutenberg.org:443 opds"
            ]
        );
    }

    #[test]
    fn secrets_go_only_to_a_private_folder() {
        use std::os::unix::fs::PermissionsExt;
        let base = testdir("private");
        // Root uses mode 000 to resist the stock framework's chmod g=u.
        let dir = base.join("var-local-aristotle");
        private_dir(&dir).unwrap();
        let expected_mode = if unsafe { libc::geteuid() } == 0 {
            0o000
        } else {
            0o700
        };
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            expected_mode
        );
        let a = Accounts {
            device_id: "X".into(),
            ..Accounts::default()
        };
        a.save(&dir).unwrap();
        // A folder others may write (as FAT shows itself, 0777) is refused,
        // and nothing is written into it.
        let open = base.join("userstore");
        fs::create_dir_all(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            a.save(&open).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!open.join(FILE).exists());
        // And one the framework's `chmod -R g=u` has opened to its group.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(a.save(&dir).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        // So is a symlink to a good folder.
        std::os::unix::fs::symlink(&dir, base.join("link")).unwrap();
        assert!(private_dir(&base.join("link")).is_err());
    }

    #[test]
    fn saved_owner_only_with_typed_fields() {
        let dir = testdir("accounts");
        let mut a = Accounts::default();
        a.set_sync_field(SyncField::Server, "http://192.168.15.201:17200")
            .unwrap();
        a.set_sync_field(SyncField::Username, "anna").unwrap();
        a.set_sync_field(SyncField::Password, "test").unwrap();
        assert!(a.sync.as_ref().unwrap().account().is_some());
        assert!(a.set_sync_field(SyncField::Server, "not a url").is_err());
        // A new user forgets the key.
        a.set_sync_field(SyncField::Username, "jan").unwrap();
        assert!(a.sync.as_ref().unwrap().account().is_none());
        let id = a.device_id();
        assert_eq!(id.len(), 32);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()));
        assert_eq!(a.device_id(), id);
        a.save(&dir).unwrap();
        let mode = fs::metadata(dir.join(FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(Accounts::load(&dir), a);
        assert!(a
            .allow_entries()
            .contains(&("192.168.15.201:17200".to_string(), SYNC_FEATURE)));
    }

    #[test]
    fn access_token_stays_out_of_the_file_and_missing_one_forces_refresh() {
        let settings = ReadestSettings {
            email: "reader@example.com".into(),
            refresh_token: "refresh-secret".into(),
            access_token: Some("access-secret".into()),
            expires_at: 1_900_000_000,
            last_sync_time: None,
            pending_queue: Vec::new(),
            ..blank()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("access-secret"), "access token was saved");
        assert!(json.contains("refresh-secret"));

        let loaded: ReadestSettings = serde_json::from_str(&json).unwrap();
        assert!(loaded.access_token.is_none());
        assert_eq!(
            loaded.to_session().expires_at,
            0,
            "no access token means the session must refresh"
        );

        // In memory the token is used as is.
        assert_eq!(settings.to_session().access_token, "access-secret");
        assert_eq!(settings.to_session().expires_at, 1_900_000_000);
    }

    #[test]
    fn push_queue_keeps_one_entry_per_book() {
        let mut settings = ReadestSettings {
            email: "reader@example.com".into(),
            refresh_token: "refresh".into(),
            access_token: None,
            expires_at: 0,
            last_sync_time: None,
            pending_queue: Vec::new(),
            ..blank()
        };
        settings.enqueue_push("book-1");
        settings.enqueue_push("book-1");
        settings.enqueue_push("book-2");
        assert_eq!(settings.pending_pushes(), 2);
        settings.dequeue_push("book-1");
        assert_eq!(settings.pending_pushes(), 1);
        assert_eq!(settings.pending_queue, vec!["book-2".to_string()]);
        settings.dequeue_push("book-2");
        assert_eq!(settings.pending_pushes(), 0);
    }

    #[test]
    fn automatic_push_defaults_to_the_old_behaviour_and_is_remembered() {
        // Settings written before the switch existed have no `auto_push`: they
        // must keep pushing on close, as they always did.
        let mut a: Accounts =
            serde_json::from_str(r#"{"readest":{"email":"r@example.com","refresh_token":"r"}}"#)
                .unwrap();
        let readest = a.readest.as_mut().unwrap();
        assert!(readest.auto_push_enabled());
        readest.set_auto_push(false);
        assert!(!readest.auto_push_enabled());

        // The choice survives a save and a re-login.
        let dir = testdir("auto-push");
        a.save(&dir).unwrap();
        let loaded = Accounts::load(&dir);
        assert!(!loaded.readest.as_ref().unwrap().auto_push_enabled());

        let signed_in = ReadestSettings::after_login(
            "reader@example.com",
            &inkread_core::sync::Session::new("acc", "ref", 1_900_000_000_000),
            loaded.readest.as_ref(),
        );
        assert!(!signed_in.auto_push_enabled());
        assert_eq!(signed_in.to_session().access_token, "acc");
    }

    #[test]
    fn cursors_are_per_record_type_and_survive_a_restart() {
        let mut s = blank();
        assert_eq!(s.cursor(CursorKind::Books), 0);
        s.advance_cursor(CursorKind::Books, Some(1_000));
        s.advance_cursor(CursorKind::Configs, Some(2_000));
        // A cursor never goes backwards, and a pull that saw nothing keeps it.
        s.advance_cursor(CursorKind::Books, Some(900));
        s.advance_cursor(CursorKind::Books, None);
        assert_eq!(s.cursor(CursorKind::Books), 1_000);
        assert_eq!(s.cursor(CursorKind::Configs), 2_000);
        assert_eq!(s.cursor(CursorKind::Notes), 0);

        // Settings from before the per-type cursors fall back to the last
        // sync time for every type.
        let mut old: ReadestSettings = serde_json::from_str(
            r#"{"email":"r@e.com","refresh_token":"r","last_sync_time":5000}"#,
        )
        .unwrap();
        assert_eq!(old.cursor(CursorKind::Books), 5_000);
        assert_eq!(old.cursor(CursorKind::Notes), 5_000);
        old.advance_cursor(CursorKind::Notes, Some(7_000));
        assert_eq!(old.cursor(CursorKind::Notes), 7_000);
        assert_eq!(old.cursor(CursorKind::Books), 5_000);

        let dir = testdir("cursors");
        let a = Accounts {
            readest: Some(s),
            ..Accounts::default()
        };
        a.save(&dir).unwrap();
        let loaded = Accounts::load(&dir);
        let loaded = loaded.readest.as_ref().unwrap();
        assert_eq!(loaded.cursor(CursorKind::Books), 1_000);
        assert_eq!(loaded.cursor(CursorKind::Configs), 2_000);
        assert_eq!(loaded.cursor(CursorKind::Notes), 0);
    }

    #[test]
    fn a_saved_former_official_auth_host_is_treated_as_unset() {
        let dir = testdir("legacy-host");
        fs::write(
            dir.join(FILE),
            r#"{"readest_endpoints":{"supabase_url":"https://vbsyxfusjjqdxkjqlysc.supabase.co"}}"#,
        )
        .unwrap();
        let a = Accounts::load(&dir);
        assert!(a.readest_endpoints.supabase_url.is_none());
        assert_eq!(
            a.readest_config().supabase_url,
            inkread_core::sync::SyncConfig::default().supabase_url
        );

        // A self-hosted address the user really typed is left alone.
        fs::write(
            dir.join(FILE),
            r#"{"readest_endpoints":{"supabase_url":"https://auth.example.com"}}"#,
        )
        .unwrap();
        assert_eq!(
            Accounts::load(&dir).readest_config().supabase_url.as_str(),
            "https://auth.example.com"
        );
    }

    #[test]
    fn readest_endpoints_are_allowed_before_the_first_login() {
        let a = Accounts::default();
        let entries: Vec<String> = a
            .readest_allow_entries()
            .into_iter()
            .map(|(e, f)| format!("{e} {f}"))
            .collect();
        assert_eq!(
            entries,
            [
                "auth-cf.readest.com:443 readest",
                "web.readest.com:443 readest"
            ]
        );

        // A public plain-HTTP endpoint is refused: tokens would go in clear.
        let a = Accounts {
            readest_endpoints: ReadestEndpoints {
                api_base: Some("http://books.example.com/api".into()),
                supabase_url: Some("https://auth.example.com".into()),
                supabase_anon_key: None,
            },
            ..Accounts::default()
        };
        let entries: Vec<String> = a
            .readest_allow_entries()
            .into_iter()
            .map(|(e, f)| format!("{e} {f}"))
            .collect();
        assert_eq!(entries, ["auth.example.com:443 readest"]);

        // A mock server on a private address is allowed for development.
        let a = Accounts {
            readest_endpoints: ReadestEndpoints {
                api_base: Some("http://192.168.1.5:8765/api".into()),
                supabase_url: Some("http://127.0.0.1:8765".into()),
                supabase_anon_key: None,
            },
            ..Accounts::default()
        };
        let entries: Vec<String> = a
            .readest_allow_entries()
            .into_iter()
            .map(|(e, f)| format!("{e} {f}"))
            .collect();
        assert_eq!(
            entries,
            ["127.0.0.1:8765 readest", "192.168.1.5:8765 readest"]
        );
        // The entries are also part of the whole allowlist without an account.
        assert!(a
            .allow_entries()
            .contains(&("192.168.1.5:8765".to_string(), READEST_FEATURE)));
    }

    #[test]
    fn endpoints_survive_logout_and_are_editable_before_login() {
        let dir = testdir("readest-endpoints");
        // Before the first sign-in there is no account, but the endpoints are
        // still saved.
        let mut a = Accounts {
            readest_endpoints: ReadestEndpoints {
                api_base: Some("http://10.0.0.2:8765/api".into()),
                supabase_url: Some("http://10.0.0.2:8765".into()),
                supabase_anon_key: Some("anon".into()),
            },
            ..Accounts::default()
        };
        a.save(&dir).unwrap();
        assert_eq!(Accounts::load(&dir).readest_endpoints, a.readest_endpoints);

        // Signing in keeps them; signing out does not remove them.
        a.readest = Some(ReadestSettings {
            email: "reader@example.com".into(),
            refresh_token: "refresh".into(),
            access_token: None,
            expires_at: 0,
            last_sync_time: None,
            pending_queue: Vec::new(),
            ..blank()
        });
        a.save(&dir).unwrap();
        assert_eq!(Accounts::load(&dir).readest_endpoints, a.readest_endpoints);
        a.readest = None;
        a.save(&dir).unwrap();
        assert_eq!(Accounts::load(&dir).readest_endpoints, a.readest_endpoints);
        assert!(Accounts::load(&dir).readest.is_none());
    }

    #[test]
    fn old_archives_move_endpoints_out_of_the_account() {
        let dir = testdir("readest-migrate");
        fs::write(
            dir.join(FILE),
            r#"{
                "readest": {
                    "email": "reader@example.com",
                    "refresh_token": "refresh",
                    "api_base": "https://sync.example.com/api",
                    "supabase_url": "https://auth.example.com",
                    "supabase_anon_key": "anon-key"
                }
            }"#,
        )
        .unwrap();
        let a = Accounts::load(&dir);
        assert_eq!(
            a.readest_endpoints,
            ReadestEndpoints {
                api_base: Some("https://sync.example.com/api".into()),
                supabase_url: Some("https://auth.example.com".into()),
                supabase_anon_key: Some("anon-key".into()),
            }
        );
        // Saving writes them once, outside the account.
        a.save(&dir).unwrap();
        let saved = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(saved.contains("readest_endpoints"));
        let readest = saved.split("\"readest_endpoints\"").next().unwrap();
        assert!(!readest.contains("api_base"), "{saved}");
        // Existing data wins over the old place.
        fs::write(
            dir.join(FILE),
            r#"{
                "readest": {"email": "old", "refresh_token": "r", "api_base": "https://old.example.com"},
                "readest_endpoints": {"api_base": "https://new.example.com"}
            }"#,
        )
        .unwrap();
        let a = Accounts::load(&dir);
        assert_eq!(
            a.readest_endpoints.api_base.as_deref(),
            Some("https://new.example.com")
        );
    }

    #[test]
    fn readest_config_falls_back_to_the_defaults() {
        let def = inkread_core::sync::SyncConfig::default();
        assert_eq!(def.supabase_url, "https://auth-cf.readest.com");
        let a = Accounts::default();
        assert_eq!(a.readest_config().api_base, def.api_base);
        assert_eq!(a.readest_config().supabase_url, def.supabase_url);
        assert_eq!(a.readest_config().supabase_anon_key, def.supabase_anon_key);
        assert!(a
            .readest_allow_entries()
            .contains(&("auth-cf.readest.com:443".to_string(), READEST_FEATURE)));
        let old: Accounts = serde_json::from_str("{}").unwrap();
        assert_eq!(old.readest_config().supabase_url, def.supabase_url);

        // Only the set parts change; the rest keeps the defaults.
        let a = Accounts {
            readest_endpoints: ReadestEndpoints {
                api_base: Some("http://192.168.1.5:8765/api".into()),
                supabase_url: None,
                supabase_anon_key: None,
            },
            ..Accounts::default()
        };
        let config = a.readest_config();
        assert_eq!(config.api_base, "http://192.168.1.5:8765/api");
        assert_eq!(config.supabase_url, def.supabase_url);
        assert_eq!(config.supabase_anon_key, def.supabase_anon_key);
    }

    #[test]
    fn typed_endpoints_work_before_login_and_survive_sign_out() {
        let mut a = Accounts::default();
        a.set_readest_endpoint(
            ReadestEndpointField::ApiBase,
            "  https://sync.example.com/api  ",
        )
        .unwrap();
        a.set_readest_endpoint(ReadestEndpointField::SupabaseUrl, "http://10.0.0.2:8765")
            .unwrap();
        a.set_readest_endpoint(ReadestEndpointField::AnonKey, "anon-key")
            .unwrap();
        assert_eq!(a.readest_config().api_base, "https://sync.example.com/api");
        assert_eq!(a.readest_config().supabase_url, "http://10.0.0.2:8765");
        assert_eq!(a.readest_config().supabase_anon_key, "anon-key");

        // A public plain-HTTP address and a non-address are refused, and the
        // stored value stays.
        assert!(a
            .set_readest_endpoint(ReadestEndpointField::ApiBase, "http://example.com/api")
            .is_err());
        assert!(a
            .set_readest_endpoint(ReadestEndpointField::SupabaseUrl, "not a url")
            .is_err());
        assert_eq!(a.readest_config().api_base, "https://sync.example.com/api");
        assert_eq!(a.readest_config().supabase_url, "http://10.0.0.2:8765");

        // Signing in and out keeps the endpoints.
        a.readest = Some(ReadestSettings {
            email: "reader@example.com".into(),
            refresh_token: "refresh".into(),
            access_token: None,
            expires_at: 0,
            last_sync_time: None,
            pending_queue: Vec::new(),
            ..blank()
        });
        assert_eq!(a.readest_config().api_base, "https://sync.example.com/api");
        a.readest = None;
        assert_eq!(a.readest_config().api_base, "https://sync.example.com/api");

        // Restoring defaults clears all three and gives the official config.
        a.reset_readest_endpoints();
        assert!(a.readest_endpoints.is_empty());
        let def = inkread_core::sync::SyncConfig::default();
        assert_eq!(a.readest_config().api_base, def.api_base);
        assert_eq!(
            a.readest_config().supabase_url,
            "https://auth-cf.readest.com"
        );
        assert_eq!(a.readest_config().supabase_anon_key, def.supabase_anon_key);

        // Typing the official value back does not count as a custom endpoint.
        a.set_readest_endpoint(ReadestEndpointField::ApiBase, &def.api_base)
            .unwrap();
        a.set_readest_endpoint(ReadestEndpointField::SupabaseUrl, &def.supabase_url)
            .unwrap();
        a.set_readest_endpoint(ReadestEndpointField::AnonKey, &def.supabase_anon_key)
            .unwrap();
        assert!(a.readest_endpoints.is_empty());
        assert_eq!(a.readest_config().api_base, def.api_base);
        assert_eq!(a.readest_config().supabase_url, def.supabase_url);
        assert_eq!(a.readest_config().supabase_anon_key, def.supabase_anon_key);
    }

    #[test]
    fn jianguoyun_json_is_read_once_and_loses_its_password() {
        let dir = testdir("jianguoyun-import");
        let file = dir.join("jianguoyun.json");
        fs::write(
            &file,
            r#"{"username": "reader@example.com", "password": "app-password", "remote_dir": "书库", "auto_push": false}"#,
        )
        .unwrap();
        let mut a = Accounts::default();
        assert_eq!(a.import_jianguoyun(&file, false).unwrap(), Imported::Loaded);
        let s = a.jianguoyun.clone().unwrap();
        assert_eq!(s.server_url, "https://dav.jianguoyun.com/dav");
        assert_eq!(s.username, "reader@example.com");
        assert_eq!(s.password, "app-password");
        assert_eq!(s.remote_dir, "书库");
        assert!(!s.auto_push);
        assert_eq!(
            s.to_core_config().normalized_base_url().unwrap(),
            "https://dav.jianguoyun.com/dav/%E4%B9%A6%E5%BA%93/"
        );

        // The password is gone from the file, and the rewritten file counts
        // as read; the endpoint survives even though the file omitted it.
        let left = fs::read_to_string(&file).unwrap();
        assert!(!left.contains("password"), "{left}");
        assert!(left.contains("dav.jianguoyun.com"), "{left}");
        assert_eq!(
            a.import_jianguoyun(&file, false).unwrap(),
            Imported::Unchanged
        );
        assert_eq!(a.import_jianguoyun(&file, true).unwrap(), Imported::Loaded);
        assert_eq!(a.jianguoyun.as_ref().unwrap().password, "app-password");
        assert_eq!(a.jianguoyun.as_ref().unwrap().remote_dir, "书库");
        assert!(!a.jianguoyun.as_ref().unwrap().auto_push);

        // A settings screen may save the official address with its trailing
        // slash. Re-reading the wiped file must keep the stored password even
        // though the two spellings differ.
        a.jianguoyun.as_mut().unwrap().server_url = "https://dav.jianguoyun.com/dav/".to_string();
        assert_eq!(a.import_jianguoyun(&file, true).unwrap(), Imported::Loaded);
        assert_eq!(a.jianguoyun.as_ref().unwrap().password, "app-password");

        // A missing file is not an error; a bad endpoint is, and the old
        // settings stay.
        assert_eq!(
            a.import_jianguoyun(&dir.join("none.json"), true).unwrap(),
            Imported::Missing
        );
        fs::write(
            &file,
            r#"{"server_url": "http://example.com/dav/", "username": "reader@example.com"}"#,
        )
        .unwrap();
        assert!(matches!(
            a.import_jianguoyun(&file, false),
            Err(ImportError::Invalid(_))
        ));
        assert_eq!(a.jianguoyun.as_ref().unwrap().password, "app-password");
        fs::write(
            &file,
            r#"{"username": "reader@example.com", "password": "x", "remote_dir": "../etc"}"#,
        )
        .unwrap();
        assert!(matches!(
            a.import_jianguoyun(&file, false),
            Err(ImportError::Invalid(_))
        ));
        fs::write(
            &file,
            r#"{"server_url": "http://127.0.0.1:8080/dav", "username": "reader@example.com", "password": "app-password"}"#,
        )
        .unwrap();
        assert_eq!(a.import_jianguoyun(&file, false).unwrap(), Imported::Loaded);
        assert_eq!(
            a.jianguoyun.as_ref().unwrap().server_url,
            "http://127.0.0.1:8080/dav"
        );
    }

    #[test]
    fn jianguoyun_settings_redact_and_queue() {
        let mut s = JianguoyunSettings::official("reader@example.com", "app-password");
        assert!(s.auto_push);
        let debug = format!("{s:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("app-password"));
        assert!(debug.contains("reader@example.com"));
        assert!(s.pending_queue.is_empty());
        s.enqueue_push("book-a");
        s.enqueue_push("book-a");
        s.enqueue_push("book-b");
        assert_eq!(s.pending_pushes(), 2);
        s.dequeue_push("book-a");
        assert_eq!(s.pending_queue, ["book-b"]);

        let parsed: JianguoyunSettings =
            serde_json::from_str(r#"{"server_url": "", "username": "u", "password": "p"}"#)
                .unwrap();
        assert!(parsed.auto_push);
        assert!(parsed.pending_queue.is_empty());
        assert!(parsed.last_sync_time.is_none());
        assert_eq!(
            parsed.to_core_config().normalized_base_url().unwrap(),
            "https://dav.jianguoyun.com/dav/"
        );
    }

    #[test]
    fn jianguoyun_accounts_persist_private_and_stay_backward_compatible() {
        // An accounts.json written before Jianguoyun existed still loads.
        let dir = testdir("jianguoyun-legacy");
        fs::write(dir.join(FILE), r#"{"device_id": "ABCDEF", "catalogs": []}"#).unwrap();
        let mut a = Accounts::load(&dir);
        assert!(a.jianguoyun.is_none());
        assert!(!format!("{:?}", a).contains("dav.jianguoyun"));

        a.jianguoyun = Some(JianguoyunSettings::official(
            "reader@example.com",
            "app-password",
        ));
        a.jianguoyun.as_mut().unwrap().enqueue_push("book-a");
        a.save(&dir).unwrap();
        let mode = fs::metadata(dir.join(FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let loaded = Accounts::load(&dir);
        assert_eq!(
            loaded.jianguoyun.as_ref().unwrap().username,
            "reader@example.com"
        );
        assert_eq!(
            loaded.jianguoyun.as_ref().unwrap().pending_queue,
            ["book-a"]
        );
        // The raw file holds the password the settings need, and the Debug
        // output never does.
        let raw = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(raw.contains("app-password"));
        assert!(raw.contains("\"jianguoyun\""));
        assert!(!format!("{:?}", loaded.jianguoyun).contains("app-password"));

        // A custom loopback endpoint is registered; a public plain-HTTP one
        // never is, because the app password would be readable on the wire.
        assert_eq!(
            loaded
                .jianguoyun_allow_entries()
                .into_iter()
                .collect::<Vec<_>>(),
            [("dav.jianguoyun.com:443".to_string(), JIANGUOYUN_FEATURE)]
        );
        let mut scoped = loaded;
        scoped.jianguoyun.as_mut().unwrap().server_url = "http://127.0.0.1:9/dav".into();
        assert!(scoped
            .allow_entries()
            .contains(&("127.0.0.1:9".to_string(), JIANGUOYUN_FEATURE)));
        assert!(!scoped
            .allow_entries()
            .contains(&("dav.jianguoyun.com:443".to_string(), JIANGUOYUN_FEATURE)));
        scoped.jianguoyun.as_mut().unwrap().server_url = "http://example.com/dav".into();
        assert!(scoped.jianguoyun_allow_entries().is_empty());
        assert!(!scoped
            .allow_entries()
            .iter()
            .any(|(_, feature)| *feature == JIANGUOYUN_FEATURE));
    }
}
