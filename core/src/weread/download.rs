//! WeRead chapter download pipeline and local book mapping (WR-003).
//!
//! The pipeline is intentionally split into small, testable steps:
//! entitlement filtering, per-chapter `chapterdownload` with a persistent cache,
//! ZipCrypto extraction, EPUB 2 rebuild, `partialMD5` self-check, atomic output
//! replacement, and a versioned `bookId -> {hash, title, format, synckey}` JSON
//! store. HTTP is injected through [`HttpTransport`]; this module never opens a
//! socket of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::hash::partial_md5;
use crate::sync::transport::HttpTransport;
use crate::weread::client::{BookFormat, WeReadClient};
use crate::weread::epub::{
    build_epub, extract_chapter_package, find_info_txt, is_info_txt_path, normalize_archive_path,
    BuiltEpub, EpubChapter, EpubError, EpubSource, ExtractedPackage, SpineEntry,
};
use crate::weread::model::{ChapterInfo, ChapterInfos};
use crate::weread::WeReadError;

/// Schema version of the on-disk `bookId -> hash` mapping document.
pub const BOOK_MAPPING_SCHEMA_VERSION: u32 = 1;

const CACHE_SCHEMA_VERSION: u32 = 1;
const EPUB_FORMAT: &str = "epub";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Book container format reported by the shelf row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    /// EPUB: the only format v1 can rebuild (contract §14.3).
    Epub,
    /// TXT: no rebuild specification in contract §14 yet.
    Txt,
    /// PDF: needs the server-side `/trans/pdfepub` conversion (not in §14 yet).
    Pdf,
}

impl SourceFormat {
    /// Parse a shelf `format` value, case-insensitively.
    pub fn parse(raw: &str) -> Result<Self, DownloadError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "epub" => Ok(Self::Epub),
            "txt" => Ok(Self::Txt),
            "pdf" => Ok(Self::Pdf),
            other => Err(DownloadError::UnsupportedFormat(other.to_string())),
        }
    }

    /// Parse the optional `format` field of a §14.3 shelf row.
    pub fn from_shelf(raw: Option<&str>) -> Result<Self, DownloadError> {
        raw.ok_or(DownloadError::MissingFormat)
            .and_then(Self::parse)
    }

    /// Wire value used in the mapping document.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Txt => "txt",
            Self::Pdf => "pdf",
        }
    }
}

/// One chapter removed by the §14.3 entitlement red line.
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedChapter {
    /// WeRead `chapterUid`.
    pub chapter_uid: i64,
    /// WeRead `chapterIdx`, when the TOC provided it.
    pub chapter_idx: Option<i64>,
    /// Chapter title from the TOC.
    pub title: String,
    /// TOC `price`; `-1.0` when the server omitted it.
    pub price: f64,
    /// TOC `paid`; `0` when the server omitted it.
    pub paid: i64,
}

/// The filtered download set for one book.
#[derive(Debug, Clone, Default)]
pub struct DownloadPlan {
    /// Chapters that passed the entitlement check, sorted by `chapter_idx`.
    pub eligible: Vec<ChapterInfo>,
    /// Chapters removed before any HTTP request.
    pub skipped: Vec<SkippedChapter>,
}

/// One download-and-rebuild request. All paths are caller-owned.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// WeRead book id.
    pub book_id: String,
    /// Shelf title written into the mapping.
    pub title: String,
    /// Source format; v1 accepts only [`SourceFormat::Epub`].
    pub format: SourceFormat,
    /// Shelf/`getProgress` `bookVersion` used by `chapterdownload`.
    pub book_version: String,
    /// Target chapters (`chapterUid`) to rebuild. Chapter 0 is implicit.
    pub chapters: Vec<i64>,
    /// Final EPUB destination.
    pub epub_path: PathBuf,
    /// `bookId -> {hash, title, format, synckey}` JSON destination.
    pub mapping_path: PathBuf,
    /// Persistent chapter cache and `.part` staging directory.
    pub work_dir: PathBuf,
}

/// Result of a successful download-and-rebuild run.
#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    /// WeRead book id.
    pub book_id: String,
    /// `partialMD5` of the rebuilt EPUB.
    pub hash: String,
    /// Final EPUB path that was committed.
    pub epub_path: PathBuf,
    /// Mapping file that was updated.
    pub mapping_path: PathBuf,
    /// Chapters fetched from the network during this run.
    pub downloaded: Vec<i64>,
    /// Chapters reused from the persistent cache during this run.
    pub cached: Vec<i64>,
    /// Ineligible chapters removed before the request phase.
    pub skipped: Vec<SkippedChapter>,
    /// Spine mapping retained for the later XPointer/WR-004 work.
    pub spine: Vec<SpineEntry>,
}

/// Pipeline failures.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// The shelf row had no `format` field.
    #[error("book format is missing from the shelf row")]
    MissingFormat,
    /// The format has no v1 rebuild specification.
    #[error("format '{0}' is not supported yet; v1 only rebuilds EPUB (contract §14.3)")]
    UnsupportedFormat(String),
    /// The request has no book id.
    #[error("book id is empty")]
    EmptyBookId,
    /// EPUB `chapterdownload` needs a non-empty `bookVersion`.
    #[error("book version is empty")]
    EmptyBookVersion,
    /// The TOC belongs to another book.
    #[error("table of contents belongs to book {toc}, not {requested}")]
    BookIdMismatch { toc: String, requested: String },
    /// No target chapters were supplied.
    #[error("target chapter set is empty")]
    NoTargetChapters,
    /// A planned chapter unexpectedly lost its `chapterUid`.
    #[error("planned chapter has no chapterUid")]
    MissingChapterUid,
    /// A target chapter is not present in the TOC.
    #[error("chapter {0} is not in the table of contents")]
    UnknownChapter(i64),
    /// A chapter has no `chapterIdx`, so spine order cannot be established.
    #[error("chapter {0} has no chapterIdx")]
    MissingChapterIdx(i64),
    /// Every target chapter was ineligible; no HTTP request was made.
    #[error("no eligible chapters to download; {} chapter(s) skipped", .skipped.len())]
    NoEligibleChapters { skipped: Vec<SkippedChapter> },
    /// A downloaded package has no selected XHTML body for the chapter.
    #[error("chapter {chapter_uid} has no XHTML body in its package")]
    MissingChapterText { chapter_uid: i64 },
    /// The package manifest points at a file that is not in the ZIP.
    #[error("chapter {chapter_uid} package is missing {href}")]
    MissingPackageFile { chapter_uid: i64, href: String },
    /// The package has several text files and no manifest entry to choose from.
    #[error("chapter {chapter_uid} package manifest is missing or ambiguous")]
    MissingChapterManifest { chapter_uid: i64 },
    /// A cache entry cannot be reused and should be downloaded again.
    #[error("chapter {chapter_uid} cache is not reusable: {detail}")]
    InvalidCache { chapter_uid: i64, detail: String },
    /// The written EPUB hash does not match the in-memory hash.
    #[error("output hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    /// Local filesystem failure, with the path formatted into the message.
    #[error("storage error: {0}")]
    Storage(String),
    /// WeRead endpoint failure (including the §14.3 entitlement red line).
    #[error(transparent)]
    WeRead(#[from] WeReadError),
    /// EPUB extraction/rebuild failure.
    #[error(transparent)]
    Epub(#[from] EpubError),
    /// Mapping store failure.
    #[error(transparent)]
    Mapping(#[from] MappingError),
}

/// One mapping row: `bookId -> {hash, title, format, synckey}` (§14.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMapping {
    /// `partialMD5` of the rebuilt EPUB.
    pub hash: String,
    /// Shelf title.
    pub title: String,
    /// Source format; v1 writes `epub`.
    pub format: String,
    /// TOC `synckey` captured at download time.
    pub synckey: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MappingDocument {
    version: u32,
    books: BTreeMap<String, BookMapping>,
}

/// Failures of the versioned `bookId -> hash` JSON store.
#[derive(Debug, thiserror::Error)]
pub enum MappingError {
    /// The file could not be read or atomically replaced.
    #[error("cannot access mapping file {path}: {source}")]
    Io {
        /// File being accessed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid JSON or has the wrong shape.
    #[error("mapping file {path} is invalid JSON: {source}")]
    Json {
        /// File being parsed.
        path: PathBuf,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },
    /// The document declares an unsupported schema version.
    #[error("mapping schema version {found} is not supported (expected {expected})")]
    UnsupportedVersion {
        /// Version found on disk.
        found: u32,
        /// Version supported by this client.
        expected: u32,
    },
}

impl MappingError {
    fn is_recoverable(&self) -> bool {
        matches!(self, Self::Json { .. })
    }
}

/// In-memory mapping table backed by a versioned JSON file.
#[derive(Debug, Clone)]
pub struct BookMappingStore {
    path: PathBuf,
    books: BTreeMap<String, BookMapping>,
}

impl BookMappingStore {
    /// Load a mapping store, falling back to the sibling `*.bak` generation.
    ///
    /// A missing primary and backup starts an empty store. An unparsable
    /// primary falls back to the backup; an unsupported schema version is a
    /// hard error and never silently ignores newer data.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, MappingError> {
        let path = path.into();
        let backup = append_suffix(&path, ".bak");
        let primary_error = match read_mapping_document(&path) {
            Ok(Some(document)) => {
                return Ok(Self {
                    path,
                    books: document.books,
                })
            }
            Ok(None) => None,
            Err(error) if error.is_recoverable() => Some(error),
            Err(error) => return Err(error),
        };
        match read_mapping_document(&backup) {
            Ok(Some(document)) => Ok(Self {
                path,
                books: document.books,
            }),
            Ok(None) => match primary_error {
                Some(error) => Err(error),
                None => Ok(Self {
                    path,
                    books: BTreeMap::new(),
                }),
            },
            Err(error) => Err(error),
        }
    }

    /// The store file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The mapping for `book_id`, if present.
    pub fn get(&self, book_id: &str) -> Option<&BookMapping> {
        self.books.get(book_id)
    }

    /// Insert or replace one mapping row.
    pub fn insert(&mut self, book_id: impl Into<String>, mapping: BookMapping) {
        self.books.insert(book_id.into(), mapping);
    }

    /// Remove one mapping row.
    pub fn remove(&mut self, book_id: &str) -> Option<BookMapping> {
        self.books.remove(book_id)
    }

    /// Iterate rows in book-id order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &BookMapping)> {
        self.books
            .iter()
            .map(|(book_id, mapping)| (book_id.as_str(), mapping))
    }

    /// Number of mapping rows.
    pub fn len(&self) -> usize {
        self.books.len()
    }

    /// Whether the store has no rows.
    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }

    /// Persist the store atomically, keeping the previous generation as `*.bak`.
    pub fn save(&self) -> Result<(), MappingError> {
        let document = MappingDocument {
            version: BOOK_MAPPING_SCHEMA_VERSION,
            books: self.books.clone(),
        };
        let mut bytes =
            serde_json::to_vec_pretty(&document).map_err(|source| MappingError::Json {
                path: self.path.clone(),
                source,
            })?;
        bytes.push(b'\n');
        write_atomic(&self.path, &bytes).map_err(|source| MappingError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

fn read_mapping_document(path: &Path) -> Result<Option<MappingDocument>, MappingError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MappingError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let document: MappingDocument =
        serde_json::from_slice(&bytes).map_err(|source| MappingError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    if document.version != BOOK_MAPPING_SCHEMA_VERSION {
        return Err(MappingError::UnsupportedVersion {
            found: document.version,
            expected: BOOK_MAPPING_SCHEMA_VERSION,
        });
    }
    Ok(Some(document))
}

/// Filter the target chapter set before any request is sent.
///
/// Chapter 0 is the shared-resource package and is never a spine target; it is
/// downloaded implicitly by [`download_book`]. Chapters failing the §14.3
/// red line are returned in `skipped` and are never requested.
pub fn plan_download(toc: &ChapterInfos, targets: &[i64]) -> Result<DownloadPlan, DownloadError> {
    if targets.is_empty() {
        return Err(DownloadError::NoTargetChapters);
    }
    let mut seen = BTreeSet::new();
    let mut eligible = Vec::new();
    let mut skipped = Vec::new();
    for &chapter_uid in targets {
        if chapter_uid == 0 || !seen.insert(chapter_uid) {
            continue;
        }
        let chapter = toc
            .chapters
            .iter()
            .find(|chapter| chapter.chapter_uid == Some(chapter_uid))
            .ok_or(DownloadError::UnknownChapter(chapter_uid))?;
        if !chapter.is_eligible() {
            skipped.push(SkippedChapter {
                chapter_uid,
                chapter_idx: chapter.chapter_idx,
                title: chapter.title.clone().unwrap_or_default(),
                price: chapter.price.unwrap_or(-1.0),
                paid: chapter.paid.unwrap_or(0),
            });
            continue;
        }
        if chapter.chapter_idx.is_none() {
            return Err(DownloadError::MissingChapterIdx(chapter_uid));
        }
        eligible.push(chapter.clone());
    }
    eligible.sort_by_key(|chapter| {
        (
            chapter.chapter_idx.unwrap_or(i64::MAX),
            chapter.chapter_uid.unwrap_or(i64::MAX),
        )
    });
    if eligible.is_empty() {
        return Err(DownloadError::NoEligibleChapters { skipped });
    }
    Ok(DownloadPlan { eligible, skipped })
}

/// Download the missing chapters, rebuild one EPUB, and update the mapping store.
///
/// The entitlement filter runs before the first HTTP request. Eligible chapters
/// are fetched in small batches with automatic single-chapter fallback, keeping
/// all chapter images/styles across every batch and persisting a resumable cache
/// under `work_dir`.
///
/// Package caches are staged as `packages/.tmp-pkg-*`; stale staging trees are
/// swept before the run and again before returning, so both the success and the
/// failure path leave no `.tmp-*` directory behind.
pub fn download_book<T: HttpTransport>(
    client: &mut WeReadClient<T>,
    toc: &ChapterInfos,
    request: &DownloadRequest,
) -> Result<DownloadOutcome, DownloadError> {
    download_book_with_progress(client, toc, request, |_, _| {})
}

/// Download the missing chapters with per-chapter progress reporting `(done, total)`.
pub fn download_book_with_progress<T, F>(
    client: &mut WeReadClient<T>,
    toc: &ChapterInfos,
    request: &DownloadRequest,
    mut on_progress: F,
) -> Result<DownloadOutcome, DownloadError>
where
    T: HttpTransport,
    F: FnMut(usize, usize),
{
    let result = download_book_inner(client, toc, request, &mut on_progress);
    cleanup_stale_temp_dirs(&request.work_dir);
    if let Err(ref error) = result {
        if should_purge_cache_on_error(error) {
            purge_download_cache(&request.work_dir);
        }
    }
    result
}

/// Purge cached chapter/public trees when assembly fails so the next retry
/// downloads fresh packages instead of looping on corrupted cache data.
fn should_purge_cache_on_error(error: &DownloadError) -> bool {
    matches!(
        error,
        DownloadError::Epub(_)
            | DownloadError::InvalidCache { .. }
            | DownloadError::MissingChapterManifest { .. }
            | DownloadError::MissingPackageFile { .. }
            | DownloadError::MissingChapterText { .. }
    )
}

fn purge_download_cache(work_dir: &Path) {
    discard_cache(&work_dir.join("packages"));
    discard_cache(&work_dir.join("public"));
}

/// Errors that affect the whole session or book rather than a multi-chapter batch;
/// never swallow these and spam single-chapter retries.
fn is_fatal_batch_error(error: &WeReadError) -> bool {
    match error {
        WeReadError::AuthExpired
        | WeReadError::AuthRejected(_)
        | WeReadError::NotAuthenticated
        | WeReadError::WebAuthExpired
        | WeReadError::ManualVerification(_)
        | WeReadError::RateLimited(_)
        | WeReadError::IneligibleChapter { .. }
        | WeReadError::Blocked(_) => true,
        WeReadError::Api { code: -2063, .. } => true,
        WeReadError::HttpStatus(401 | 403 | 429, _) => true,
        WeReadError::Transport(msg) if msg.eq_ignore_ascii_case("cancelled") => true,
        _ => false,
    }
}

fn is_retryable_single_chapter_error(error: &WeReadError) -> bool {
    if is_fatal_batch_error(error) {
        return false;
    }
    match error {
        WeReadError::Transport(msg) => !msg.eq_ignore_ascii_case("cancelled"),
        WeReadError::HttpStatus(500..=599, _) => true,
        _ => false,
    }
}

fn adaptive_batch_size(remaining: usize) -> usize {
    if remaining > 50 {
        10
    } else if remaining > 15 {
        5
    } else if remaining > 2 {
        3
    } else {
        1
    }
}

fn collect_package_assets(package: &ExtractedPackage, assets: &mut BTreeMap<String, Vec<u8>>) {
    for (path, bytes) in &package.files {
        if is_asset_path(path) {
            assets.entry(path.clone()).or_insert_with(|| bytes.clone());
        }
    }
}

fn download_book_inner<T, F>(
    client: &mut WeReadClient<T>,
    toc: &ChapterInfos,
    request: &DownloadRequest,
    on_progress: &mut F,
) -> Result<DownloadOutcome, DownloadError>
where
    T: HttpTransport,
    F: FnMut(usize, usize),
{
    if request.format != SourceFormat::Epub {
        return Err(DownloadError::UnsupportedFormat(
            request.format.as_str().to_string(),
        ));
    }
    validate_request(toc, request)?;
    let plan = plan_download(toc, &request.chapters)?;
    let total_chapters = plan.eligible.len();

    prepare_work_dir(&request.work_dir)?;
    cleanup_stale_temp_dirs(&request.work_dir);
    cleanup_stale_part(&request.epub_path)?;

    let packages_dir = request.work_dir.join("packages");
    fs::create_dir_all(&packages_dir).map_err(|error| storage_path(&packages_dir, error))?;

    let mut downloaded = Vec::new();
    let mut cached = Vec::new();
    let mut to_download: Vec<&ChapterInfo> = Vec::new();
    let mut chapter_files_map: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    let mut texts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut assets: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let mut has_public =
        public_cache_valid(&request.work_dir, &request.book_id, &request.book_version);
    if has_public {
        let public_dir = request.work_dir.join("public");
        if public_dir.exists() {
            for (path, bytes) in read_tree(&public_dir)? {
                if is_asset_path(&path) {
                    assets.entry(path).or_insert(bytes);
                }
            }
        }
    }

    for chapter in &plan.eligible {
        let chapter_uid = chapter
            .chapter_uid
            .ok_or(DownloadError::MissingChapterUid)?;
        if has_public {
            if let Some(pkg) = load_package_cache(
                &packages_dir,
                &request.book_id,
                &request.book_version,
                chapter_uid,
                chapter,
            )? {
                let files = select_chapter_files(&pkg, chapter)?;
                for file in &files {
                    if let Some(bytes) = pkg.files.get(file) {
                        texts.entry(file.clone()).or_insert_with(|| bytes.clone());
                    }
                }
                collect_package_assets(&pkg, &mut assets);
                chapter_files_map.insert(chapter_uid, files);
                cached.push(chapter_uid);
                continue;
            }
        }
        to_download.push(chapter);
        // Once the first chapter is queued to fetch chapter 0, any remaining
        // cached chapters can safely reuse their local cache.
        has_public = true;
    }

    let mut completed = cached.len();
    if completed > 0 {
        on_progress(completed, total_chapters);
    }

    // Adaptive batch sizing: up to 10 chapters/request on large books to cut
    // HTTP roundtrips in half while keeping small-book progress responsive.
    let batch_size = adaptive_batch_size(to_download.len());

    for chunk in to_download.chunks(batch_size) {
        let chunk_owned: Vec<ChapterInfo> = chunk.iter().map(|c| (*c).clone()).collect();
        let need_public =
            !public_cache_valid(&request.work_dir, &request.book_id, &request.book_version);
        if chunk_owned.len() > 1 {
            match client.chapterdownload_with_public(
                &request.book_id,
                BookFormat::Epub,
                &request.book_version,
                &chunk_owned,
                need_public,
            ) {
                Ok(package) => {
                    if let Ok(extracted) = extract_chapter_package(&package.zip, &package.password)
                    {
                        // Always merge every batch's assets into both the
                        // persistent public cache and in-memory assets so batch
                        // 2..N illustrations, fonts, and stylesheets are never dropped.
                        merge_public_cache(
                            &request.work_dir,
                            &request.book_id,
                            &request.book_version,
                            &extracted,
                        )?;
                        collect_package_assets(&extracted, &mut assets);

                        let mut missing_in_batch = Vec::new();
                        let mut attached_batch_assets = need_public;
                        for chapter in &chunk_owned {
                            let chapter_uid = chapter
                                .chapter_uid
                                .ok_or(DownloadError::MissingChapterUid)?;
                            if let Ok(files) = select_chapter_files(&extracted, chapter) {
                                let mut subset = BTreeMap::new();
                                if let Some((_, info_bytes)) = find_info_txt(&extracted.files) {
                                    subset.insert("info.txt".to_string(), info_bytes.to_vec());
                                }
                                for href in &files {
                                    if let Some(bytes) = extracted.files.get(href) {
                                        texts.entry(href.clone()).or_insert_with(|| bytes.clone());
                                        subset.insert(href.clone(), bytes.clone());
                                    }
                                }
                                // Keep batch 2..N per-chapter assets on the
                                // first cached chapter of the batch as well as `public/`
                                // so deleting `public/` and resuming never drops them.
                                if !attached_batch_assets {
                                    for (path, bytes) in &extracted.files {
                                        if is_asset_path(path) {
                                            subset.insert(path.clone(), bytes.clone());
                                        }
                                    }
                                    attached_batch_assets = true;
                                }
                                let chapter_pkg = ExtractedPackage {
                                    info: extracted.info.clone(),
                                    files: subset,
                                };
                                write_package_cache(
                                    &packages_dir,
                                    &request.book_id,
                                    &request.book_version,
                                    chapter_uid,
                                    &chapter_pkg,
                                    false,
                                )?;
                                chapter_files_map.insert(chapter_uid, files);
                                downloaded.push(chapter_uid);
                                completed += 1;
                                on_progress(completed.min(total_chapters), total_chapters);
                            } else {
                                missing_in_batch.push(chapter.clone());
                            }
                        }
                        if missing_in_batch.is_empty() {
                            continue;
                        }
                        for chapter in &missing_in_batch {
                            let (uid, files) = download_single_chapter(
                                client,
                                request,
                                &packages_dir,
                                chapter,
                                &mut texts,
                                &mut assets,
                                &mut downloaded,
                            )?;
                            chapter_files_map.insert(uid, files);
                            completed += 1;
                            on_progress(completed.min(total_chapters), total_chapters);
                        }
                        continue;
                    }
                }
                Err(error) if is_fatal_batch_error(&error) => {
                    cleanup_stale_part(&request.epub_path)?;
                    return Err(DownloadError::WeRead(error));
                }
                Err(_) => {
                    // Recoverable batch failure (e.g. TooLarge/Timeout on an
                    // illustrated batch): fall back to single-chapter fetches.
                }
            }
        }

        for chapter in &chunk_owned {
            let (uid, files) = download_single_chapter(
                client,
                request,
                &packages_dir,
                chapter,
                &mut texts,
                &mut assets,
                &mut downloaded,
            )?;
            chapter_files_map.insert(uid, files);
            completed += 1;
            on_progress(completed.min(total_chapters), total_chapters);
        }
    }

    let mut chapters = Vec::with_capacity(plan.eligible.len());
    for chapter in &plan.eligible {
        let chapter_uid = chapter
            .chapter_uid
            .ok_or(DownloadError::MissingChapterUid)?;
        let files =
            chapter_files_map
                .remove(&chapter_uid)
                .ok_or_else(|| DownloadError::InvalidCache {
                    chapter_uid,
                    detail: "chapter files missing after download phase".to_string(),
                })?;
        for file in &files {
            if !texts.contains_key(file) {
                return Err(DownloadError::MissingPackageFile {
                    chapter_uid,
                    href: file.clone(),
                });
            }
        }
        chapters.push(EpubChapter {
            chapter_uid,
            chapter_idx: chapter.chapter_idx.unwrap_or_default(),
            title: chapter
                .title
                .clone()
                .unwrap_or_else(|| format!("Chapter {chapter_uid}")),
            files,
        });
    }

    let source = EpubSource {
        book_id: request.book_id.clone(),
        title: request.title.clone(),
        chapters,
        texts,
        assets,
    };
    let built = build_epub(&source)?;
    write_epub_atomic(&request.epub_path, &built)?;

    let mut mapping = BookMappingStore::load(&request.mapping_path)?;
    mapping.insert(
        &request.book_id,
        BookMapping {
            hash: built.hash.clone(),
            title: request.title.clone(),
            format: EPUB_FORMAT.to_string(),
            synckey: toc.synckey,
        },
    );
    mapping.save()?;

    Ok(DownloadOutcome {
        book_id: request.book_id.clone(),
        hash: built.hash,
        epub_path: request.epub_path.clone(),
        mapping_path: request.mapping_path.clone(),
        downloaded,
        cached,
        skipped: plan.skipped,
        spine: built.spine,
    })
}

const SINGLE_CHAPTER_MAX_RETRIES: usize = 2;
const SINGLE_CHAPTER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

fn download_single_chapter<T: HttpTransport>(
    client: &mut WeReadClient<T>,
    request: &DownloadRequest,
    packages_dir: &Path,
    chapter: &ChapterInfo,
    texts: &mut BTreeMap<String, Vec<u8>>,
    assets: &mut BTreeMap<String, Vec<u8>>,
    downloaded: &mut Vec<i64>,
) -> Result<(i64, Vec<String>), DownloadError> {
    let chapter_uid = chapter
        .chapter_uid
        .ok_or(DownloadError::MissingChapterUid)?;
    let need_public =
        !public_cache_valid(&request.work_dir, &request.book_id, &request.book_version);
    let mut attempts = 0usize;
    let package = loop {
        match client.chapterdownload_with_public(
            &request.book_id,
            BookFormat::Epub,
            &request.book_version,
            std::slice::from_ref(chapter),
            need_public,
        ) {
            Ok(package) => break package,
            Err(error)
                if attempts < SINGLE_CHAPTER_MAX_RETRIES
                    && is_retryable_single_chapter_error(&error) =>
            {
                attempts += 1;
                std::thread::sleep(SINGLE_CHAPTER_RETRY_DELAY);
            }
            Err(error) => {
                cleanup_stale_part(&request.epub_path)?;
                return Err(DownloadError::WeRead(error));
            }
        }
    };
    let extracted = extract_chapter_package(&package.zip, &package.password)?;
    let files = select_chapter_files(&extracted, chapter)?;
    for file in &files {
        if let Some(bytes) = extracted.files.get(file) {
            texts.entry(file.clone()).or_insert_with(|| bytes.clone());
        }
    }
    collect_package_assets(&extracted, assets);
    merge_public_cache(
        &request.work_dir,
        &request.book_id,
        &request.book_version,
        &extracted,
    )?;
    write_package_cache(
        packages_dir,
        &request.book_id,
        &request.book_version,
        chapter_uid,
        &extracted,
        true,
    )?;
    downloaded.push(chapter_uid);
    Ok((chapter_uid, files))
}

fn validate_request(toc: &ChapterInfos, request: &DownloadRequest) -> Result<(), DownloadError> {
    if request.book_id.trim().is_empty() {
        return Err(DownloadError::EmptyBookId);
    }
    if request.book_version.trim().is_empty() {
        return Err(DownloadError::EmptyBookVersion);
    }
    if !toc.book_id.is_empty() && toc.book_id != request.book_id {
        return Err(DownloadError::BookIdMismatch {
            toc: toc.book_id.clone(),
            requested: request.book_id.clone(),
        });
    }
    if request.chapters.is_empty() {
        return Err(DownloadError::NoTargetChapters);
    }
    Ok(())
}

fn select_chapter_files(
    package: &ExtractedPackage,
    chapter: &ChapterInfo,
) -> Result<Vec<String>, DownloadError> {
    let chapter_uid = chapter
        .chapter_uid
        .ok_or(DownloadError::MissingChapterUid)?;
    let candidates: Vec<String> = if let Some(files) = package.chapter_files(chapter_uid) {
        files.to_vec()
    } else if !chapter.files.is_empty() {
        chapter.files.clone()
    } else {
        let fallback: Vec<String> = package
            .files
            .keys()
            .filter(|path| {
                is_xhtml(path)
                    && !is_placeholder_error_page(path)
                    && file_stem(path).parse::<i64>().ok() != Some(0)
            })
            .cloned()
            .collect();
        if fallback.len() == 1 {
            fallback
        } else {
            let matching: Vec<String> = fallback
                .into_iter()
                .filter(|path| file_stem(path).parse::<i64>().ok() == Some(chapter_uid))
                .collect();
            if matching.len() == 1 {
                matching
            } else {
                return Err(DownloadError::MissingChapterManifest { chapter_uid });
            }
        }
    };

    let mut selected = Vec::new();
    let mut first_missing: Option<String> = None;
    for candidate in candidates {
        let candidate = normalize_archive_path(&candidate)?;
        if !is_xhtml(&candidate) || is_placeholder_error_page(&candidate) {
            continue;
        }
        let resolved = if package.files.contains_key(&candidate) {
            Some(candidate.clone())
        } else {
            let under_text = format!("Text/{candidate}");
            if package.files.contains_key(&under_text) {
                Some(under_text)
            } else if let Some(found) = package.files.keys().find(|path| {
                !is_placeholder_error_page(path) && path.eq_ignore_ascii_case(&candidate)
            }) {
                Some(found.clone())
            } else {
                let needle = candidate
                    .rsplit_once('/')
                    .map(|(_, name)| name)
                    .unwrap_or(&candidate);
                package
                    .files
                    .keys()
                    .find(|path| {
                        is_xhtml(path)
                            && !is_placeholder_error_page(path)
                            && path
                                .rsplit_once('/')
                                .map(|(_, name)| name)
                                .unwrap_or(path)
                                .eq_ignore_ascii_case(needle)
                    })
                    .cloned()
            }
        };
        match resolved {
            Some(resolved) => {
                if !selected.contains(&resolved) {
                    selected.push(resolved);
                }
            }
            None => {
                first_missing.get_or_insert(candidate);
            }
        }
    }
    if selected.is_empty() {
        if let Some(href) = first_missing {
            Err(DownloadError::MissingPackageFile { chapter_uid, href })
        } else {
            Err(DownloadError::MissingChapterText { chapter_uid })
        }
    } else {
        Ok(selected)
    }
}

fn is_xhtml(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")
}

fn is_asset_path(path: &str) -> bool {
    !is_info_txt_path(path) && !is_xhtml(path)
}

/// WeRead injects `Text/ErrLossFileSourcePre.html` as a fallback placeholder in
/// some chapter ZIPs; never treat it as real chapter content.
fn is_placeholder_error_page(path: &str) -> bool {
    let name = path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path);
    name.eq_ignore_ascii_case("ErrLossFileSourcePre.html")
        || name.eq_ignore_ascii_case("ErrLossFileSourcePre.xhtml")
        || name.eq_ignore_ascii_case("ErrLossFileSourcePre.htm")
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit_once('/').map(|(_, name)| name).unwrap_or(path);
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheState {
    schema: u32,
    book_id: String,
    book_version: String,
    chapter_uid: i64,
}

fn package_dir(packages_dir: &Path, chapter_uid: i64) -> PathBuf {
    packages_dir.join(chapter_uid.to_string())
}

fn load_package_cache(
    packages_dir: &Path,
    book_id: &str,
    book_version: &str,
    chapter_uid: i64,
    chapter: &ChapterInfo,
) -> Result<Option<ExtractedPackage>, DownloadError> {
    let dir = package_dir(packages_dir, chapter_uid);
    if !dir.exists() {
        return Ok(None);
    }
    let state = match read_cache_state(&dir) {
        Ok(state) => state,
        Err(_) => {
            discard_cache(&dir);
            return Ok(None);
        }
    };
    if state.schema != CACHE_SCHEMA_VERSION
        || state.book_id != book_id
        || state.book_version != book_version
        || state.chapter_uid != chapter_uid
    {
        discard_cache(&dir);
        return Ok(None);
    }
    let files = match read_tree(&dir) {
        Ok(files) => files,
        Err(_) => {
            discard_cache(&dir);
            return Ok(None);
        }
    };
    let info = match find_info_txt(&files) {
        Some((_, bytes)) => match crate::weread::model::ChapterPackageManifest::from_json(bytes) {
            Ok(info) => info,
            Err(_) => {
                discard_cache(&dir);
                return Ok(None);
            }
        },
        None => {
            discard_cache(&dir);
            return Ok(None);
        }
    };
    let package = ExtractedPackage { info, files };
    if select_chapter_files(&package, chapter).is_err() {
        discard_cache(&dir);
        return Ok(None);
    }
    Ok(Some(package))
}

fn write_package_cache(
    packages_dir: &Path,
    book_id: &str,
    book_version: &str,
    chapter_uid: i64,
    package: &ExtractedPackage,
    skip_shared_assets: bool,
) -> Result<(), DownloadError> {
    let final_dir = package_dir(packages_dir, chapter_uid);
    let temp = unique_temp_dir(packages_dir, &format!("pkg-{chapter_uid}"))?;
    let result = (|| -> Result<(), DownloadError> {
        if skip_shared_assets {
            let files: BTreeMap<String, Vec<u8>> = package
                .files
                .iter()
                .filter(|(path, _)| !is_asset_path(path))
                .map(|(path, bytes)| (path.clone(), bytes.clone()))
                .collect();
            write_tree(&temp, &files)?;
        } else {
            write_tree(&temp, &package.files)?;
        }
        write_cache_state(
            &temp,
            &CacheState {
                schema: CACHE_SCHEMA_VERSION,
                book_id: book_id.to_string(),
                book_version: book_version.to_string(),
                chapter_uid,
            },
        )?;
        remove_path_if_exists(&final_dir)?;
        fs::rename(&temp, &final_dir).map_err(|error| storage_path(&final_dir, error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    result
}

fn public_cache_valid(work_dir: &Path, book_id: &str, book_version: &str) -> bool {
    let dir = work_dir.join("public");
    let Ok(state) = read_cache_state(&dir) else {
        return false;
    };
    state.schema == CACHE_SCHEMA_VERSION
        && state.book_id == book_id
        && state.book_version == book_version
        && state.chapter_uid == 0
}

fn merge_public_cache(
    work_dir: &Path,
    book_id: &str,
    book_version: &str,
    package: &ExtractedPackage,
) -> Result<(), DownloadError> {
    let final_dir = work_dir.join("public");
    let assets: BTreeMap<String, Vec<u8>> = package
        .files
        .iter()
        .filter(|(path, _)| is_asset_path(path))
        .map(|(path, bytes)| (path.clone(), bytes.clone()))
        .collect();
    if public_cache_valid(work_dir, book_id, book_version) {
        if assets.is_empty() {
            return Ok(());
        }
        // Incrementally add any new per-chapter assets from batch
        // 2..N into the existing valid `public/` cache without wiping batch 1.
        let mut to_write = BTreeMap::new();
        for (name, bytes) in assets {
            let normalized = normalize_archive_path(&name)?;
            let target = final_dir.join(encode_cache_rel_path(&normalized));
            if !target.exists() {
                to_write.insert(normalized, bytes);
            }
        }
        if !to_write.is_empty() {
            write_tree(&final_dir, &to_write)?;
        }
        return Ok(());
    }
    let temp = unique_temp_dir(work_dir, "public")?;
    let result = (|| -> Result<(), DownloadError> {
        write_tree(&temp, &assets)?;
        write_cache_state(
            &temp,
            &CacheState {
                schema: CACHE_SCHEMA_VERSION,
                book_id: book_id.to_string(),
                book_version: book_version.to_string(),
                chapter_uid: 0,
            },
        )?;
        remove_path_if_exists(&final_dir)?;
        fs::rename(&temp, &final_dir).map_err(|error| storage_path(&final_dir, error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    result
}

#[cfg(test)]
fn merge_assets(
    work_dir: &Path,
    package_dirs: &[PathBuf],
) -> Result<BTreeMap<String, Vec<u8>>, DownloadError> {
    let mut directories = Vec::with_capacity(package_dirs.len() + 1);
    directories.push(work_dir.join("public"));
    directories.extend(package_dirs.iter().cloned());
    let mut merged = BTreeMap::new();
    for directory in directories {
        if !directory.exists() {
            continue;
        }
        for (path, bytes) in read_tree(&directory)? {
            if !is_asset_path(&path) {
                continue;
            }
            // Keep the first copy when multiple chapters ship slightly
            // differing shared assets (real WeRead packages occasionally do).
            merged.entry(path).or_insert(bytes);
        }
    }
    Ok(merged)
}

fn read_cache_state(dir: &Path) -> Result<CacheState, DownloadError> {
    let path = dir.join("state.json");
    let bytes = fs::read(&path).map_err(|error| storage_path(&path, error))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| DownloadError::Storage(format!("{}: {error}", path.display())))
}

fn write_cache_state(dir: &Path, state: &CacheState) -> Result<(), DownloadError> {
    let path = dir.join("state.json");
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| DownloadError::Storage(format!("{}: {error}", path.display())))?;
    let mut file = File::create(&path).map_err(|error| storage_path(&path, error))?;
    file.write_all(&bytes)
        .map_err(|error| storage_path(&path, error))
}

fn read_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, DownloadError> {
    let mut files = BTreeMap::new();
    collect_tree(root, root, &mut files)?;
    Ok(files)
}

fn collect_tree(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), DownloadError> {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .map_err(|error| storage_path(directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| storage_path(directory, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| storage_path(&path, error))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_tree(root, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| DownloadError::Storage(error.to_string()))?;
        let raw_name = relative
            .to_str()
            .ok_or_else(|| {
                DownloadError::Storage(format!("non-UTF-8 cache path {}", path.display()))
            })?
            .replace('\\', "/");
        if raw_name == "state.json" {
            continue;
        }
        let name = decode_cache_rel_path(&raw_name);
        let bytes = fs::read(&path).map_err(|error| storage_path(&path, error))?;
        files.insert(name, bytes);
    }
    Ok(())
}

fn write_tree(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), DownloadError> {
    for (name, bytes) in files {
        let name = normalize_archive_path(name)?;
        let encoded = encode_cache_rel_path(&name);
        let path = root.join(encoded);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| storage_path(parent, error))?;
        }
        let mut file = File::create(&path).map_err(|error| storage_path(&path, error))?;
        file.write_all(bytes)
            .map_err(|error| storage_path(&path, error))?;
    }
    Ok(())
}

/// Percent-encode characters illegal on FAT32/Windows (`:`, `*`, `?`, `"`, `<`, `>`, `|`, `%`)
/// so obfuscated WeRead archive paths (like `Text/:****:.xhtml`) can always be cached safely.
fn encode_cache_rel_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if matches!(byte, b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|' | b'%') {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        } else {
            out.push(byte as char);
        }
    }
    out
}

fn decode_cache_rel_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| path.to_string())
}

fn prepare_work_dir(work_dir: &Path) -> Result<(), DownloadError> {
    fs::create_dir_all(work_dir).map_err(|error| storage_path(work_dir, error))
}

fn cleanup_stale_temp_dirs(work_dir: &Path) {
    sweep_temp_dirs(work_dir);
    // Package caches are staged directly under `packages/`, one level below the
    // top-level public-cache staging directories.
    sweep_temp_dirs(&work_dir.join("packages"));
}

fn sweep_temp_dirs(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(".tmp-") {
            let _ = remove_path_if_exists(&entry.path());
        }
    }
}

fn unique_temp_dir(parent: &Path, label: &str) -> Result<PathBuf, DownloadError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = parent.join(format!(
        ".tmp-{label}-{}-{nanos}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&path).map_err(|error| storage_path(&path, error))?;
    Ok(path)
}

fn discard_cache(directory: &Path) {
    let _ = remove_path_if_exists(directory);
}

fn remove_path_if_exists(path: &Path) -> Result<(), DownloadError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).map_err(|error| storage_path(path, error))
        }
        Ok(_) => fs::remove_file(path).map_err(|error| storage_path(path, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_path(path, error)),
    }
}

fn cleanup_stale_part(path: &Path) -> Result<(), DownloadError> {
    remove_path_if_exists(&append_suffix(path, ".part"))
}

fn write_epub_atomic(path: &Path, built: &BuiltEpub) -> Result<(), DownloadError> {
    let part = write_part(path, &built.bytes).map_err(|error| storage_path(path, error))?;
    let result = (|| -> Result<(), DownloadError> {
        let mut file = File::open(&part).map_err(|error| storage_path(&part, error))?;
        let actual = partial_md5(&mut file)
            .map_err(|error| DownloadError::Storage(format!("{}: {error}", part.display())))?;
        if actual != built.hash {
            return Err(DownloadError::HashMismatch {
                expected: built.hash.clone(),
                actual,
            });
        }
        commit_part(path, &part).map_err(|error| storage_path(path, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let part = write_part(path, bytes)?;
    if let Err(error) = commit_part(path, &part) {
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    Ok(())
}

fn write_part(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let part = append_suffix(path, ".part");
    match fs::symlink_metadata(&part) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&part)?,
        Ok(_) => fs::remove_file(&part)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let write = (|| -> io::Result<()> {
        let mut file = File::create(&part)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    Ok(part)
}

fn commit_part(path: &Path, part: &Path) -> io::Result<()> {
    let backup = append_suffix(path, ".bak");
    if path.exists() {
        match fs::remove_file(&backup) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(part, path) {
        if backup.exists() && !path.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(error);
    }
    sync_parent_dir(path);
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn storage_path(path: &Path, error: io::Error) -> DownloadError {
    DownloadError::Storage(format!("{}: {error}", path.display()))
}

fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};
    use std::sync::{Arc, Mutex};

    use base64::Engine as _;
    use roxmltree::Document;
    use zip::{CompressionMethod, ZipArchive};

    use crate::sync::transport::{HttpRequest, HttpResponse, TransportError};
    use crate::weread::auth::Credentials;
    use crate::weread::epub::{BuiltEpub, EPUB_MIMETYPE};
    use crate::weread::mock::make_envelope;
    use crate::weread::WeReadError;

    const FIXTURE_B64: &str =
        include_str!("../../../tests/fixtures/weread/chapter-package.zip.b64");
    const FIXTURE_PASSWORD: &str = "synthetic-zip-pass";
    const FIXTURE_VID: &str = "1234567890";
    const FIXTURE_BOOK_ID: &str = "100001";
    const FIXTURE_VERSION: &str = "42";

    #[derive(Default)]
    struct MockState {
        fail: BTreeSet<i64>,
        requests: Vec<String>,
    }

    #[derive(Clone)]
    struct ChapterMock {
        state: Arc<Mutex<MockState>>,
        zip: Vec<u8>,
        password: String,
        vid: String,
    }

    impl ChapterMock {
        fn new() -> Self {
            let compact: String = FIXTURE_B64.chars().filter(|c| !c.is_whitespace()).collect();
            let zip = base64::engine::general_purpose::STANDARD
                .decode(compact)
                .expect("fixture base64");
            Self {
                state: Arc::new(Mutex::new(MockState::default())),
                zip,
                password: FIXTURE_PASSWORD.to_string(),
                vid: FIXTURE_VID.to_string(),
            }
        }

        fn fail_chapter(&self, chapter_uid: i64) {
            self.state.lock().unwrap().fail.insert(chapter_uid);
        }

        fn clear_failures(&self) {
            self.state.lock().unwrap().fail.clear();
        }

        fn request_count(&self) -> usize {
            self.state.lock().unwrap().requests.len()
        }

        fn count_requests_for(&self, chapter_uid: i64) -> usize {
            self.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|url| parse_chapters(url).contains(&chapter_uid))
                .count()
        }

        fn chapter_sets(&self) -> Vec<Vec<i64>> {
            self.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .map(|url| parse_chapters(url))
                .collect()
        }
    }

    impl HttpTransport for ChapterMock {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
            self.state
                .lock()
                .unwrap()
                .requests
                .push(request.url.clone());
            if !request.url.contains("/book/chapterdownload") {
                return Ok(HttpResponse::new(
                    404,
                    br#"{"errcode":-1,"errmsg":"unexpected endpoint"}"#.to_vec(),
                ));
            }
            let chapters = parse_chapters(&request.url);
            let fail = self.state.lock().unwrap().fail.clone();
            if chapters.iter().any(|uid| fail.contains(uid)) {
                // 401 + §14.7 terminal -2011 so the client's read-refresh path
                // cannot hide the injected failure.
                return Ok(HttpResponse::new(
                    401,
                    br#"{"errcode":-2011,"errmsg":"synthetic terminal 401"}"#.to_vec(),
                ));
            }
            Ok(HttpResponse::new(200, self.zip.clone())
                .with_header("encryptkey", make_envelope(&self.vid, &self.password)))
        }
    }

    fn parse_chapters(url: &str) -> Vec<i64> {
        let Some((_, query)) = url.split_once('?') else {
            return Vec::new();
        };
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("chapters=") {
                return value
                    .split(',')
                    .filter_map(|part| part.parse().ok())
                    .collect();
            }
        }
        Vec::new()
    }

    fn test_client(mock: &ChapterMock) -> WeReadClient<ChapterMock> {
        WeReadClient::new(
            mock.clone(),
            Credentials::new(
                FIXTURE_VID,
                "synthetic-access-token",
                "synthetic-refresh-token",
                "synthetic-device",
            ),
        )
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = format!(
                "inkread-wr003-{label}-{}-{}",
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).expect("temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn file(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn chapter(
        chapter_uid: i64,
        chapter_idx: i64,
        title: &str,
        price: f64,
        paid: i64,
        files: &[&str],
    ) -> ChapterInfo {
        ChapterInfo {
            chapter_uid: Some(chapter_uid),
            chapter_idx: Some(chapter_idx),
            title: Some(title.to_string()),
            word_count: Some(100),
            price: Some(price),
            paid: Some(paid),
            tar: None,
            files: files.iter().map(|file| file.to_string()).collect(),
        }
    }

    fn toc() -> ChapterInfos {
        ChapterInfos {
            book_id: FIXTURE_BOOK_ID.to_string(),
            synckey: Some(7),
            book_version: None,
            chapters: vec![
                chapter(0, 0, "Public resources", 0.0, 0, &["Text/0.xhtml"]),
                chapter(2, 2, "Chapter Two", 0.0, 0, &["Text/TOC-WRONG-2.xhtml"]),
                chapter(1, 1, "Chapter One", 0.0, 0, &["Text/TOC-WRONG-1.xhtml"]),
                chapter(8, 8, "Paid chapter", 100.0, 0, &["Text/8.xhtml"]),
                chapter(9, 9, "Purchased chapter", 100.0, 1, &["Text/9.xhtml"]),
            ],
        }
    }

    fn download_request(
        temp: &TempDir,
        format: SourceFormat,
        chapters: Vec<i64>,
    ) -> DownloadRequest {
        DownloadRequest {
            book_id: FIXTURE_BOOK_ID.to_string(),
            title: "Synthetic Three Chapters".to_string(),
            format,
            book_version: FIXTURE_VERSION.to_string(),
            chapters,
            epub_path: temp.file("book.epub"),
            mapping_path: temp.file("weread-books.json"),
            work_dir: temp.file("work"),
        }
    }

    fn read_entries(path: &Path) -> Vec<(String, CompressionMethod, Vec<u8>)> {
        let bytes = fs::read(path).expect("epub file");
        let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let mut entries = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).expect("entry");
            let name = entry.name().to_string();
            let method = entry.compression();
            let mut content = Vec::new();
            entry.read_to_end(&mut content).expect("entry read");
            entries.push((name, method, content));
        }
        entries
    }

    fn entry<'a>(entries: &'a [(String, CompressionMethod, Vec<u8>)], name: &str) -> &'a [u8] {
        entries
            .iter()
            .find(|(entry_name, _, _)| entry_name == name)
            .map(|(_, _, bytes)| bytes.as_slice())
            .unwrap_or_else(|| panic!("missing ZIP entry {name}"))
    }

    fn no_temp_leftovers(work_dir: &Path) -> bool {
        fn clean(parent: &Path) -> bool {
            fs::read_dir(parent)
                .map(|entries| {
                    entries
                        .flatten()
                        .all(|entry| !entry.file_name().to_string_lossy().starts_with(".tmp-"))
                })
                .unwrap_or(true)
        }
        clean(work_dir) && clean(&work_dir.join("packages"))
    }

    #[test]
    fn source_format_parses_shelf_values_explicitly() {
        assert_eq!(SourceFormat::parse(" EPUB ").unwrap(), SourceFormat::Epub);
        assert_eq!(SourceFormat::parse("txt").unwrap(), SourceFormat::Txt);
        assert_eq!(SourceFormat::parse("PDF").unwrap(), SourceFormat::Pdf);
        assert_eq!(SourceFormat::Epub.as_str(), "epub");
        assert!(matches!(
            SourceFormat::parse("mobi"),
            Err(DownloadError::UnsupportedFormat(value)) if value == "mobi"
        ));
        assert!(matches!(
            SourceFormat::from_shelf(None),
            Err(DownloadError::MissingFormat)
        ));
    }

    #[test]
    fn plan_filters_ineligible_before_any_request_and_sorts_by_idx() {
        let plan = plan_download(&toc(), &[2, 1, 8]).unwrap();
        assert_eq!(
            plan.eligible
                .iter()
                .map(|chapter| chapter.chapter_uid.unwrap())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].chapter_uid, 8);
        assert_eq!(plan.skipped[0].title, "Paid chapter");
        assert_eq!(plan.skipped[0].price, 100.0);
        assert_eq!(plan.skipped[0].paid, 0);

        assert!(matches!(
            plan_download(&toc(), &[99]),
            Err(DownloadError::UnknownChapter(99))
        ));
        assert!(matches!(
            plan_download(&toc(), &[]),
            Err(DownloadError::NoTargetChapters)
        ));
        assert!(matches!(
            plan_download(&toc(), &[8]),
            Err(DownloadError::NoEligibleChapters { skipped }) if skipped.len() == 1
        ));
        assert!(matches!(
            plan_download(&toc(), &[0]),
            Err(DownloadError::NoEligibleChapters { skipped }) if skipped.is_empty()
        ));
    }

    #[test]
    fn plan_rejects_a_chapter_without_chapter_idx() {
        let mut toc = toc();
        toc.chapters.push(ChapterInfo {
            chapter_uid: Some(3),
            chapter_idx: None,
            title: Some("No index".to_string()),
            price: Some(0.0),
            paid: Some(0),
            ..ChapterInfo::default()
        });
        assert!(matches!(
            plan_download(&toc, &[3]),
            Err(DownloadError::MissingChapterIdx(3))
        ));
    }

    #[test]
    fn pipeline_rebuilds_epub_in_toc_order_and_uses_info_txt_paths() {
        let temp = TempDir::new("pipeline");
        let mock = ChapterMock::new();
        let mut client = test_client(&mock);
        let request = download_request(&temp, SourceFormat::Epub, vec![2, 1]);
        let outcome = download_book(&mut client, &toc(), &request).expect("pipeline");

        assert_eq!(outcome.downloaded, [1, 2]);
        assert!(outcome.cached.is_empty());
        assert_eq!(mock.request_count(), 2);
        // Chapter 0 (shared Images/Styles) is fetched once with the first
        // chapter and cached under `public/`; later chapters omit `0`.
        assert_eq!(mock.chapter_sets(), [vec![0, 1], vec![2]]);
        assert_eq!(
            outcome
                .spine
                .iter()
                .map(|entry| entry.chapter_uid)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(outcome.spine[0].href, "Text/ch1.xhtml");
        assert_eq!(outcome.spine[1].href, "Text/ch2.xhtml");
        assert_eq!(outcome.skipped.len(), 0);

        let entries = read_entries(&outcome.epub_path);
        assert_eq!(entries[0].0, "mimetype");
        assert_eq!(entries[0].1, CompressionMethod::Stored);
        assert_eq!(entries[0].2, EPUB_MIMETYPE.as_bytes());

        let names: Vec<&str> = entries.iter().map(|(name, _, _)| name.as_str()).collect();
        assert!(!names.contains(&"OEBPS/Text/0.xhtml"));
        assert!(!names.contains(&"OEBPS/Text/TOC-WRONG-1.xhtml"));
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "OEBPS/Styles/shared.css")
                .count(),
            1
        );
        assert!(names.contains(&"OEBPS/Images/cover.png"));
        assert!(names.contains(&"OEBPS/Images/inline.png"));
        assert!(names.contains(&"OEBPS/Styles/ch2.css"));
        assert!(names.contains(&"OEBPS/toc.ncx"));
        assert!(names.contains(&"META-INF/container.xml"));

        let opf = std::str::from_utf8(entry(&entries, "OEBPS/content.opf")).unwrap();
        let document = Document::parse(opf).expect("OPF well-formed");
        let mut manifest = BTreeMap::new();
        for item in document
            .descendants()
            .filter(|node| node.has_tag_name("item"))
        {
            manifest.insert(
                item.attribute("id").unwrap(),
                item.attribute("href").unwrap(),
            );
        }
        let spine: Vec<&str> = document
            .descendants()
            .filter(|node| node.has_tag_name("itemref"))
            .map(|node| manifest[node.attribute("idref").unwrap()])
            .collect();
        assert_eq!(spine, ["Text/ch1.xhtml", "Text/ch2.xhtml"]);

        for chapter in ["OEBPS/Text/ch1.xhtml", "OEBPS/Text/ch2.xhtml"] {
            let text = std::str::from_utf8(entry(&entries, chapter)).unwrap();
            Document::parse(text).expect("chapter well-formed");
        }

        let mut file = File::open(&outcome.epub_path).expect("epub");
        assert_eq!(partial_md5(&mut file).unwrap(), outcome.hash);
        assert!(!append_suffix(&outcome.epub_path, ".part").exists());
        assert!(no_temp_leftovers(&request.work_dir));

        let mapping: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&request.mapping_path).unwrap()).unwrap();
        assert_eq!(mapping["version"], 1);
        assert_eq!(mapping["books"][FIXTURE_BOOK_ID]["hash"], outcome.hash);
        assert_eq!(mapping["books"][FIXTURE_BOOK_ID]["format"], "epub");
        assert_eq!(mapping["books"][FIXTURE_BOOK_ID]["title"], request.title);
        assert_eq!(mapping["books"][FIXTURE_BOOK_ID]["synckey"], 7);
    }

    #[test]
    fn resume_skips_completed_chapters_and_cleans_part() {
        let temp = TempDir::new("resume");
        let mock = ChapterMock::new();
        mock.fail_chapter(2);
        let mut client = test_client(&mock);
        let request = download_request(&temp, SourceFormat::Epub, vec![1, 2]);

        fs::write(append_suffix(&request.epub_path, ".part"), b"stale").unwrap();
        let error = download_book(&mut client, &toc(), &request).unwrap_err();
        assert!(
            matches!(
                error,
                DownloadError::WeRead(WeReadError::ManualVerification(-2011))
            ),
            "unexpected error: {error:?}"
        );
        assert!(!request.epub_path.exists());
        assert!(!append_suffix(&request.epub_path, ".part").exists());
        assert_eq!(mock.count_requests_for(1), 1);
        assert_eq!(mock.count_requests_for(2), 1);
        assert!(request.work_dir.join("packages/1").exists());
        assert!(!request.work_dir.join("packages/2").exists());
        assert!(no_temp_leftovers(&request.work_dir));

        mock.clear_failures();
        let outcome = download_book(&mut client, &toc(), &request).expect("retry");
        assert_eq!(outcome.cached, [1]);
        assert_eq!(outcome.downloaded, [2]);
        assert_eq!(mock.count_requests_for(1), 1);
        assert_eq!(mock.count_requests_for(2), 2);
        assert!(request.epub_path.exists());
        assert!(!append_suffix(&request.epub_path, ".part").exists());
        assert!(no_temp_leftovers(&request.work_dir));
    }

    #[test]
    fn stale_package_temp_dirs_are_swept_on_success_and_failure() {
        let temp = TempDir::new("temp-sweep");
        let mock = ChapterMock::new();
        mock.fail_chapter(2);
        let mut client = test_client(&mock);
        let request = download_request(&temp, SourceFormat::Epub, vec![1, 2]);
        let packages_dir = request.work_dir.join("packages");

        let stale_package = packages_dir.join(".tmp-pkg-1-stale");
        fs::create_dir_all(stale_package.join("Text")).unwrap();
        fs::write(stale_package.join("Text/ch1.xhtml"), b"stale").unwrap();
        fs::write(request.work_dir.join(".tmp-public-stale"), b"stale").unwrap();

        let error = download_book(&mut client, &toc(), &request).unwrap_err();
        assert!(
            matches!(
                error,
                DownloadError::WeRead(WeReadError::ManualVerification(-2011))
            ),
            "unexpected error: {error:?}"
        );
        assert!(!stale_package.exists());
        assert!(!request.work_dir.join(".tmp-public-stale").exists());
        assert!(packages_dir.join("1").exists());
        assert!(no_temp_leftovers(&request.work_dir));

        mock.clear_failures();
        let stale_after_success = packages_dir.join(".tmp-pkg-2-stale");
        fs::create_dir_all(&stale_after_success).unwrap();
        download_book(&mut client, &toc(), &request).expect("retry");
        assert!(!stale_after_success.exists());
        assert!(no_temp_leftovers(&request.work_dir));
    }

    #[test]
    fn ineligible_chapter_is_reported_without_any_http_request() {
        let temp = TempDir::new("ineligible");
        let mock = ChapterMock::new();
        let mut client = test_client(&mock);
        let request = download_request(&temp, SourceFormat::Epub, vec![8]);
        let error = download_book(&mut client, &toc(), &request).unwrap_err();
        match error {
            DownloadError::NoEligibleChapters { skipped } => {
                assert_eq!(skipped.len(), 1);
                assert_eq!(skipped[0].chapter_uid, 8);
                assert_eq!(skipped[0].chapter_idx, Some(8));
                assert_eq!(skipped[0].title, "Paid chapter");
                assert_eq!(skipped[0].price, 100.0);
                assert_eq!(skipped[0].paid, 0);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(mock.request_count(), 0);
        assert!(!temp.file("weread-books.json").exists());
        assert!(!temp.file("book.epub").exists());
    }

    #[test]
    fn mixed_selection_downloads_only_eligible_chapters() {
        let temp = TempDir::new("mixed");
        let mock = ChapterMock::new();
        let mut client = test_client(&mock);
        let request = download_request(&temp, SourceFormat::Epub, vec![1, 8]);
        let outcome = download_book(&mut client, &toc(), &request).expect("pipeline");
        assert_eq!(outcome.downloaded, [1]);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].chapter_uid, 8);
        assert_eq!(mock.request_count(), 1);
        assert_eq!(mock.chapter_sets(), [vec![0, 1]]);
        assert_eq!(outcome.spine.len(), 1);
    }

    #[test]
    fn txt_and_pdf_are_rejected_without_http() {
        let temp = TempDir::new("formats");
        let mock = ChapterMock::new();
        let mut client = test_client(&mock);
        for (format, expected) in [(SourceFormat::Txt, "txt"), (SourceFormat::Pdf, "pdf")] {
            let request = download_request(&temp, format, vec![1]);
            let error = download_book(&mut client, &toc(), &request).unwrap_err();
            assert!(
                matches!(
                    error,
                    DownloadError::UnsupportedFormat(ref found) if found == expected
                ),
                "unexpected error: {error:?}"
            );
        }
        assert_eq!(mock.request_count(), 0);
    }

    #[test]
    fn chapter_selection_falls_back_to_the_uid_stem() {
        let mut files = BTreeMap::new();
        files.insert("Text/0.xhtml".to_string(), b"<html/>".to_vec());
        files.insert("Text/1.xhtml".to_string(), b"<html/>".to_vec());
        let package = ExtractedPackage {
            info: crate::weread::model::ChapterPackageManifest::default(),
            files,
        };
        let chapter = chapter(1, 1, "One", 0.0, 0, &[]);
        assert_eq!(
            select_chapter_files(&package, &chapter).unwrap(),
            ["Text/1.xhtml"]
        );
    }

    #[test]
    fn cache_is_keyed_by_book_version() {
        let temp = TempDir::new("cache-version");
        let mock = ChapterMock::new();
        let mut client = test_client(&mock);
        let request = download_request(&temp, SourceFormat::Epub, vec![1]);
        download_book(&mut client, &toc(), &request).expect("first run");
        assert_eq!(mock.count_requests_for(1), 1);

        let state_path = request.work_dir.join("packages/1/state.json");
        let mut state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        state["bookVersion"] = serde_json::Value::String("41".to_string());
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();

        download_book(&mut client, &toc(), &request).expect("second run");
        assert_eq!(mock.count_requests_for(1), 2);
    }

    #[test]
    fn conflicting_asset_bytes_keep_first_copy_without_aborting() {
        let temp = TempDir::new("assets");
        let first = temp.file("packages/1");
        let second = temp.file("packages/2");
        fs::create_dir_all(first.join("Images")).unwrap();
        fs::create_dir_all(second.join("Images")).unwrap();
        fs::write(first.join("Images/x.png"), b"one").unwrap();
        fs::write(second.join("Images/x.png"), b"two").unwrap();
        let merged = merge_assets(temp.path(), &[first, second]).expect("keeps first asset");
        assert_eq!(
            merged.get("Images/x.png").map(Vec::as_slice),
            Some(&b"one"[..])
        );
    }

    #[test]
    fn epub_write_hash_mismatch_removes_part() {
        let temp = TempDir::new("hash");
        let path = temp.file("book.epub");
        let built = BuiltEpub {
            bytes: b"not really an epub".to_vec(),
            hash: "0".repeat(32),
            spine: Vec::new(),
        };
        let error = write_epub_atomic(&path, &built).unwrap_err();
        assert!(matches!(error, DownloadError::HashMismatch { .. }));
        assert!(!path.exists());
        assert!(!append_suffix(&path, ".part").exists());
    }

    #[test]
    fn mapping_roundtrip_uses_versioned_schema_and_atomic_write() {
        let temp = TempDir::new("mapping");
        let path = temp.file("weread-books.json");
        let mut store = BookMappingStore::load(&path).unwrap();
        assert!(store.is_empty());
        store.insert(
            FIXTURE_BOOK_ID,
            BookMapping {
                hash: "a".repeat(32),
                title: "Synthetic Book".to_string(),
                format: "epub".to_string(),
                synckey: Some(7),
            },
        );
        store.save().unwrap();
        assert!(!append_suffix(&path, ".part").exists());

        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["version"], BOOK_MAPPING_SCHEMA_VERSION);
        assert_eq!(json["books"][FIXTURE_BOOK_ID]["format"], "epub");
        assert_eq!(json["books"][FIXTURE_BOOK_ID]["synckey"], 7);

        let loaded = BookMappingStore::load(&path).unwrap();
        assert_eq!(loaded.get(FIXTURE_BOOK_ID).unwrap().hash, "a".repeat(32));
    }

    #[test]
    fn mapping_recovers_from_bak_after_a_torn_write() {
        let temp = TempDir::new("mapping-bak");
        let path = temp.file("weread-books.json");
        let mut store = BookMappingStore::load(&path).unwrap();
        store.insert(
            FIXTURE_BOOK_ID,
            BookMapping {
                hash: "a".repeat(32),
                title: "First".to_string(),
                format: "epub".to_string(),
                synckey: Some(7),
            },
        );
        store.save().unwrap();

        let mut store = BookMappingStore::load(&path).unwrap();
        store.insert(
            FIXTURE_BOOK_ID,
            BookMapping {
                hash: "b".repeat(32),
                title: "Second".to_string(),
                format: "epub".to_string(),
                synckey: Some(8),
            },
        );
        store.save().unwrap();
        assert!(append_suffix(&path, ".bak").exists());

        fs::write(&path, b"{\"version\":1,\"books\":{").unwrap();
        let recovered = BookMappingStore::load(&path).unwrap();
        assert_eq!(recovered.get(FIXTURE_BOOK_ID).unwrap().hash, "a".repeat(32));
        assert_eq!(recovered.get(FIXTURE_BOOK_ID).unwrap().title, "First");

        fs::remove_file(&path).unwrap();
        let recovered = BookMappingStore::load(&path).unwrap();
        assert_eq!(recovered.get(FIXTURE_BOOK_ID).unwrap().title, "First");
    }

    #[test]
    fn mapping_rejects_an_unknown_schema_version() {
        let temp = TempDir::new("mapping-version");
        let path = temp.file("weread-books.json");
        fs::write(&path, br#"{"version":99,"books":{}}"#).unwrap();
        let error = BookMappingStore::load(&path).unwrap_err();
        assert!(matches!(
            error,
            MappingError::UnsupportedVersion {
                found: 99,
                expected: BOOK_MAPPING_SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn mapping_save_failure_leaves_no_part() {
        let temp = TempDir::new("mapping-fail");
        let path = temp.file("weread-books.json");
        let mut store = BookMappingStore::load(&path).unwrap();
        store.insert(
            FIXTURE_BOOK_ID,
            BookMapping {
                hash: "a".repeat(32),
                title: "First".to_string(),
                format: "epub".to_string(),
                synckey: None,
            },
        );
        store.save().unwrap();
        fs::create_dir(append_suffix(&path, ".bak")).unwrap();
        assert!(store.save().is_err());
        assert!(!append_suffix(&path, ".part").exists());
    }

    fn make_test_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn root_layout_and_htm_chapters_download_and_rebuild_epub() {
        let info = br#"{"chapters":[
            {"chapterUid":1,"files":["coverpage.html","missing_extra.html"]},
            {"chapterUid":2,"files":["xhtml/ch2.htm"]}
        ]}"#;
        let zip_bytes = make_test_zip(&[
            ("info.txt", info),
            (
                "coverpage.html",
                br#"<!DOCTYPE html><html><head><meta charset="utf-8"></head><body><img src="cover.jpg"><br><p>Cover</p></body></html>"#,
            ),
            (
                "xhtml/ch2.htm",
                br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Two</title></head><body><p>Body</p><img src="../images/pic.png"/></body></html>"#,
            ),
            ("stylesheet.css", b"body { margin: 0; }"),
            ("cover.jpg", b"JPG-BYTES"),
            ("images/pic.png", b"PNG-BYTES"),
            ("Fonts/book.ttf", b"TTF-BYTES"),
        ]);

        let mock = ChapterMock {
            state: Arc::new(Mutex::new(MockState::default())),
            zip: zip_bytes,
            password: FIXTURE_PASSWORD.to_string(),
            vid: FIXTURE_VID.to_string(),
        };
        let mut client = test_client(&mock);
        let temp = TempDir::new("root-layout");
        let toc = ChapterInfos {
            book_id: FIXTURE_BOOK_ID.to_string(),
            synckey: Some(11),
            book_version: Some(42),
            chapters: vec![
                chapter(1, 1, "Cover", 0.0, 0, &["coverpage.html"]),
                chapter(2, 2, "Chapter Two", 0.0, 0, &["xhtml/ch2.htm"]),
            ],
        };
        let request = download_request(&temp, SourceFormat::Epub, vec![1, 2]);
        let outcome = download_book(&mut client, &toc, &request).expect("root layout download");
        assert_eq!(outcome.downloaded, [1, 2]);
        assert_eq!(outcome.spine.len(), 2);
        assert_eq!(outcome.spine[0].href, "coverpage.html");
        assert_eq!(outcome.spine[1].href, "xhtml/ch2.htm");

        let entries = read_entries(&outcome.epub_path);
        let names: Vec<&str> = entries.iter().map(|(name, _, _)| name.as_str()).collect();
        assert!(names.contains(&"OEBPS/coverpage.html"));
        assert!(names.contains(&"OEBPS/xhtml/ch2.htm"));
        assert!(names.contains(&"OEBPS/stylesheet.css"));
        assert!(names.contains(&"OEBPS/cover.jpg"));
        assert!(names.contains(&"OEBPS/images/pic.png"));
        assert!(names.contains(&"OEBPS/Fonts/book.ttf"));

        // Second run must reuse the cache for both chapters and all root/nested assets.
        let cached_outcome = download_book(&mut client, &toc, &request).expect("cached run");
        assert_eq!(cached_outcome.cached, [1, 2]);
        assert!(cached_outcome.downloaded.is_empty());
        assert_eq!(mock.request_count(), 2);
    }

    #[test]
    fn adaptive_batching_scales_with_chapter_count() {
        assert_eq!(adaptive_batch_size(1), 1);
        assert_eq!(adaptive_batch_size(2), 1);
        assert_eq!(adaptive_batch_size(3), 3);
        assert_eq!(adaptive_batch_size(15), 3);
        assert_eq!(adaptive_batch_size(16), 5);
        assert_eq!(adaptive_batch_size(50), 5);
        assert_eq!(adaptive_batch_size(51), 10);
        assert_eq!(adaptive_batch_size(1330), 10);

        // End-to-end 55-chapter download must batch by 10 (6 HTTP requests total).
        let total = 55i64;
        let mut info_chapters = Vec::new();
        let mut zip_files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut toc_chapters = Vec::new();
        let mut targets = Vec::new();
        for uid in 1..=total {
            let href = format!("ch{uid}.html");
            info_chapters.push(serde_json::json!({
                "chapterUid": uid,
                "files": [href]
            }));
            zip_files.push((
                href.clone(),
                format!(
                    "<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><body><p>Ch {uid}</p></body></html>"
                )
                .into_bytes(),
            ));
            toc_chapters.push(chapter(
                uid,
                uid,
                &format!("Chapter {uid}"),
                0.0,
                0,
                &[&href],
            ));
            targets.push(uid);
        }
        let info_bytes =
            serde_json::to_vec(&serde_json::json!({ "chapters": info_chapters })).unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("info.txt", options).unwrap();
        writer.write_all(&info_bytes).unwrap();
        writer.start_file("stylesheet.css", options).unwrap();
        writer.write_all(b"body{}").unwrap();
        for (name, bytes) in &zip_files {
            writer.start_file(name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        let zip_bytes = writer.finish().unwrap().into_inner();

        let mock = ChapterMock {
            state: Arc::new(Mutex::new(MockState::default())),
            zip: zip_bytes,
            password: FIXTURE_PASSWORD.to_string(),
            vid: FIXTURE_VID.to_string(),
        };
        let mut client = test_client(&mock);
        let temp = TempDir::new("adaptive-batch");
        let toc = ChapterInfos {
            book_id: FIXTURE_BOOK_ID.to_string(),
            synckey: Some(12),
            book_version: Some(42),
            chapters: toc_chapters,
        };
        let request = download_request(&temp, SourceFormat::Epub, targets);
        let outcome = download_book(&mut client, &toc, &request).expect("55-chapter download");
        assert_eq!(outcome.downloaded.len(), 55);
        assert_eq!(mock.request_count(), 6);
        let sets = mock.chapter_sets();
        // First batch includes chapter 0 + 10 chapters; last batch has 5 chapters.
        assert_eq!(sets[0].len(), 11);
        assert_eq!(sets[0][0], 0);
        assert_eq!(sets[1].len(), 10);
        assert_eq!(sets[5].len(), 5);
    }

    #[test]
    fn single_chapter_retries_transient_errors_and_never_retries_fatal() {
        assert!(is_retryable_single_chapter_error(&WeReadError::Transport(
            "connection reset".to_string()
        )));
        assert!(is_retryable_single_chapter_error(&WeReadError::HttpStatus(
            503,
            "unavailable".to_string()
        )));
        assert!(!is_retryable_single_chapter_error(&WeReadError::Transport(
            "cancelled".to_string()
        )));
        assert!(!is_retryable_single_chapter_error(&WeReadError::Api {
            code: -2063,
            message: "version expired".to_string()
        }));
        assert!(!is_retryable_single_chapter_error(
            &WeReadError::RateLimited(-10102)
        ));
        assert!(!is_retryable_single_chapter_error(
            &WeReadError::ManualVerification(-2041)
        ));
        assert!(!is_retryable_single_chapter_error(
            &WeReadError::AuthExpired
        ));

        #[derive(Clone)]
        struct FlakyMock {
            attempts: Arc<Mutex<usize>>,
            zip: Vec<u8>,
        }
        impl HttpTransport for FlakyMock {
            fn send(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
                let mut count = self.attempts.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    return Err(TransportError::Network("packet dropped".to_string()));
                }
                if *count == 2 {
                    return Ok(HttpResponse::new(503, b"temporary outage".to_vec()));
                }
                Ok(HttpResponse::new(200, self.zip.clone())
                    .with_header("encryptkey", make_envelope(FIXTURE_VID, FIXTURE_PASSWORD)))
            }
        }

        let compact: String = FIXTURE_B64.chars().filter(|c| !c.is_whitespace()).collect();
        let zip = base64::engine::general_purpose::STANDARD
            .decode(compact)
            .expect("fixture base64");
        let attempts = Arc::new(Mutex::new(0));
        let mock = FlakyMock {
            attempts: attempts.clone(),
            zip,
        };
        let mut client = WeReadClient::new(
            mock,
            Credentials::new(
                FIXTURE_VID,
                "synthetic-access-token",
                "synthetic-refresh-token",
                "synthetic-device",
            ),
        );
        let temp = TempDir::new("flaky-retry");
        let request = download_request(&temp, SourceFormat::Epub, vec![1]);
        let outcome = download_book(&mut client, &toc(), &request).expect("recovers on 3rd try");
        assert_eq!(outcome.downloaded, [1]);
        assert_eq!(*attempts.lock().unwrap(), 3);
    }
}
