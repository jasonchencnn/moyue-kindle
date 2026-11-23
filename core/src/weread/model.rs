//! Wire models for the WeRead E-Ink endpoints (contract §14.3).
//!
//! Every struct ignores unknown JSON fields; optional fields tolerate older servers that omit
//! them. Only the fields the client needs are typed, so a server-side addition cannot break sync.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON scalar rendered as a string: WeRead sends `vid` as either a number or a string.
pub(crate) fn as_string_or_number(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// `GET /shelf/sync` response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShelfSync {
    #[serde(default)]
    pub books: Vec<ShelfBook>,
    #[serde(default)]
    pub synckey: Option<i64>,
}

/// Tolerant boolean deserializer: WeRead sends flags like `finished` and `secret`
/// as either `0`/`1` integers or JSON booleans.
fn de_opt_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(b)),
        Some(Value::Number(n)) => Ok(Some(n.as_i64().unwrap_or(0) != 0)),
        Some(Value::String(s)) => match s.as_str() {
            "1" | "true" | "TRUE" => Ok(Some(true)),
            "0" | "false" | "FALSE" | "" => Ok(Some(false)),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// One shelf row (§14.3: `bookId/title/format/finished/secret/progress/readingTime/archive`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShelfBook {
    #[serde(rename = "bookId", default)]
    pub book_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub finished: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_bool")]
    pub secret: Option<bool>,
    #[serde(default)]
    pub progress: Option<i64>,
    #[serde(rename = "readingTime", default)]
    pub reading_time: Option<i64>,
    #[serde(default)]
    pub archive: Option<Value>,
}

/// `GET /book/getProgress?bookId=` response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BookProgressResponse {
    #[serde(rename = "bookId", default)]
    pub book_id: Option<String>,
    #[serde(default)]
    pub book: Option<ProgressBook>,
    #[serde(default)]
    pub tts: Option<TtsProgress>,
    #[serde(rename = "canFreeRead", default)]
    pub can_free_read: Option<i64>,
    #[serde(default)]
    pub timestamp: Option<i64>,
}

/// The cloud position record. The coordinate triple is `(chapterUid, chapterIdx, chapterOffset)`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProgressBook {
    #[serde(rename = "appId", default)]
    pub app_id: Option<String>,
    #[serde(rename = "bookVersion", default)]
    pub book_version: Option<i64>,
    #[serde(rename = "chapterUid", default)]
    pub chapter_uid: Option<i64>,
    #[serde(rename = "chapterIdx", default)]
    pub chapter_idx: Option<i64>,
    #[serde(rename = "chapterOffset", default)]
    pub chapter_offset: Option<i64>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub progress: Option<i64>,
    #[serde(rename = "readingTime", default)]
    pub reading_time: Option<i64>,
}

impl ProgressBook {
    /// The coordinate triple, when the server returned all three parts.
    pub fn position(&self) -> Option<ReadingPosition> {
        Some(ReadingPosition {
            chapter_uid: self.chapter_uid?,
            chapter_idx: self.chapter_idx?,
            chapter_offset: self.chapter_offset?,
        })
    }
}

/// WeRead reading coordinate: chapter uid + chapter index + character offset in the chapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingPosition {
    pub chapter_uid: i64,
    pub chapter_idx: i64,
    pub chapter_offset: i64,
}

/// Optional TTS progress record returned next to `book`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TtsProgress {
    #[serde(rename = "appId", default)]
    pub app_id: Option<String>,
    #[serde(rename = "bookVersion", default)]
    pub book_version: Option<i64>,
    #[serde(rename = "chapterUid", default)]
    pub chapter_uid: Option<i64>,
    #[serde(rename = "chapterIdx", default)]
    pub chapter_idx: Option<i64>,
    #[serde(rename = "chapterOffset", default)]
    pub chapter_offset: Option<i64>,
    #[serde(default)]
    pub summary: Option<String>,
}

/// `book` object of a `chapterInfos` response: the download's version and
/// `synckey` fallbacks when `getProgress` has no `bookVersion` (§14.3).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChapterInfosBook {
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default)]
    pub synckey: Option<i64>,
}

/// `POST /book/chapterInfos` response. Some servers wrap entries in `data`, some answer directly.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChapterInfosResponse {
    #[serde(default)]
    pub data: Vec<BookChapters>,
    #[serde(rename = "bookId", default)]
    pub book_id: Option<String>,
    #[serde(default)]
    pub book: Option<ChapterInfosBook>,
    #[serde(default)]
    pub synckey: Option<i64>,
    #[serde(default)]
    pub chapters: Vec<ChapterInfo>,
    #[serde(default)]
    pub updated: Vec<ChapterInfo>,
    #[serde(rename = "chapterInfos", default)]
    pub chapter_infos: Vec<ChapterInfo>,
}

impl ChapterInfosResponse {
    /// Normalize either wire shape for `book_id`.
    pub fn into_chapters(self, book_id: &str) -> ChapterInfos {
        let wanted = |candidate: &str| candidate.is_empty() || candidate == book_id;
        let top_book = self.book;
        for entry in self.data {
            if wanted(&entry.book_id) {
                let synckey = entry
                    .synckey
                    .or_else(|| entry.book.as_ref().and_then(|book| book.synckey));
                let book_version = entry
                    .book
                    .as_ref()
                    .and_then(|book| book.version)
                    .or_else(|| top_book.as_ref().and_then(|book| book.version));
                let chapters = entry.into_chapters();
                if !chapters.is_empty() {
                    return ChapterInfos {
                        book_id: book_id.to_string(),
                        synckey,
                        book_version,
                        chapters,
                    };
                }
            }
        }
        let direct = if !self.updated.is_empty() {
            self.updated
        } else if !self.chapters.is_empty() {
            self.chapters
        } else {
            self.chapter_infos
        };
        ChapterInfos {
            book_id: book_id.to_string(),
            synckey: self
                .synckey
                .or_else(|| top_book.as_ref().and_then(|book| book.synckey)),
            book_version: top_book.and_then(|book| book.version),
            chapters: direct,
        }
    }
}

/// One book inside a `chapterInfos` response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BookChapters {
    #[serde(rename = "bookId", default)]
    pub book_id: String,
    #[serde(default)]
    pub book: Option<ChapterInfosBook>,
    #[serde(default)]
    pub synckey: Option<i64>,
    #[serde(default)]
    pub chapters: Vec<ChapterInfo>,
    #[serde(default)]
    pub updated: Vec<ChapterInfo>,
    #[serde(rename = "chapterInfos", default)]
    pub chapter_infos: Vec<ChapterInfo>,
}

impl BookChapters {
    fn into_chapters(self) -> Vec<ChapterInfo> {
        if !self.updated.is_empty() {
            self.updated
        } else if !self.chapters.is_empty() {
            self.chapters
        } else {
            self.chapter_infos
        }
    }
}

/// A normalized table of contents.
#[derive(Debug, Clone, Default)]
pub struct ChapterInfos {
    pub book_id: String,
    pub synckey: Option<i64>,
    /// `book.version` sent with the TOC, when present; the first fallback
    /// for a missing `getProgress` `bookVersion` (§14.3).
    pub book_version: Option<i64>,
    pub chapters: Vec<ChapterInfo>,
}

/// One TOC row (§14.3: `chapterUid/chapterIdx/title/wordCount/price/paid/tar?/files[]`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChapterInfo {
    #[serde(rename = "chapterUid", default)]
    pub chapter_uid: Option<i64>,
    #[serde(rename = "chapterIdx", default)]
    pub chapter_idx: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "wordCount", default)]
    pub word_count: Option<i64>,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub paid: Option<i64>,
    #[serde(default)]
    pub tar: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
}

impl ChapterInfo {
    /// Contract §14.3 red line: only `price<=0` or `paid>0` chapters may be requested.
    pub fn is_eligible(&self) -> bool {
        self.price.is_some_and(|price| price <= 0.0) || self.paid.unwrap_or(0) > 0
    }

    /// The chapter uid, or a typed error when the TOC omitted it.
    pub fn uid(&self) -> Result<i64, crate::weread::WeReadError> {
        self.chapter_uid.ok_or_else(|| {
            crate::weread::WeReadError::InvalidParameter(
                "TOC chapter is missing chapterUid".to_string(),
            )
        })
    }
}

/// `info.txt` inside a chapter ZIP: the chapter list and its file names.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChapterPackageManifest {
    #[serde(default)]
    pub chapters: Vec<ManifestChapter>,
}

/// One chapter entry of `info.txt`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ManifestChapter {
    #[serde(rename = "chapterUid", default)]
    pub chapter_uid: Option<i64>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub tar: Option<String>,
}

impl ChapterPackageManifest {
    /// Parse a synthetic or real `info.txt` document.
    pub fn from_json(bytes: &[u8]) -> Result<Self, crate::weread::WeReadError> {
        serde_json::from_slice(bytes).map_err(|error| {
            crate::weread::WeReadError::InvalidResponse(format!("info.txt: {error}"))
        })
    }
}

/// One GMT+8-aligned hour bucket (§14.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadingHour {
    #[serde(rename = "startOfHour")]
    pub start_of_hour: i64,
    #[serde(rename = "timeZone")]
    pub time_zone: String,
    #[serde(rename = "readingTime")]
    pub reading_time: i64,
    #[serde(rename = "ttsTime")]
    pub tts_time: i64,
}

/// A reading interval to split into hour buckets. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingInterval {
    pub start_time: i64,
    pub duration: i64,
    pub time_zone: String,
    pub tts_time: i64,
}

impl ReadingInterval {
    /// A reading interval without TTS time.
    pub fn new(start_time: i64, duration: i64, time_zone: impl Into<String>) -> Self {
        Self {
            start_time,
            duration,
            time_zone: time_zone.into(),
            tts_time: 0,
        }
    }
}

/// `BaseBookReadPostBody`: the time-only DTO. Coordinates are present only for a heartbeat echo.
#[derive(Debug, Clone, Serialize)]
pub struct BaseBookReadPostBody {
    #[serde(rename = "bookId")]
    pub book_id: String,
    #[serde(rename = "bookVersion")]
    pub book_version: i64,
    #[serde(rename = "appId")]
    pub app_id: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "installId")]
    pub install_id: String,
    pub summary: String,
    pub progress: i64,
    #[serde(rename = "currentProgress")]
    pub current_progress: i64,
    #[serde(rename = "autoTime")]
    pub auto_time: i64,
    #[serde(rename = "isLecture")]
    pub is_lecture: i64,
    #[serde(rename = "isResendReadingInfo")]
    pub is_resend_reading_info: i64,
    #[serde(rename = "lectureTime")]
    pub lecture_time: i64,
    #[serde(rename = "lectureTextTime")]
    pub lecture_text_time: i64,
    #[serde(rename = "novalTime")]
    pub noval_time: i64,
    #[serde(rename = "ttsTime")]
    pub tts_time: i64,
    pub finish: bool,
    pub risk: i64,
    #[serde(rename = "readingTime")]
    pub reading_time: i64,
    #[serde(rename = "recordCreateTimeZone")]
    pub record_create_time_zone: String,
    pub timestamp: i64,
    pub random: u16,
    pub hours: Vec<ReadingHour>,
    pub signature: String,
    #[serde(rename = "chapterUid", skip_serializing_if = "Option::is_none")]
    pub chapter_uid: Option<i64>,
    #[serde(rename = "chapterIdx", skip_serializing_if = "Option::is_none")]
    pub chapter_idx: Option<i64>,
    #[serde(rename = "chapterOffset", skip_serializing_if = "Option::is_none")]
    pub chapter_offset: Option<i64>,
}

/// `POST /book/batchUploadProgress` body.
#[derive(Debug, Clone, Serialize)]
pub struct BatchUploadBody {
    pub books: Vec<BaseBookReadPostBody>,
    pub timestamp: i64,
    pub random: u16,
    pub signature: String,
}
