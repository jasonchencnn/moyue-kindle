//! Wire record models aligned strictly with Readest server definitions (`records.ts`)
//! and sync-contract v2.

use serde::{Deserialize, Serialize};

use crate::model::{BookConfig, BookNote, NoteType};
use crate::sync::time::{format_iso8601, parse_iso8601};

/// Deserialize an optional JSON field that may arrive either as a pre-stringified
/// JSON string (from Supabase DB rows in `GET /api/sync`) or as a structured JSON
/// value/object (from client `POST /api/sync` payloads before `transformsToDB`).
fn deserialize_string_or_json_opt<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(other) => serde_json::to_string(&other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Deserialize `DbBookConfig.progress` which may be a JSON string `"[10, 150]"`
/// (DB row format) or a 2-element JSON array `[10, 150]` (client POST format).
fn deserialize_config_progress_opt<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Array(arr)) if arr.len() == 2 => {
            let cur = arr[0]
                .as_u64()
                .ok_or_else(|| serde::de::Error::custom("progress[0] must be u32"))?;
            let total = arr[1]
                .as_u64()
                .ok_or_else(|| serde::de::Error::custom("progress[1] must be u32"))?;
            Ok(Some(format!("[{cur}, {total}]")))
        }
        Some(other) => Err(serde::de::Error::custom(format!(
            "unexpected config.progress format: {other}"
        ))),
    }
}

/// DBBook wire representation (`records.ts:1-25`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DbBook {
    #[serde(skip_serializing_if = "Option::is_none", alias = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "hash", alias = "bookHash")]
    pub book_hash: String,
    #[serde(skip_serializing_if = "Option::is_none", alias = "metaHash")]
    pub meta_hash: Option<String>,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none", alias = "sourceTitle")]
    pub source_title: Option<String>,
    #[serde(default)]
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none", alias = "groupId")]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "groupName")]
    pub group_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "groupUpdatedAt")]
    pub group_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "readingStatus")]
    pub reading_status: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "readingStatusUpdatedAt"
    )]
    pub reading_status_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "coverHash")]
    pub cover_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "coverUpdatedAt")]
    pub cover_updated_at: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_string_or_json_opt"
    )]
    pub metadata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "metadataUpdatedAt")]
    pub metadata_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "createdAt")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "updatedAt")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "deletedAt")]
    pub deleted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "uploadedAt")]
    pub uploaded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "syncedAt")]
    pub synced_at: Option<String>,
}

/// DBBookConfig wire representation (`records.ts:27-41`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DbBookConfig {
    #[serde(skip_serializing_if = "Option::is_none", alias = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "bookHash")]
    pub book_hash: String,
    #[serde(skip_serializing_if = "Option::is_none", alias = "metaHash")]
    pub meta_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpointer: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_config_progress_opt"
    )]
    pub progress: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "rsvpPosition",
        deserialize_with = "deserialize_string_or_json_opt"
    )]
    pub rsvp_position: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "searchConfig",
        deserialize_with = "deserialize_string_or_json_opt"
    )]
    pub search_config: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "viewSettings",
        deserialize_with = "deserialize_string_or_json_opt"
    )]
    pub view_settings: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "createdAt")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "updatedAt")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "deletedAt")]
    pub deleted_at: Option<String>,
}

/// Format a `(current, total)` page pair the way contract §3.2 stores it.
///
/// Returns `None` when either side is unknown: a fraction must never be sent
/// in its place, because other clients parse this as a page pair.
pub fn format_progress_pair(pair: Option<(u32, u32)>) -> Option<String> {
    pair.filter(|(cur, total)| *cur > 0 && *total > 0)
        .map(|(cur, total)| format!("[{cur}, {total}]"))
}

impl DbBookConfig {
    /// Convert wire record into local `BookConfig` model.
    pub fn to_model(&self) -> BookConfig {
        let parsed_progress = self.progress.as_deref().and_then(|p| {
            let trimmed = p.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                let inner = &trimmed[1..trimmed.len() - 1];
                let parts: Vec<&str> = inner.split(',').map(|s| s.trim()).collect();
                if parts.len() == 2 {
                    if let (Ok(cur), Ok(total)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                    {
                        return Some((cur, total));
                    }
                }
            }
            None
        });

        let updated_at_ms = self
            .updated_at
            .as_deref()
            .and_then(|ts| parse_iso8601(ts).ok())
            .unwrap_or(0);

        BookConfig {
            schema_version: Some(1),
            book_hash: Some(self.book_hash.clone()),
            progress: parsed_progress,
            location: self.location.clone(),
            xpointer: self.xpointer.clone(),
            booknotes: Vec::new(),
            updated_at: updated_at_ms,
        }
    }

    /// Convert local `BookConfig` model into wire record.
    pub fn from_model(book_hash: &str, model: &BookConfig) -> Self {
        let progress_str = model
            .progress
            .map(|(cur, total)| format!("[{cur}, {total}]"));
        let updated_at_str = if model.updated_at > 0 {
            Some(format_iso8601(model.updated_at))
        } else {
            None
        };

        Self {
            user_id: None,
            book_hash: book_hash.to_string(),
            meta_hash: None,
            location: model.location.clone(),
            xpointer: model.xpointer.clone(),
            progress: progress_str,
            rsvp_position: None,
            search_config: None,
            view_settings: None,
            created_at: None,
            updated_at: updated_at_str,
            deleted_at: None,
        }
    }
}

/// DBBookNote wire representation (`records.ts:43-62`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DbBookNote {
    #[serde(skip_serializing_if = "Option::is_none", alias = "userId")]
    pub user_id: Option<String>,
    #[serde(alias = "bookHash")]
    pub book_hash: String,
    #[serde(skip_serializing_if = "Option::is_none", alias = "metaHash")]
    pub meta_hash: Option<String>,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpointer0: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpointer1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "createdAt")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "updatedAt")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "deletedAt")]
    pub deleted_at: Option<String>,
}

impl DbBookNote {
    /// Convert wire note into local `BookNote` model.
    pub fn to_model(&self) -> BookNote {
        let kind = match self.kind.as_str() {
            "bookmark" => NoteType::Bookmark,
            "annotation" => NoteType::Annotation,
            "excerpt" => NoteType::Excerpt,
            _ => NoteType::Other,
        };

        let created_at_ms = self
            .created_at
            .as_deref()
            .and_then(|ts| parse_iso8601(ts).ok())
            .unwrap_or(0);
        let updated_at_ms = self
            .updated_at
            .as_deref()
            .and_then(|ts| parse_iso8601(ts).ok())
            .unwrap_or(0);
        let deleted_at_ms = self
            .deleted_at
            .as_deref()
            .and_then(|ts| parse_iso8601(ts).ok());

        BookNote {
            id: self.id.clone(),
            kind,
            cfi: self.cfi.clone().unwrap_or_default(),
            xpointer0: self.xpointer0.clone(),
            xpointer1: self.xpointer1.clone(),
            text: self.text.clone(),
            color: self.color.clone(),
            style: self.style.clone(),
            note: self.note.clone().unwrap_or_default(),
            created_at: created_at_ms,
            updated_at: updated_at_ms,
            deleted_at: deleted_at_ms,
        }
    }

    /// Convert local `BookNote` model into wire note.
    pub fn from_model(book_hash: &str, model: &BookNote) -> Self {
        let kind_str = match model.kind {
            NoteType::Bookmark => "bookmark",
            NoteType::Annotation => "annotation",
            NoteType::Excerpt => "excerpt",
            NoteType::Other => "other",
        };

        let cfi_opt = if model.cfi.is_empty() {
            None
        } else {
            Some(model.cfi.clone())
        };

        let created_at_str = if model.created_at > 0 {
            Some(format_iso8601(model.created_at))
        } else {
            None
        };
        let updated_at_str = if model.updated_at > 0 {
            Some(format_iso8601(model.updated_at))
        } else {
            None
        };
        let deleted_at_str = model.deleted_at.map(format_iso8601);

        Self {
            user_id: None,
            book_hash: book_hash.to_string(),
            meta_hash: None,
            id: model.id.clone(),
            kind: kind_str.to_string(),
            cfi: cfi_opt,
            xpointer0: model.xpointer0.clone(),
            xpointer1: model.xpointer1.clone(),
            page: None,
            text: model.text.clone(),
            style: model.style.clone(),
            color: model.color.clone(),
            note: if model.note.is_empty() {
                None
            } else {
                Some(model.note.clone())
            },
            global: None,
            created_at: created_at_str,
            updated_at: updated_at_str,
            deleted_at: deleted_at_str,
        }
    }
}

/// Reading statistics book record (`sync-contract.md §7`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StatBookRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub book_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_hash: Option<String>,
    #[serde(default)]
    pub read_count: u32,
    #[serde(default)]
    pub total_reading_time: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_read_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Reading statistics page-stay record (`sync-contract.md §7`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StatPageRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub book_hash: String,
    pub page: u32,
    pub start_time: i64,
    pub duration: u32,
    pub total_pages: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Readest dummy book hash injected by upstream `/api/sync` on empty initial pulls (contract §1.4).
pub const DUMMY_BOOK_HASH: &str = "00000000000000000000000000000000";

/// Deserialize `books` tolerating upstream wire quirks such as the empty-account
/// dummy book row (`sync.ts:416-434`), which carries integer epoch-ms timestamps
/// alongside duplicate snake_case + camelCase keys (`book_hash`/`hash`,
/// `updated_at`/`updatedAt`, `deleted_at`/`deletedAt`, `created_at`/`createdAt`).
fn deserialize_books_lenient<'de, D>(deserializer: D) -> Result<Vec<DbBook>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    const DUPLICATE_PAIRS: &[(&str, &str)] = &[
        ("book_hash", "hash"),
        ("meta_hash", "metaHash"),
        ("source_title", "sourceTitle"),
        ("group_id", "groupId"),
        ("group_name", "groupName"),
        ("group_updated_at", "groupUpdatedAt"),
        ("reading_status", "readingStatus"),
        ("reading_status_updated_at", "readingStatusUpdatedAt"),
        ("cover_hash", "coverHash"),
        ("cover_updated_at", "coverUpdatedAt"),
        ("metadata_updated_at", "metadataUpdatedAt"),
        ("created_at", "createdAt"),
        ("updated_at", "updatedAt"),
        ("deleted_at", "deletedAt"),
        ("uploaded_at", "uploadedAt"),
        ("synced_at", "syncedAt"),
    ];
    const TIMESTAMP_KEYS: &[&str] = &[
        "created_at",
        "createdAt",
        "updated_at",
        "updatedAt",
        "deleted_at",
        "deletedAt",
        "uploaded_at",
        "uploadedAt",
        "synced_at",
        "syncedAt",
        "group_updated_at",
        "groupUpdatedAt",
        "reading_status_updated_at",
        "readingStatusUpdatedAt",
        "cover_updated_at",
        "coverUpdatedAt",
        "metadata_updated_at",
        "metadataUpdatedAt",
    ];

    let raw_books = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut books = Vec::with_capacity(raw_books.len());
    for mut value in raw_books {
        if let Some(obj) = value.as_object_mut() {
            for &(snake, camel) in DUPLICATE_PAIRS {
                if obj.contains_key(snake) && obj.contains_key(camel) {
                    obj.remove(camel);
                }
            }
            for &key in TIMESTAMP_KEYS {
                if let Some(ms) = obj.get(key).and_then(|v| v.as_i64()) {
                    obj.insert(
                        key.to_string(),
                        serde_json::Value::String(format_iso8601(ms)),
                    );
                }
            }
        }
        let book: DbBook = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        books.push(book);
    }
    Ok(books)
}

/// Pull response payload from `GET /api/sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncPullResponse {
    #[serde(default, deserialize_with = "deserialize_books_lenient")]
    pub books: Vec<DbBook>,
    #[serde(default)]
    pub configs: Vec<DbBookConfig>,
    #[serde(default)]
    pub notes: Vec<DbBookNote>,
    #[serde(default)]
    pub stat_books: Vec<StatBookRecord>,
    #[serde(default)]
    pub stat_pages: Vec<StatPageRecord>,
}

/// The later of two optional millisecond timestamps.
fn latest(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

/// The millisecond value of an ISO-8601 field, if it parses.
fn ms(field: Option<&str>) -> Option<i64> {
    field.and_then(|ts| parse_iso8601(ts).ok())
}

impl SyncPullResponse {
    /// The newest server timestamp among the pulled `books` rows.
    ///
    /// Contract §1.2: the `books` cursor is the server's `synced_at` (falling
    /// back to `updated_at` when a mock or an older row has none). A device
    /// must never advance this cursor with its own clock: the Kindle's clock
    /// drifts, and the contract uses the server clock precisely to avoid
    /// skipping another device's changes.
    pub fn books_cursor(&self) -> Option<i64> {
        self.books.iter().fold(None, |acc, b| {
            latest(
                acc,
                ms(b.synced_at.as_deref()).or_else(|| ms(b.updated_at.as_deref())),
            )
        })
    }

    /// The newest server timestamp among the pulled `configs` rows
    /// (`updated_at` or `deleted_at`, contract §1.2).
    pub fn configs_cursor(&self) -> Option<i64> {
        self.configs.iter().fold(None, |acc, c| {
            latest(
                acc,
                latest(ms(c.updated_at.as_deref()), ms(c.deleted_at.as_deref())),
            )
        })
    }

    /// The newest server timestamp among the pulled `notes` rows
    /// (`updated_at` or `deleted_at`, contract §1.2).
    pub fn notes_cursor(&self) -> Option<i64> {
        self.notes.iter().fold(None, |acc, n| {
            latest(
                acc,
                latest(ms(n.updated_at.as_deref()), ms(n.deleted_at.as_deref())),
            )
        })
    }
}

/// Push request payload for `POST /api/sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncPushRequest {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub books: Vec<DbBook>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub configs: Vec<DbBookConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notes: Vec<DbBookNote>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stat_books: Vec<StatBookRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stat_pages: Vec<StatPageRecord>,
}

/// Client-to-server push book wire representation matching Readest's
/// `transformBookToDB` (`src/utils/transform.ts:68-119`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PushWireBook<'a> {
    hash: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta_hash: &'a Option<String>,
    format: &'a str,
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_title: &'a Option<String>,
    author: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_name: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_updated_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: &'a Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reading_status: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reading_status_updated_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_hash: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_updated_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata_updated_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uploaded_at: &'a Option<String>,
}

/// Client-to-server push config wire representation matching Readest's
/// `transformBookConfigToDB` (`src/utils/transform.ts:16-41`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PushWireConfig<'a> {
    book_hash: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta_hash: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xpointer: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rsvp_position: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    view_settings: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at: &'a Option<String>,
}

/// Client-to-server push note wire representation matching Readest's
/// `transformBookNoteToDB` (`src/utils/transform.ts:185-225`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PushWireNote<'a> {
    book_hash: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta_hash: &'a Option<String>,
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cfi: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xpointer0: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xpointer1: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    style: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    global: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at: &'a Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PushWireEnvelope<'a> {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    books: Vec<PushWireBook<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    configs: Vec<PushWireConfig<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<PushWireNote<'a>>,
    #[serde(skip_serializing_if = "Slice::is_empty")]
    stat_books: &'a [StatBookRecord],
    #[serde(skip_serializing_if = "Slice::is_empty")]
    stat_pages: &'a [StatPageRecord],
}

type Slice<T> = [T];

/// Parse a JSON-encoded string field into a structured [`serde_json::Value`]
/// so Readest's `transformBookToDB` / `transformBookConfigToDB` does not
/// `JSON.stringify(...)` an already-serialized string a second time. Falls
/// back to `Value::String` when the stored value is not valid JSON.
fn parse_json_or_string_opt(field: &Option<String>) -> Option<serde_json::Value> {
    field.as_ref().map(|s| {
        serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::String(s.clone()))
    })
}

impl SyncPushRequest {
    /// Serialize the push request into the exact camelCase JSON expected by
    /// Readest's `POST /api/sync` (`transformsToDB` in `src/utils/transform.ts`).
    pub fn to_wire_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let books = self
            .books
            .iter()
            .map(|b| PushWireBook {
                hash: &b.book_hash,
                meta_hash: &b.meta_hash,
                format: &b.format,
                title: &b.title,
                source_title: &b.source_title,
                author: &b.author,
                group_id: &b.group_id,
                group_name: &b.group_name,
                group_updated_at: &b.group_updated_at,
                tags: &b.tags,
                progress: b.progress,
                reading_status: &b.reading_status,
                reading_status_updated_at: &b.reading_status_updated_at,
                cover_hash: &b.cover_hash,
                cover_updated_at: &b.cover_updated_at,
                metadata: parse_json_or_string_opt(&b.metadata),
                metadata_updated_at: &b.metadata_updated_at,
                created_at: &b.created_at,
                updated_at: &b.updated_at,
                deleted_at: &b.deleted_at,
                uploaded_at: &b.uploaded_at,
            })
            .collect();

        let configs = self
            .configs
            .iter()
            .map(|c| PushWireConfig {
                book_hash: &c.book_hash,
                meta_hash: &c.meta_hash,
                location: &c.location,
                xpointer: &c.xpointer,
                progress: c.to_model().progress,
                rsvp_position: parse_json_or_string_opt(&c.rsvp_position),
                search_config: parse_json_or_string_opt(&c.search_config),
                view_settings: parse_json_or_string_opt(&c.view_settings),
                created_at: &c.created_at,
                updated_at: &c.updated_at,
                deleted_at: &c.deleted_at,
            })
            .collect();

        let notes = self
            .notes
            .iter()
            .map(|n| PushWireNote {
                book_hash: &n.book_hash,
                meta_hash: &n.meta_hash,
                id: &n.id,
                kind: &n.kind,
                cfi: &n.cfi,
                xpointer0: &n.xpointer0,
                xpointer1: &n.xpointer1,
                page: n.page,
                text: &n.text,
                style: &n.style,
                color: &n.color,
                note: &n.note,
                global: n.global,
                created_at: &n.created_at,
                updated_at: &n.updated_at,
                deleted_at: &n.deleted_at,
            })
            .collect();

        let envelope = PushWireEnvelope {
            books,
            configs,
            notes,
            stat_books: &self.stat_books,
            stat_pages: &self.stat_pages,
        };
        serde_json::to_vec(&envelope)
    }
}

/// Push response payload from `POST /api/sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncPushResponse {
    #[serde(default)]
    pub books: Vec<DbBook>,
    #[serde(default)]
    pub configs: Vec<DbBookConfig>,
    #[serde(default)]
    pub notes: Vec<DbBookNote>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_conversion() {
        let model = BookConfig {
            schema_version: Some(1),
            book_hash: Some("abc123hash".to_string()),
            progress: Some((42, 100)),
            location: Some("epubcfi(/6/2)".to_string()),
            xpointer: Some("/body/DocFragment[1]/p[1].0".to_string()),
            booknotes: Vec::new(),
            updated_at: 1762344000000,
        };

        let wire = DbBookConfig::from_model("abc123hash", &model);
        assert_eq!(wire.progress.as_deref(), Some("[42, 100]"));
        assert_eq!(wire.xpointer.as_deref(), model.xpointer.as_deref());

        let converted_back = wire.to_model();
        assert_eq!(converted_back.progress, model.progress);
        assert_eq!(converted_back.xpointer, model.xpointer);
        assert_eq!(converted_back.updated_at, model.updated_at);
    }

    #[test]
    fn progress_pair_is_written_only_when_both_pages_are_known() {
        assert_eq!(
            format_progress_pair(Some((10, 150))).as_deref(),
            Some("[10, 150]")
        );
        assert_eq!(format_progress_pair(None), None);
        assert_eq!(format_progress_pair(Some((0, 150))), None);
        assert_eq!(format_progress_pair(Some((10, 0))), None);
    }

    #[test]
    fn note_roundtrip_conversion() {
        let note = BookNote {
            id: "note-01".to_string(),
            kind: NoteType::Annotation,
            cfi: "epubcfi(/6/4)".to_string(),
            xpointer0: Some("xp0".to_string()),
            xpointer1: Some("xp1".to_string()),
            text: Some("sample text".to_string()),
            color: Some("yellow".to_string()),
            style: Some("highlight".to_string()),
            note: "sample note".to_string(),
            created_at: 1762344000000,
            updated_at: 1762344100000,
            deleted_at: Some(1762344200000),
        };

        let wire = DbBookNote::from_model("book-01", &note);
        assert_eq!(wire.kind, "annotation");
        assert_eq!(wire.id, "note-01");
        assert_eq!(wire.color.as_deref(), Some("yellow"));
        assert_eq!(wire.style.as_deref(), Some("highlight"));

        let converted_back = wire.to_model();
        assert_eq!(converted_back.id, note.id);
        assert_eq!(converted_back.kind, note.kind);
        assert_eq!(converted_back.text, note.text);
        assert_eq!(converted_back.color, note.color);
        assert_eq!(converted_back.style, note.style);
        assert_eq!(converted_back.note, note.note);
        assert_eq!(converted_back.created_at, note.created_at);
        assert_eq!(converted_back.updated_at, note.updated_at);
        assert_eq!(converted_back.deleted_at, note.deleted_at);
    }

    #[test]
    fn serde_json_array_serialization() {
        let book = DbBook {
            book_hash: "hash01".to_string(),
            title: "Test Book".to_string(),
            progress: Some((15, 300)),
            ..Default::default()
        };
        let json = serde_json::to_string(&book).unwrap();
        assert!(json.contains("\"progress\":[15,300]"));

        let deserialized: DbBook = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.progress, Some((15, 300)));
    }

    #[test]
    fn push_wire_bytes_emits_json_objects_without_double_stringify() {
        let req = SyncPushRequest {
            books: vec![DbBook {
                book_hash: "hash01".to_string(),
                format: "EPUB".to_string(),
                title: "Test Book".to_string(),
                author: "Author".to_string(),
                metadata: Some(r#"{"publisher":"InkPress","lang":"zh"}"#.to_string()),
                ..Default::default()
            }],
            configs: vec![DbBookConfig {
                book_hash: "hash01".to_string(),
                rsvp_position: Some(r#"{"wordIndex":12}"#.to_string()),
                search_config: Some(r#"{"matchCase":true}"#.to_string()),
                view_settings: Some(r#"{"fontSize":18}"#.to_string()),
                ..Default::default()
            }],
            notes: vec![],
            stat_books: vec![],
            stat_pages: vec![],
        };

        let bytes = req.to_wire_bytes().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v["books"][0]["metadata"],
            serde_json::json!({"publisher": "InkPress", "lang": "zh"})
        );
        assert_eq!(
            v["configs"][0]["rsvpPosition"],
            serde_json::json!({"wordIndex": 12})
        );
        assert_eq!(
            v["configs"][0]["searchConfig"],
            serde_json::json!({"matchCase": true})
        );
        assert_eq!(
            v["configs"][0]["viewSettings"],
            serde_json::json!({"fontSize": 18})
        );

        // Non-JSON string falls back cleanly to a JSON string value.
        let fallback_req = SyncPushRequest {
            books: vec![DbBook {
                book_hash: "hash02".to_string(),
                format: "EPUB".to_string(),
                title: "Fallback".to_string(),
                author: "Author".to_string(),
                metadata: Some("raw-non-json".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let fallback_v: serde_json::Value =
            serde_json::from_slice(&fallback_req.to_wire_bytes().unwrap()).unwrap();
        assert_eq!(
            fallback_v["books"][0]["metadata"],
            serde_json::Value::String("raw-non-json".to_string())
        );
    }
}
