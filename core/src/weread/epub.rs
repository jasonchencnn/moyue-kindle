//! EPUB 2 rebuild from WeRead chapter packages (WR-003).
//!
//! A WeRead `chapterdownload` ZIP contains `Text/*.xhtml`, shared `Images/` and
//! `Styles/`, and an `info.txt` manifest. This module decrypts that ZipCrypto
//! archive, extracts the whitelisted entries, validates the XHTML/reference
//! graph, and emits a self-contained EPUB 2 archive with a `mimetype` first
//! stored entry, an OPF manifest/spine in TOC order, and an NCX table of contents.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{Cursor, Read, Write};

use roxmltree::Document;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::hash::partial_md5;
use crate::weread::model::ChapterPackageManifest;

/// EPUB media type; the first ZIP entry must be this string, stored.
pub const EPUB_MIMETYPE: &str = "application/epub+zip";

const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY_SIZE: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_SIZE: u64 = 256 * 1024 * 1024;

/// Files extracted from one WeRead chapter ZIP.
#[derive(Debug, Clone, Default)]
pub struct ExtractedPackage {
    /// Parsed `info.txt`; its `files[]` is the source of truth for chapter bodies.
    pub info: ChapterPackageManifest,
    /// Normalized package paths (e.g. `Text/...`, `coverpage.html`, `Images/...`, `Styles/...`, `info.txt`).
    pub files: BTreeMap<String, Vec<u8>>,
}

impl ExtractedPackage {
    /// The `files[]` list recorded for `chapter_uid` in `info.txt`, if present.
    pub fn chapter_files(&self, chapter_uid: i64) -> Option<&[String]> {
        self.info
            .chapters
            .iter()
            .find(|chapter| chapter.chapter_uid == Some(chapter_uid))
            .map(|chapter| chapter.files.as_slice())
            .filter(|files| !files.is_empty())
    }
}

/// One chapter entry in the rebuilt EPUB spine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpubChapter {
    /// WeRead `chapterUid` this entry came from.
    pub chapter_uid: i64,
    /// WeRead `chapterIdx`, used for TOC ordering.
    pub chapter_idx: i64,
    /// Chapter label written to the NCX.
    pub title: String,
    /// Package-relative XHTML paths (`Text/...`), in reading order.
    pub files: Vec<String>,
}

/// Inputs for [`build_epub`].
#[derive(Debug, Clone, Default)]
pub struct EpubSource {
    /// WeRead book id, written into the OPF identifier and NCX UID.
    pub book_id: String,
    /// Book title written into the OPF metadata.
    pub title: String,
    /// Chapters in spine order (already sorted by `chapter_idx`).
    pub chapters: Vec<EpubChapter>,
    /// All package `Text/...` files, keyed by package-relative path. Files listed
    /// in `chapters` form the spine; every other entry is still packed and listed
    /// in the OPF manifest, but never added to the spine.
    pub texts: BTreeMap<String, Vec<u8>>,
    /// Shared images/styles, keyed by package-relative path.
    pub assets: BTreeMap<String, Vec<u8>>,
}

/// A spine entry and its stable href inside the rebuilt EPUB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineEntry {
    /// WeRead `chapterUid`.
    pub chapter_uid: i64,
    /// WeRead `chapterIdx`.
    pub chapter_idx: i64,
    /// Chapter label.
    pub title: String,
    /// Href relative to the OPF (`Text/...`).
    pub href: String,
}

/// A complete rebuilt EPUB and its `partialMD5`.
#[derive(Debug, Clone)]
pub struct BuiltEpub {
    /// Final EPUB bytes.
    pub bytes: Vec<u8>,
    /// `partialMD5` of [`Self::bytes`], the value stored in the book mapping.
    pub hash: String,
    /// Spine mapping for the later XPointer/WR-004 coordinate work.
    pub spine: Vec<SpineEntry>,
}

/// Failures while decrypting a chapter ZIP or rebuilding the EPUB.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EpubError {
    /// The chapter ZIP could not be opened as a ZIP archive.
    #[error("chapter ZIP is invalid: {0}")]
    Zip(String),
    /// The ZIP password from the `encryptkey` envelope did not open the archive.
    #[error("chapter ZIP password is invalid")]
    WrongPassword,
    /// A ZIP entry name is absolute, contains `..`, or is not UTF-8.
    #[error("unsafe ZIP entry path: {0}")]
    UnsafePath(String),
    /// The chapter ZIP has no `info.txt`.
    #[error("chapter ZIP has no info.txt")]
    MissingInfoTxt,
    /// `info.txt` is not the expected JSON shape.
    #[error("info.txt is invalid: {0}")]
    InvalidInfoTxt(String),
    /// The archive contains two entries with the same normalized path.
    #[error("chapter ZIP contains duplicate entry {0}")]
    DuplicateEntry(String),
    /// An entry exceeded the per-entry limit or the whole archive exceeded the total limit.
    #[error("chapter ZIP exceeded the {limit}-byte safety limit")]
    PackageTooLarge { limit: u64 },
    /// The archive contains too many entries.
    #[error("chapter ZIP exceeded the {limit}-entry safety limit")]
    TooManyEntries { limit: usize },
    /// The rebuilt EPUB has no chapters.
    #[error("cannot build an EPUB without chapters")]
    NoChapters,
    /// The source book id is empty.
    #[error("cannot build an EPUB without a book id")]
    MissingBookId,
    /// A chapter has no selected XHTML file.
    #[error("chapter {chapter_uid} has no XHTML body")]
    MissingChapterText { chapter_uid: i64 },
    /// A selected chapter file is not present in the source map.
    #[error("chapter {chapter_uid} file {href} is missing")]
    MissingChapterFile { chapter_uid: i64, href: String },
    /// A chapter file path is invalid or reserved (e.g. `info.txt`, `META-INF/...`, `mimetype`).
    #[error("chapter file path {0} is not allowed")]
    InvalidTextPath(String),
    /// Two spine entries point at the same file.
    #[error("duplicate spine file: {0}")]
    DuplicateSpineFile(String),
    /// A chapter XHTML document is not well-formed XML.
    #[error("chapter XHTML {path} is not well-formed: {detail}")]
    MalformedXhtml { path: String, detail: String },
    /// A chapter XHTML references a file that is not in the package.
    #[error("chapter {chapter_uid} references missing file {href}")]
    MissingReference { chapter_uid: i64, href: String },
    /// A stylesheet references a file that is not in the package.
    #[error("stylesheet {path} references missing file {href}")]
    MissingStylesheetReference { path: String, href: String },
    /// Two chapter packages ship different bytes for the same asset path.
    #[error("asset {0} has conflicting contents across chapter packages")]
    ConflictingAsset(String),
    /// An asset path is invalid or reserved (e.g. `info.txt`, `META-INF/...`, `mimetype`).
    #[error("asset path {0} is not allowed")]
    InvalidAssetPath(String),
    /// A text asset is not UTF-8.
    #[error("text asset {path} is not UTF-8")]
    InvalidUtf8 { path: String },
    /// Hashing the rebuilt EPUB failed.
    #[error("partialMD5 failed: {0}")]
    Hash(String),
    /// Writing the EPUB archive failed.
    #[error("EPUB archive write failed: {0}")]
    Write(String),
}

/// Decrypt one WeRead chapter ZIP and extract its allow-listed entries.
///
/// Traditional ZipCrypto is the only encryption accepted (contract §14.3).
/// Container metadata (`META-INF/`, `mimetype`, `*.opf`, `*.ncx`, `*.plist`,
/// `__MACOSX/`) is ignored; `info.txt` and EPUB content/asset extensions are
/// extracted at any safe relative depth.
pub fn extract_chapter_package(zip: &[u8], password: &str) -> Result<ExtractedPackage, EpubError> {
    let mut archive =
        ZipArchive::new(Cursor::new(zip)).map_err(|error| EpubError::Zip(error.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(EpubError::TooManyEntries { limit: MAX_ENTRIES });
    }
    let mut total = 0u64;
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index_decrypt(index, password.as_bytes())
            .map_err(map_zip_error)?;
        if entry.is_dir() {
            continue;
        }
        let raw_name = entry
            .enclosed_name()
            .and_then(|path| path.to_str().map(ToString::to_string))
            .ok_or_else(|| EpubError::UnsafePath(entry.name().to_string()))?;
        let name = normalize_archive_path(&raw_name)?;
        if !is_package_entry(&name) {
            continue;
        }
        let size = entry.size();
        if size > MAX_ENTRY_SIZE {
            return Err(EpubError::PackageTooLarge {
                limit: MAX_ENTRY_SIZE,
            });
        }
        let mut bytes = Vec::with_capacity(size.min(MAX_ENTRY_SIZE) as usize);
        let mut limited = entry.take(MAX_ENTRY_SIZE + 1);
        limited
            .read_to_end(&mut bytes)
            .map_err(|error| EpubError::Zip(error.to_string()))?;
        if bytes.len() as u64 > MAX_ENTRY_SIZE {
            return Err(EpubError::PackageTooLarge {
                limit: MAX_ENTRY_SIZE,
            });
        }
        total = total
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= MAX_TOTAL_SIZE)
            .ok_or(EpubError::PackageTooLarge {
                limit: MAX_TOTAL_SIZE,
            })?;
        if let Some(existing) = files.get(&name) {
            // Multi-chapter WeRead ZIPs can bundle identical shared assets or
            // placeholder files more than once. Only reject colliding raw paths
            // (e.g. `Text/a.xhtml` vs `Text\a.xhtml`) or differing contents.
            if raw_name != name || existing != &bytes {
                return Err(EpubError::DuplicateEntry(name));
            }
            continue;
        }
        files.insert(name, bytes);
    }
    let (_, info_bytes) = find_info_txt(&files).ok_or(EpubError::MissingInfoTxt)?;
    let info = ChapterPackageManifest::from_json(info_bytes)
        .map_err(|error| EpubError::InvalidInfoTxt(error.to_string()))?;
    Ok(ExtractedPackage { info, files })
}

/// Rebuild an EPUB 2 archive from selected chapter files and shared assets.
///
/// The OPF manifest and spine follow `source.chapters`; the caller is expected
/// to sort chapters by `chapter_idx` first. Every XHTML document is parsed with
/// `roxmltree` and every file is packed under `OEBPS/<original-normalized-path>`
/// so relative `href`/`src`/CSS `url()` links remain valid regardless of folder
/// layout. Non-spine text entries are packed and manifest-listed (not
/// spine-listed) instead of being silently dropped.
pub fn build_epub(source: &EpubSource) -> Result<BuiltEpub, EpubError> {
    if source.book_id.trim().is_empty() {
        return Err(EpubError::MissingBookId);
    }
    if source.chapters.is_empty() {
        return Err(EpubError::NoChapters);
    }

    let mut texts = normalize_map(&source.texts, |path| {
        if is_safe_oebps_content_path(path) {
            Ok(())
        } else {
            Err(EpubError::InvalidTextPath(path.to_string()))
        }
    })?;
    let assets = normalize_map(&source.assets, |path| {
        if is_safe_oebps_content_path(path) {
            Ok(())
        } else {
            Err(EpubError::InvalidAssetPath(path.to_string()))
        }
    })?;
    let css_paths: Vec<String> = assets
        .keys()
        .filter(|path| path.to_ascii_lowercase().ends_with(".css"))
        .cloned()
        .collect();

    let mut pending = Vec::new();
    let mut seen_hrefs: BTreeSet<String> = BTreeSet::new();
    let mut chapter_hrefs: BTreeMap<i64, String> = BTreeMap::new();
    for chapter in &source.chapters {
        if chapter.files.is_empty() {
            return Err(EpubError::MissingChapterText {
                chapter_uid: chapter.chapter_uid,
            });
        }
        for href in &chapter.files {
            let href = normalize_archive_path(href)?;
            if !is_safe_oebps_content_path(&href) {
                return Err(EpubError::InvalidTextPath(href));
            }
            chapter_hrefs
                .entry(chapter.chapter_uid)
                .or_insert_with(|| href.clone());
            // Real WeRead multi-level TOCs sometimes map multiple chapterUids
            // (e.g. section anchors) to the same XHTML file. Deduplicate the
            // OPF spine while preserving every chapter entry in toc.ncx.
            if !seen_hrefs.insert(href.clone()) {
                continue;
            }
            let original = texts
                .remove(&href)
                .ok_or_else(|| EpubError::MissingChapterFile {
                    chapter_uid: chapter.chapter_uid,
                    href: href.clone(),
                })?;
            let normalized = normalize_chapter_xhtml(&href, &original, &css_paths, &chapter.title)?;
            let references = xhtml_references(&href, &normalized)?;
            texts.insert(href.clone(), normalized);
            pending.push((
                SpineEntry {
                    chapter_uid: chapter.chapter_uid,
                    chapter_idx: chapter.chapter_idx,
                    title: chapter.title.clone(),
                    href,
                },
                references,
            ));
        }
    }

    // WeRead frequently omits external `tar` images, publisher fonts, or stripped
    // stylesheets while leaving `<img src>` / `<link href>` references in the
    // chapter XHTML. crengine degrades missing media/CSS gracefully, so keep
    // valid XHTML instead of aborting the whole book on dangling references.
    for (path, bytes) in &assets {
        if path.to_ascii_lowercase().ends_with(".css") {
            let _css = std::str::from_utf8(bytes)
                .map_err(|_| EpubError::InvalidUtf8 { path: path.clone() })?;
        }
    }

    let spine_files: BTreeSet<String> = pending
        .iter()
        .map(|(entry, _)| entry.href.clone())
        .collect();
    let extra_texts: BTreeMap<String, Vec<u8>> = texts
        .iter()
        .filter(|(path, _)| {
            !spine_files.contains(path.as_str()) && !assets.contains_key(path.as_str())
        })
        .map(|(path, bytes)| (path.clone(), bytes.clone()))
        .collect();

    let container = container_xml();
    let opf = build_opf(source, &pending, &extra_texts, &assets)?;
    let ncx = build_ncx(source, &pending, &chapter_hrefs)?;

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    writer
        .start_file("mimetype", stored)
        .map_err(map_zip_error)?;
    writer
        .write_all(EPUB_MIMETYPE.as_bytes())
        .map_err(|error| EpubError::Write(error.to_string()))?;

    write_zip_entry(&mut writer, "META-INF/container.xml", container.as_bytes())?;
    write_zip_entry(&mut writer, "OEBPS/content.opf", opf.as_bytes())?;
    for (entry, _) in &pending {
        let bytes = texts
            .get(&entry.href)
            .ok_or_else(|| EpubError::MissingChapterFile {
                chapter_uid: entry.chapter_uid,
                href: entry.href.clone(),
            })?;
        write_zip_entry(
            &mut writer,
            &format!("OEBPS/{}", entry.href),
            bytes.as_slice(),
        )?;
    }
    for (path, bytes) in &extra_texts {
        write_zip_entry(&mut writer, &format!("OEBPS/{path}"), bytes.as_slice())?;
    }
    for (path, bytes) in &assets {
        write_zip_entry(&mut writer, &format!("OEBPS/{path}"), bytes.as_slice())?;
    }
    write_zip_entry(&mut writer, "OEBPS/toc.ncx", ncx.as_bytes())?;

    let cursor = writer
        .finish()
        .map_err(|error| EpubError::Write(error.to_string()))?;
    let bytes = cursor.into_inner();
    let hash = partial_md5(&mut Cursor::new(&bytes))
        .map_err(|error| EpubError::Hash(error.to_string()))?;
    Ok(BuiltEpub {
        bytes,
        hash,
        spine: pending.into_iter().map(|(entry, _)| entry).collect(),
    })
}

fn write_zip_entry<W: Write + std::io::Seek>(
    writer: &mut ZipWriter<W>,
    path: &str,
    bytes: &[u8],
) -> Result<(), EpubError> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    writer.start_file(path, options).map_err(map_zip_error)?;
    writer
        .write_all(bytes)
        .map_err(|error| EpubError::Write(error.to_string()))
}

fn normalize_map(
    source: &BTreeMap<String, Vec<u8>>,
    check: impl Fn(&str) -> Result<(), EpubError>,
) -> Result<BTreeMap<String, Vec<u8>>, EpubError> {
    let mut normalized = BTreeMap::new();
    for (path, bytes) in source {
        let path = normalize_archive_path(path)?;
        check(&path)?;
        if normalized.insert(path.clone(), bytes.clone()).is_some() {
            return Err(EpubError::UnsafePath(path));
        }
    }
    Ok(normalized)
}

fn build_opf(
    source: &EpubSource,
    pending: &[(SpineEntry, Vec<String>)],
    extra_texts: &BTreeMap<String, Vec<u8>>,
    assets: &BTreeMap<String, Vec<u8>>,
) -> Result<String, EpubError> {
    let mut manifest = String::new();
    manifest.push_str(
        "    <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n",
    );
    for (index, (entry, _)) in pending.iter().enumerate() {
        let _ = writeln!(
            manifest,
            "    <item id=\"c{index}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>",
            xml_escape(&entry.href)
        );
    }
    for (index, path) in extra_texts.keys().enumerate() {
        let _ = writeln!(
            manifest,
            "    <item id=\"t{index}\" href=\"{}\" media-type=\"{}\"/>",
            xml_escape(path),
            media_type(path)
        );
    }
    let mut cover_id: Option<String> = None;
    let mut first_image_id: Option<String> = None;
    for (index, path) in assets.keys().enumerate() {
        let mt = media_type(path);
        let id = format!("r{index}");
        if mt.starts_with("image/") {
            if first_image_id.is_none() {
                first_image_id = Some(id.clone());
            }
            if cover_id.is_none() && path.to_ascii_lowercase().contains("cover") {
                cover_id = Some(id.clone());
            }
        }
        let _ = writeln!(
            manifest,
            "    <item id=\"{id}\" href=\"{}\" media-type=\"{mt}\"/>",
            xml_escape(path),
        );
    }
    let cover_meta = cover_id
        .or(first_image_id)
        .map(|id| format!("    <meta name=\"cover\" content=\"{id}\"/>\n"))
        .unwrap_or_default();

    let mut spine = String::new();
    for (index, _) in pending.iter().enumerate() {
        let _ = writeln!(spine, "    <itemref idref=\"c{index}\"/>");
    }

    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"2.0\" unique-identifier=\"bookid\">\n\
  <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
    <dc:title>{title}</dc:title>\n\
    <dc:language>zh</dc:language>\n\
    <dc:identifier id=\"bookid\">weread:{book_id}</dc:identifier>\n\
    <dc:creator>WeRead</dc:creator>\n\
{cover_meta}  </metadata>\n\
  <manifest>\n{manifest}  </manifest>\n\
  <spine toc=\"ncx\">\n{spine}  </spine>\n\
</package>\n",
        title = xml_escape(&source.title),
        book_id = xml_escape(&source.book_id),
    ))
}

fn build_ncx(
    source: &EpubSource,
    pending: &[(SpineEntry, Vec<String>)],
    chapter_hrefs: &BTreeMap<i64, String>,
) -> Result<String, EpubError> {
    let mut nav_map = String::new();
    for (index, chapter) in source.chapters.iter().enumerate() {
        let href = chapter_hrefs
            .get(&chapter.chapter_uid)
            .map(String::as_str)
            .or_else(|| {
                pending
                    .iter()
                    .find(|(entry, _)| entry.chapter_uid == chapter.chapter_uid)
                    .map(|(entry, _)| entry.href.as_str())
            })
            .ok_or(EpubError::MissingChapterText {
                chapter_uid: chapter.chapter_uid,
            })?;
        let _ = writeln!(
            nav_map,
            "    <navPoint id=\"navPoint-{order}\" playOrder=\"{order}\">\n      <navLabel><text>{title}</text></navLabel>\n      <content src=\"{href}\"/>\n    </navPoint>",
            order = index + 1,
            title = xml_escape(&chapter.title),
            href = xml_escape(href)
        );
    }
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n\
  <head>\n\
    <meta name=\"dtb:uid\" content=\"weread:{book_id}\"/>\n\
    <meta name=\"dtb:depth\" content=\"1\"/>\n\
    <meta name=\"dtb:totalPageCount\" content=\"0\"/>\n\
    <meta name=\"dtb:maxPageNumber\" content=\"0\"/>\n\
  </head>\n\
  <docTitle><text>{title}</text></docTitle>\n\
  <navMap>\n{nav_map}  </navMap>\n\
</ncx>\n",
        book_id = xml_escape(&source.book_id),
        title = xml_escape(&source.title),
    ))
}

fn container_xml() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n\
  <rootfiles>\n\
    <rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n\
  </rootfiles>\n\
</container>\n"
        .to_string()
}

fn stylesheet_links(path: &str, css_paths: &[String]) -> String {
    let mut links = String::new();
    for css in css_paths {
        let _ = write!(
            links,
            "<link rel=\"stylesheet\" type=\"text/css\" href=\"{}\"/>",
            xml_escape(&relative_href(path, css))
        );
    }
    links
}

fn normalize_chapter_xhtml(
    path: &str,
    bytes: &[u8],
    css_paths: &[String],
    title: &str,
) -> Result<Vec<u8>, EpubError> {
    let source = std::str::from_utf8(bytes).map_err(|_| EpubError::InvalidUtf8 {
        path: path.to_string(),
    })?;
    if let Some(normalized) = try_normalize_full_html(source, path, css_paths) {
        return Ok(normalized);
    }
    let repaired_full = repair_named_entities(source);
    if repaired_full != source {
        if let Some(normalized) = try_normalize_full_html(&repaired_full, path, css_paths) {
            return Ok(normalized);
        }
    }
    let void_repaired_full = self_close_void_html_tags(&repaired_full);
    if void_repaired_full != repaired_full {
        if let Some(normalized) = try_normalize_full_html(&void_repaired_full, path, css_paths) {
            return Ok(normalized);
        }
    }

    let fragments = body_fragments(source);
    let body = if fragments.is_empty() {
        strip_document_prolog(source)
    } else {
        fragments.join("\n")
    };
    let links = stylesheet_links(path, css_paths);
    let wrap = |body: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<!DOCTYPE html>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
<head><title>{}</title>{links}</head>\n\
<body>{body}</body>\n\
</html>\n",
            xml_escape(title)
        )
    };
    let document = wrap(&body);
    if parse_xml(&document).is_ok() {
        return Ok(document.into_bytes());
    }
    let repaired_body = repair_named_entities(&body);
    let repaired = wrap(&repaired_body);
    if parse_xml(&repaired).is_ok() {
        return Ok(repaired.into_bytes());
    }
    let void_repaired = wrap(&self_close_void_html_tags(&repaired_body));
    parse_xml(&void_repaired).map_err(|error| EpubError::MalformedXhtml {
        path: path.to_string(),
        detail: error.to_string(),
    })?;
    Ok(void_repaired.into_bytes())
}

fn try_normalize_full_html(source: &str, path: &str, css_paths: &[String]) -> Option<Vec<u8>> {
    let document = parse_xml(source).ok()?;
    if !document
        .root_element()
        .tag_name()
        .name()
        .eq_ignore_ascii_case("html")
    {
        return None;
    }
    if css_paths.is_empty()
        || document
            .descendants()
            .filter(|node| node.has_tag_name("link"))
            .filter_map(|node| node.attribute("href"))
            .any(|href| href.to_ascii_lowercase().ends_with(".css"))
    {
        return Some(source.as_bytes().to_vec());
    }
    if let Some(position) = source.to_ascii_lowercase().find("</head>") {
        let mut injected = String::with_capacity(source.len() + 64);
        injected.push_str(&source[..position]);
        injected.push_str(&stylesheet_links(path, css_paths));
        injected.push_str(&source[position..]);
        if parse_xml(&injected).is_ok() {
            return Some(injected.into_bytes());
        }
    }
    Some(source.as_bytes().to_vec())
}

fn html_named_entity_numeric(name: &str) -> Option<&'static str> {
    match name {
        "nbsp" => Some("&#160;"),
        "emsp" => Some("&#8195;"),
        "ensp" => Some("&#8194;"),
        "thinsp" => Some("&#8201;"),
        "zwnj" => Some("&#8204;"),
        "zwj" => Some("&#8205;"),
        "mdash" => Some("&#8212;"),
        "ndash" => Some("&#8211;"),
        "minus" => Some("&#8722;"),
        "hellip" => Some("&#8230;"),
        "ldquo" => Some("&#8220;"),
        "rdquo" => Some("&#8221;"),
        "lsquo" => Some("&#8216;"),
        "rsquo" => Some("&#8217;"),
        "sbquo" => Some("&#8218;"),
        "bdquo" => Some("&#8222;"),
        "laquo" => Some("&#171;"),
        "raquo" => Some("&#187;"),
        "lsaquo" => Some("&#8249;"),
        "rsaquo" => Some("&#8250;"),
        "middot" => Some("&#183;"),
        "bull" => Some("&#8226;"),
        "prime" => Some("&#8242;"),
        "Prime" => Some("&#8243;"),
        "copy" => Some("&#169;"),
        "reg" => Some("&#174;"),
        "trade" => Some("&#8482;"),
        "deg" => Some("&#176;"),
        "plusmn" => Some("&#177;"),
        "times" => Some("&#215;"),
        "divide" => Some("&#247;"),
        "sect" => Some("&#167;"),
        "para" => Some("&#182;"),
        "dagger" => Some("&#8224;"),
        "Dagger" => Some("&#8225;"),
        "permil" => Some("&#8240;"),
        "euro" => Some("&#8364;"),
        "pound" => Some("&#163;"),
        "yen" => Some("&#165;"),
        "cent" => Some("&#162;"),
        "curren" => Some("&#164;"),
        "shy" => Some("&#173;"),
        "macr" => Some("&#175;"),
        "acute" => Some("&#180;"),
        "micro" => Some("&#181;"),
        "cedil" => Some("&#184;"),
        "ordf" => Some("&#170;"),
        "ordm" => Some("&#186;"),
        "iexcl" => Some("&#161;"),
        "iquest" => Some("&#191;"),
        "brvbar" => Some("&#166;"),
        "uml" => Some("&#168;"),
        "not" => Some("&#172;"),
        "sup1" => Some("&#185;"),
        "sup2" => Some("&#178;"),
        "sup3" => Some("&#179;"),
        "frac14" => Some("&#188;"),
        "frac12" => Some("&#189;"),
        "frac34" => Some("&#190;"),
        "tilde" => Some("&#732;"),
        "circ" => Some("&#710;"),
        "larr" => Some("&#8592;"),
        "uarr" => Some("&#8593;"),
        "rarr" => Some("&#8594;"),
        "darr" => Some("&#8595;"),
        "harr" => Some("&#8596;"),
        "le" => Some("&#8804;"),
        "ge" => Some("&#8805;"),
        "ne" => Some("&#8800;"),
        "approx" => Some("&#8776;"),
        "infin" => Some("&#8734;"),
        "radic" => Some("&#8730;"),
        "sum" => Some("&#8721;"),
        "alpha" => Some("&#945;"),
        "beta" => Some("&#946;"),
        "gamma" => Some("&#947;"),
        "delta" => Some("&#948;"),
        "pi" => Some("&#960;"),
        "sigma" => Some("&#963;"),
        "omega" => Some("&#969;"),
        "Alpha" => Some("&#913;"),
        "Beta" => Some("&#914;"),
        "Gamma" => Some("&#915;"),
        "Delta" => Some("&#916;"),
        "Omega" => Some("&#937;"),
        "spades" => Some("&#9824;"),
        "clubs" => Some("&#9827;"),
        "hearts" => Some("&#9829;"),
        "diams" => Some("&#9830;"),
        "loz" => Some("&#9674;"),
        "eacute" => Some("&#233;"),
        "egrave" => Some("&#232;"),
        "ecirc" => Some("&#234;"),
        "aacute" => Some("&#225;"),
        "agrave" => Some("&#224;"),
        "uuml" => Some("&#252;"),
        "ouml" => Some("&#246;"),
        "auml" => Some("&#228;"),
        "ntilde" => Some("&#241;"),
        "ccedil" => Some("&#231;"),
        _ => None,
    }
}

fn is_valid_numeric_entity(body: &str) -> bool {
    let code = if let Some(hex) = body.strip_prefix('x').or_else(|| body.strip_prefix('X')) {
        if hex.is_empty() || hex.len() > 6 {
            return false;
        }
        u32::from_str_radix(hex, 16).ok()
    } else {
        if body.is_empty() || body.len() > 7 {
            return false;
        }
        body.parse::<u32>().ok()
    };
    code.and_then(char::from_u32)
        .is_some_and(|ch| ch != '\0' && (ch >= ' ' || matches!(ch, '\t' | '\n' | '\r')))
}

fn repair_named_entities(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            let next = source[i..]
                .find('&')
                .map(|off| i + off)
                .unwrap_or(bytes.len());
            out.push_str(&source[i..next]);
            i = next;
            continue;
        }
        // Inspect up to 32 bytes for a `;` terminating a candidate entity.
        let rest = &source[i + 1..];
        let semi = rest.find(';').filter(|&pos| pos > 0 && pos <= 32);
        if let Some(pos) = semi {
            let candidate = &rest[..pos];
            if matches!(candidate, "amp" | "lt" | "gt" | "quot" | "apos") {
                out.push_str(&source[i..i + 1 + pos + 1]);
                i += 1 + pos + 1;
                continue;
            }
            if let Some(num_body) = candidate.strip_prefix('#') {
                if is_valid_numeric_entity(num_body) {
                    out.push_str(&source[i..i + 1 + pos + 1]);
                    i += 1 + pos + 1;
                    continue;
                }
            } else if candidate.bytes().all(|b| b.is_ascii_alphanumeric()) {
                if let Some(numeric) = html_named_entity_numeric(candidate) {
                    out.push_str(numeric);
                    i += 1 + pos + 1;
                    continue;
                }
            }
        }
        // Bare `&` or unknown HTML entity name: escape `&` so roxmltree accepts it.
        out.push_str("&amp;");
        i += 1;
    }
    out
}

fn is_html_void_tag(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn self_close_void_html_tags(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 32);
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] != b'<' {
            let next = source[i..].find('<').map(|off| i + off).unwrap_or(len);
            out.push_str(&source[i..next]);
            i = next;
            continue;
        }
        let rest = &source[i..];
        if rest.starts_with("<!--") {
            let end = rest.find("-->").map(|off| i + off + 3).unwrap_or(len);
            out.push_str(&source[i..end]);
            i = end;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").map(|off| i + off + 3).unwrap_or(len);
            out.push_str(&source[i..end]);
            i = end;
            continue;
        }
        if i + 1 >= len || matches!(bytes[i + 1], b'/' | b'!' | b'?') {
            out.push('<');
            i += 1;
            continue;
        }

        let name_start = i + 1;
        let mut name_end = name_start;
        while name_end < len && bytes[name_end].is_ascii_alphanumeric() {
            name_end += 1;
        }
        if name_end == name_start {
            out.push('<');
            i += 1;
            continue;
        }
        let tag_name = &source[name_start..name_end];
        let valid_delim = name_end < len
            && (bytes[name_end].is_ascii_whitespace()
                || bytes[name_end] == b'>'
                || bytes[name_end] == b'/');
        if !valid_delim {
            out.push('<');
            i += 1;
            continue;
        }

        let mut pos = name_end;
        let mut quote: Option<u8> = None;
        while pos < len {
            let b = bytes[pos];
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
            } else if b == b'"' || b == b'\'' {
                quote = Some(b);
            } else if b == b'>' {
                break;
            }
            pos += 1;
        }
        if pos >= len {
            out.push_str(&source[i..]);
            break;
        }

        if is_html_void_tag(tag_name) {
            let mut last = pos;
            while last > name_end && bytes[last - 1].is_ascii_whitespace() {
                last -= 1;
            }
            let already_self_closed = last > name_end && bytes[last - 1] == b'/';
            let has_immediate_close = {
                let after = source[pos + 1..].trim_start().as_bytes();
                let tag_bytes = tag_name.as_bytes();
                after.starts_with(b"</")
                    && after.len() > 2 + tag_bytes.len()
                    && after[2..2 + tag_bytes.len()].eq_ignore_ascii_case(tag_bytes)
                    && after[2 + tag_bytes.len()..]
                        .iter()
                        .find(|b| !b.is_ascii_whitespace())
                        == Some(&b'>')
            };
            if already_self_closed || has_immediate_close {
                out.push_str(&source[i..=pos]);
            } else {
                out.push_str(&source[i..pos]);
                out.push_str("/>");
            }
        } else {
            out.push_str(&source[i..=pos]);
        }
        i = pos + 1;
    }
    out
}

fn body_fragments(source: &str) -> Vec<String> {
    let lower = source.to_ascii_lowercase();
    let mut fragments = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("<body") {
        let start = cursor + relative;
        let Some(open_end) = source[start..].find('>') else {
            break;
        };
        let content_start = start + open_end + 1;
        let Some(close) = lower[content_start..].find("</body>") else {
            fragments.push(source[content_start..].to_string());
            break;
        };
        let content_end = content_start + close;
        fragments.push(source[content_start..content_end].to_string());
        cursor = content_end + "</body>".len();
    }
    fragments
}

fn strip_document_prolog(source: &str) -> String {
    let mut text = source.to_string();
    loop {
        let lower = text.to_ascii_lowercase();
        let Some(start) = lower.find("<?xml") else {
            break;
        };
        let Some(end) = text[start..].find("?>") else {
            break;
        };
        text.replace_range(start..start + end + 2, "");
    }
    loop {
        let lower = text.to_ascii_lowercase();
        let Some(start) = lower.find("<!doctype") else {
            break;
        };
        let Some(end) = text[start..].find('>') else {
            break;
        };
        text.replace_range(start..start + end + 1, "");
    }
    text
}

fn relative_href(from_file: &str, target: &str) -> String {
    let from_directory: Vec<&str> = from_file
        .rsplit_once('/')
        .map(|(directory, _)| directory.split('/').collect())
        .unwrap_or_default();
    let target_parts: Vec<&str> = target.split('/').collect();
    let mut common = 0;
    while common < from_directory.len()
        && common < target_parts.len()
        && from_directory[common] == target_parts[common]
    {
        common += 1;
    }
    let mut parts: Vec<&str> = Vec::new();
    parts.extend(std::iter::repeat_n("..", from_directory.len() - common));
    parts.extend_from_slice(&target_parts[common..]);
    parts.join("/")
}

fn parse_xml(text: &str) -> Result<Document<'_>, roxmltree::Error> {
    Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..roxmltree::ParsingOptions::default()
        },
    )
}

fn xhtml_references(path: &str, bytes: &[u8]) -> Result<Vec<String>, EpubError> {
    let text = std::str::from_utf8(bytes).map_err(|_| EpubError::InvalidUtf8 {
        path: path.to_string(),
    })?;
    let document = parse_xml(text).map_err(|error| EpubError::MalformedXhtml {
        path: path.to_string(),
        detail: error.to_string(),
    })?;
    let mut references = Vec::new();
    for node in document.descendants().filter(|node| node.is_element()) {
        if let Some(value) = node.attribute("src") {
            push_reference(&mut references, value);
        }
        // <a> links may point at chapters outside the downloaded subset; only
        // package dependencies (stylesheets, images, SVG references) are required.
        if !node.has_tag_name("a") {
            if let Some(value) = node.attribute("href") {
                push_reference(&mut references, value);
            }
            if let Some(value) = node.attribute(("http://www.w3.org/1999/xlink", "href")) {
                push_reference(&mut references, value);
            }
        }
    }
    Ok(references)
}

fn push_reference(references: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() || value.starts_with('#') || has_scheme(value) {
        return;
    }
    references.push(value.to_string());
}

fn has_scheme(value: &str) -> bool {
    let head = value.split(['/', '?', '#']).next().unwrap_or_default();
    head.contains(':')
}

#[cfg(test)]
fn css_url_references(css: &str) -> Vec<String> {
    let mut references = Vec::new();
    let bytes = css.as_bytes();
    let mut cursor = 0;
    while let Some(relative) = css[cursor..].find("url(") {
        let mut position = cursor + relative + 4;
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        let value;
        if position < bytes.len() && (bytes[position] == b'"' || bytes[position] == b'\'') {
            let quote = bytes[position];
            position += 1;
            let start = position;
            while position < bytes.len() && bytes[position] != quote {
                position += 1;
            }
            value = css[start..position].trim().to_string();
            if position < bytes.len() {
                position += 1;
            }
        } else {
            let start = position;
            while position < bytes.len() && bytes[position] != b')' {
                position += 1;
            }
            value = css[start..position].trim().to_string();
        }
        while position < bytes.len() && bytes[position] != b')' {
            position += 1;
        }
        if position < bytes.len() {
            position += 1;
        }
        cursor = position.max(cursor + 1);
        if !value.is_empty() && !value.starts_with('#') && !has_scheme(&value) {
            references.push(value);
        }
    }
    references
}

#[cfg(test)]
fn resolve_reference(base_file: &str, reference: &str) -> Option<String> {
    let reference = reference.split(['#', '?']).next().unwrap_or_default();
    if reference.is_empty() {
        return None;
    }
    let base_dir = base_file
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .unwrap_or_default();
    let mut parts: Vec<&str> = if reference.starts_with('/') || base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for segment in reference.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(segment),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

pub(crate) fn normalize_archive_path(raw: &str) -> Result<String, EpubError> {
    let path = raw.replace('\\', "/");
    if path.is_empty() || path.starts_with('/') || path.contains('\0') {
        return Err(EpubError::UnsafePath(raw.to_string()));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(EpubError::UnsafePath(raw.to_string())),
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        Err(EpubError::UnsafePath(raw.to_string()))
    } else {
        Ok(parts.join("/"))
    }
}

pub(crate) fn is_info_txt_path(path: &str) -> bool {
    let base = path.rsplit_once('/').map(|(_, name)| name).unwrap_or(path);
    base.eq_ignore_ascii_case("info.txt")
}

pub(crate) fn find_info_txt(files: &BTreeMap<String, Vec<u8>>) -> Option<(&str, &[u8])> {
    if let Some((key, bytes)) = files.get_key_value("info.txt") {
        return Some((key.as_str(), bytes.as_slice()));
    }
    if let Some((key, bytes)) = files
        .iter()
        .find(|(path, _)| !path.contains('/') && path.eq_ignore_ascii_case("info.txt"))
    {
        return Some((key.as_str(), bytes.as_slice()));
    }
    files
        .iter()
        .find(|(path, _)| is_info_txt_path(path))
        .map(|(key, bytes)| (key.as_str(), bytes.as_slice()))
}

fn is_reserved_container_path(lower: &str) -> bool {
    lower == "mimetype"
        || lower == "meta-inf"
        || lower.starts_with("meta-inf/")
        || lower.split('/').any(|seg| seg == "__macosx")
}

fn is_safe_oebps_content_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    !is_info_txt_path(path)
        && !is_reserved_container_path(&lower)
        && lower != "content.opf"
        && lower != "toc.ncx"
}

fn is_package_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if is_reserved_container_path(&lower) {
        return false;
    }
    let basename = lower
        .rsplit_once('/')
        .map(|(_, base)| base)
        .unwrap_or(&lower);
    if basename == "info.txt" {
        return true;
    }
    let Some((_, ext)) = basename.rsplit_once('.') else {
        return false;
    };
    if matches!(ext, "opf" | "ncx" | "plist") {
        return false;
    }
    matches!(
        ext,
        "xhtml"
            | "html"
            | "htm"
            | "css"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "webp"
            | "bmp"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
    )
}

fn media_type(path: &str) -> &'static str {
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "xhtml" | "html" | "htm" => "application/xhtml+xml",
        "css" => "text/css",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn map_zip_error(error: zip::result::ZipError) -> EpubError {
    match error {
        zip::result::ZipError::InvalidPassword => EpubError::WrongPassword,
        other => EpubError::Zip(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    const FIXTURE_B64: &str =
        include_str!("../../../tests/fixtures/weread/chapter-package.zip.b64");
    const FIXTURE_PASSWORD: &str = "synthetic-zip-pass";

    fn fixture_zip() -> Vec<u8> {
        let compact: String = FIXTURE_B64.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(compact)
            .expect("fixture base64")
    }

    fn fixture_package() -> ExtractedPackage {
        extract_chapter_package(&fixture_zip(), FIXTURE_PASSWORD).expect("fixture decrypts")
    }

    fn source_from_fixture() -> EpubSource {
        let package = fixture_package();
        let mut texts = BTreeMap::new();
        for path in ["Text/ch1.xhtml", "Text/ch2.xhtml"] {
            texts.insert(path.to_string(), package.files[path].clone());
        }
        let mut assets = BTreeMap::new();
        for (path, bytes) in &package.files {
            if path.starts_with("Images/") || path.starts_with("Styles/") {
                assets.insert(path.clone(), bytes.clone());
            }
        }
        EpubSource {
            book_id: "100001".to_string(),
            title: "Synthetic Three Chapters".to_string(),
            chapters: vec![
                EpubChapter {
                    chapter_uid: 1,
                    chapter_idx: 1,
                    title: "Chapter One".to_string(),
                    files: vec!["Text/ch1.xhtml".to_string()],
                },
                EpubChapter {
                    chapter_uid: 2,
                    chapter_idx: 2,
                    title: "Chapter Two".to_string(),
                    files: vec!["Text/ch2.xhtml".to_string()],
                },
            ],
            texts,
            assets,
        }
    }

    fn zip_entries(bytes: &[u8]) -> Vec<(String, CompressionMethod, Vec<u8>)> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output zip");
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

    #[test]
    fn real_zipcrypto_fixture_decrypts_and_parses_info_txt() {
        let package = fixture_package();
        assert_eq!(package.info.chapters.len(), 3);
        assert_eq!(
            package.chapter_files(1).unwrap(),
            ["Text/ch1.xhtml".to_string()]
        );
        assert_eq!(
            package.chapter_files(2).unwrap(),
            ["Text/ch2.xhtml".to_string()]
        );
        assert!(package.files.contains_key("Text/0.xhtml"));
        assert!(package.files.contains_key("Images/cover.png"));
        assert!(package.files.contains_key("Styles/shared.css"));
        assert!(!package.files.contains_key("Text/"));
    }

    #[test]
    fn wrong_zipcrypto_password_is_rejected() {
        let error = extract_chapter_package(&fixture_zip(), "not-the-password").unwrap_err();
        assert!(
            matches!(error, EpubError::WrongPassword | EpubError::Zip(_)),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn unsafe_entry_paths_are_rejected() {
        assert!(matches!(
            normalize_archive_path("../Text/ch1.xhtml"),
            Err(EpubError::UnsafePath(_))
        ));
        assert!(matches!(
            normalize_archive_path("/Text/ch1.xhtml"),
            Err(EpubError::UnsafePath(_))
        ));
        assert!(matches!(
            normalize_archive_path("Text\\..\\ch1.xhtml"),
            Err(EpubError::UnsafePath(_))
        ));
        assert_eq!(
            normalize_archive_path("./Text//ch1.xhtml").unwrap(),
            "Text/ch1.xhtml"
        );
    }

    #[test]
    fn built_epub_has_mimetype_first_stored_and_ordered_spine() {
        let source = source_from_fixture();
        let built = build_epub(&source).expect("build");
        let entries = zip_entries(&built.bytes);
        assert_eq!(entries[0].0, "mimetype");
        assert_eq!(entries[0].1, CompressionMethod::Stored);
        assert_eq!(entries[0].2, EPUB_MIMETYPE.as_bytes());
        let local = &built.bytes[..30];
        assert_eq!(&local[0..4], b"PK\x03\x04");
        let name_len = u16::from_le_bytes([local[26], local[27]]) as usize;
        let extra_len = u16::from_le_bytes([local[28], local[29]]) as usize;
        assert_eq!(name_len, "mimetype".len());
        assert_eq!(extra_len, 0);
        assert_eq!(&built.bytes[30..30 + name_len], b"mimetype");

        let mut archive = ZipArchive::new(Cursor::new(&built.bytes)).expect("zip");
        let mut container = String::new();
        archive
            .by_name("META-INF/container.xml")
            .unwrap()
            .read_to_string(&mut container)
            .unwrap();
        let container_doc = Document::parse(&container).unwrap();
        assert_eq!(
            container_doc
                .descendants()
                .find(|node| node.has_tag_name("rootfile"))
                .unwrap()
                .attribute("full-path"),
            Some("OEBPS/content.opf")
        );

        let mut opf = String::new();
        archive
            .by_name("OEBPS/content.opf")
            .unwrap()
            .read_to_string(&mut opf)
            .unwrap();
        let opf_doc = Document::parse(&opf).unwrap();
        let mut ids = BTreeMap::new();
        for item in opf_doc
            .descendants()
            .filter(|node| node.has_tag_name("item"))
        {
            ids.insert(
                item.attribute("id").unwrap().to_string(),
                item.attribute("href").unwrap().to_string(),
            );
        }
        let spine: Vec<String> = opf_doc
            .descendants()
            .filter(|node| node.has_tag_name("itemref"))
            .map(|node| ids[node.attribute("idref").unwrap()].clone())
            .collect();
        assert_eq!(spine, ["Text/ch1.xhtml", "Text/ch2.xhtml"]);
        assert_eq!(built.spine.len(), 2);
        assert_eq!(built.spine[0].chapter_uid, 1);
        assert_eq!(built.spine[1].chapter_uid, 2);

        let names: Vec<&str> = entries.iter().map(|(name, _, _)| name.as_str()).collect();
        assert!(!names.contains(&"OEBPS/Text/0.xhtml"));
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

        for chapter in ["OEBPS/Text/ch1.xhtml", "OEBPS/Text/ch2.xhtml"] {
            let bytes = entries
                .iter()
                .find(|(name, _, _)| name == chapter)
                .map(|(_, _, bytes)| bytes)
                .unwrap();
            Document::parse(std::str::from_utf8(bytes).unwrap()).expect("chapter well-formed");
        }

        let mut ncx = String::new();
        archive
            .by_name("OEBPS/toc.ncx")
            .unwrap()
            .read_to_string(&mut ncx)
            .unwrap();
        let ncx_doc = Document::parse(&ncx).unwrap();
        let nav_targets: Vec<&str> = ncx_doc
            .descendants()
            .filter(|node| node.has_tag_name("content"))
            .filter_map(|node| node.attribute("src"))
            .collect();
        assert_eq!(nav_targets, ["Text/ch1.xhtml", "Text/ch2.xhtml"]);
        assert_eq!(built.hash.len(), 32);
    }

    #[test]
    fn non_spine_text_files_are_packed_and_manifest_listed() {
        let mut source = source_from_fixture();
        source
            .texts
            .insert("Text/pics/inline.png".to_string(), b"PNG-in-Text".to_vec());
        source.texts.insert(
            "Text/appendix.xhtml".to_string(),
            br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p>appendix</p></body></html>"#.to_vec(),
        );
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><img src="pics/inline.png"/></body></html>"#.to_vec(),
        );

        let built = build_epub(&source).expect("non-spine Text files must not be dropped");
        let entries = zip_entries(&built.bytes);
        assert_eq!(
            entry(&entries, "OEBPS/Text/pics/inline.png"),
            &b"PNG-in-Text"[..]
        );
        assert!(entry(&entries, "OEBPS/Text/appendix.xhtml").starts_with(b"<?xml"));

        let mut archive = ZipArchive::new(Cursor::new(&built.bytes)).expect("zip");
        let mut opf = String::new();
        archive
            .by_name("OEBPS/content.opf")
            .unwrap()
            .read_to_string(&mut opf)
            .unwrap();
        let opf_doc = Document::parse(&opf).unwrap();
        let manifest: BTreeMap<&str, &str> = opf_doc
            .descendants()
            .filter(|node| node.has_tag_name("item"))
            .map(|node| {
                (
                    node.attribute("id").unwrap(),
                    node.attribute("href").unwrap(),
                )
            })
            .collect();
        let manifest_hrefs: Vec<&str> = manifest.values().copied().collect();
        assert!(manifest_hrefs.contains(&"Text/pics/inline.png"));
        assert!(manifest_hrefs.contains(&"Text/appendix.xhtml"));

        let spine: Vec<&str> = opf_doc
            .descendants()
            .filter(|node| node.has_tag_name("itemref"))
            .map(|node| manifest[node.attribute("idref").unwrap()])
            .collect();
        assert_eq!(spine, ["Text/ch1.xhtml", "Text/ch2.xhtml"]);
        assert_eq!(built.spine.len(), 2);
    }

    #[test]
    fn xhtml_with_a_doctype_is_accepted() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<?xml version="1.0" encoding="utf-8"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd"><html xmlns="http://www.w3.org/1999/xhtml"><head><title>One</title></head><body><p>ok</p></body></html>"#.to_vec(),
        );
        let built = build_epub(&source).expect("DOCTYPE accepted");
        assert_eq!(built.spine[0].href, "Text/ch1.xhtml");
    }

    #[test]
    fn entries_that_normalize_to_the_same_path_are_rejected() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        writer.start_file("Text/ch1.xhtml", options).unwrap();
        writer.write_all(b"<html/>").unwrap();
        writer.start_file("Text\\ch1.xhtml", options).unwrap();
        writer.write_all(b"<html/>").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(matches!(
            extract_chapter_package(&bytes, ""),
            Err(EpubError::DuplicateEntry(path)) if path == "Text/ch1.xhtml"
        ));
    }

    #[test]
    fn concatenated_chapter_documents_are_normalized_into_one_document() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>a</title></head><body><p>one</p></body></html><?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p>two</p></body></html>"#.to_vec(),
        );
        let built = build_epub(&source).expect("normalized");
        let entries = zip_entries(&built.bytes);
        let text = std::str::from_utf8(entry(&entries, "OEBPS/Text/ch1.xhtml")).unwrap();
        let document = parse_xml(text).expect("valid normalized XHTML");
        let text_nodes: Vec<&str> = document
            .descendants()
            .filter_map(|node| node.text())
            .collect();
        assert!(text_nodes.contains(&"one"));
        assert!(text_nodes.contains(&"two"));
        assert!(text.contains("../Styles/shared.css"));
    }

    #[test]
    fn a_body_fragment_is_wrapped_and_gets_the_shared_styles() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            b"<p class=\"body\">fragment</p>".to_vec(),
        );
        let built = build_epub(&source).expect("wrapped");
        let entries = zip_entries(&built.bytes);
        let text = std::str::from_utf8(entry(&entries, "OEBPS/Text/ch1.xhtml")).unwrap();
        let document = parse_xml(text).expect("valid wrapped XHTML");
        assert!(document
            .descendants()
            .any(|node| node.text() == Some("fragment")));
        assert!(text.contains("../Styles/shared.css"));
        assert!(text.contains("../Styles/ch2.css"));
    }

    #[test]
    fn common_html_named_entities_are_repaired_in_wrapped_chapters() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            b"<p>one&nbsp;two &mdash; three</p>".to_vec(),
        );
        let built = build_epub(&source).expect("entities repaired");
        let entries = zip_entries(&built.bytes);
        let text = std::str::from_utf8(entry(&entries, "OEBPS/Text/ch1.xhtml")).unwrap();
        assert!(text.contains("&#160;"));
        assert!(text.contains("&#8212;"));
        parse_xml(text).expect("valid repaired XHTML");
    }

    #[test]
    fn a_valid_chapter_without_stylesheets_gets_the_shared_links() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>One</title></head><body><p>one</p></body></html>"#.to_vec(),
        );
        let built = build_epub(&source).expect("links injected");
        let entries = zip_entries(&built.bytes);
        let text = std::str::from_utf8(entry(&entries, "OEBPS/Text/ch1.xhtml")).unwrap();
        assert!(text.contains("../Styles/shared.css"));
        assert!(text.contains("../Styles/ch2.css"));
        parse_xml(text).expect("valid injected XHTML");
    }

    #[test]
    fn links_to_chapters_outside_the_download_subset_are_allowed() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>one</p><a href="not-downloaded.xhtml">next</a></body></html>"#.to_vec(),
        );
        assert!(build_epub(&source).is_ok());
    }

    #[test]
    fn malformed_chapter_xhtml_is_reported() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            b"<html><body><p>unterminated</body></html>".to_vec(),
        );
        assert!(matches!(
            build_epub(&source),
            Err(EpubError::MalformedXhtml { .. })
        ));
    }

    #[test]
    fn missing_image_reference_does_not_abort_epub_build() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><img src="../Images/missing.png"/></body></html>"#.to_vec(),
        );
        let built = build_epub(&source).expect("missing external/tar image must not abort rebuild");
        assert_eq!(built.spine.len(), 2);
    }

    #[test]
    fn shared_xhtml_across_chapters_deduplicates_spine_and_keeps_ncx() {
        let mut source = source_from_fixture();
        source.chapters.push(EpubChapter {
            chapter_uid: 3,
            chapter_idx: 3,
            title: "Section 2B".to_string(),
            files: vec!["Text/ch2.xhtml".to_string()],
        });
        let built = build_epub(&source).expect("shared XHTML should build cleanly");
        assert_eq!(built.spine.len(), 2);
        let entries = zip_entries(&built.bytes);
        let ncx = std::str::from_utf8(entry(&entries, "OEBPS/toc.ncx")).unwrap();
        assert!(ncx.contains("Section 2B"));
    }

    #[test]
    fn full_html_with_named_entities_and_bare_ampersand_preserves_head() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><style>.keep{color:black}</style></head><body><p>A & B &nbsp; &rarr; &unknownEntity;</p></body></html>"#.to_vec(),
        );
        let built = build_epub(&source).expect("full HTML entities repaired");
        let entries = zip_entries(&built.bytes);
        let text = std::str::from_utf8(entry(&entries, "OEBPS/Text/ch1.xhtml")).unwrap();
        assert!(text.contains(".keep{color:black}"));
        assert!(text.contains("&#160;"));
        assert!(text.contains("&#8594;"));
        assert!(text.contains("A &amp; B"));
        parse_xml(text).expect("valid XML");
    }

    #[test]
    fn css_url_references_are_scanned() {
        assert_eq!(
            css_url_references(
                r#"body { background: url("../Images/a.png"); } .x { background: url('b.png'); }"#
            ),
            ["../Images/a.png".to_string(), "b.png".to_string()]
        );
        assert!(
            css_url_references("body { background: url(data:image/png;base64,AA==); }").is_empty()
        );
    }

    #[test]
    fn missing_stylesheet_import_does_not_abort_epub_build() {
        let mut source = source_from_fixture();
        source.assets.insert(
            "Styles/obfuscated.css".to_string(),
            br#"@import "fonts.css"; body { background: url("../Fonts/missing.ttf"); }"#.to_vec(),
        );
        let built = build_epub(&source).expect("dangling CSS refs must not abort EPUB rebuild");
        assert!(!built.bytes.is_empty());
    }

    #[test]
    fn reference_resolution_stays_inside_the_oebps_root() {
        assert_eq!(
            resolve_reference("Text/ch1.xhtml", "../Images/a.png").unwrap(),
            "Images/a.png"
        );
        assert_eq!(
            resolve_reference("Styles/a.css", "../../../etc/passwd"),
            None
        );
        assert_eq!(
            resolve_reference("Text/ch1.xhtml", "ch2.xhtml#frag").unwrap(),
            "Text/ch2.xhtml"
        );
    }

    #[test]
    fn root_and_alternate_folder_package_extracts_and_rebuilds() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        let info = br#"{"chapters":[{"chapterUid":1,"files":["coverpage.html"]},{"chapterUid":2,"files":["xhtml/ch2.htm"]}]}"#;
        let files: &[(&str, &[u8])] = &[
            ("META-INF/container.xml", b"<container/>"),
            ("mimetype", EPUB_MIMETYPE.as_bytes()),
            ("content.opf", b"<package/>"),
            ("toc.ncx", b"<ncx/>"),
            ("iTunesMetadata.plist", b"<plist/>"),
            ("__MACOSX/coverpage.html", b"junk"),
            ("OEBPS/INFO.TXT", info),
            (
                "coverpage.html",
                br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Cover</title></head><body><img src="cover.jpg"/></body></html>"#,
            ),
            (
                "xhtml/ch2.htm",
                br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Ch2</title></head><body><p>Hello</p><img src="../images/pic.png"/></body></html>"#,
            ),
            ("stylesheet.css", b"body { color: #000; }"),
            ("cover.jpg", b"JPEG-COVER"),
            ("images/pic.png", b"PNG-PIC"),
            ("Fonts/book.ttf", b"TTF-FONT"),
        ];
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        let zip_bytes = writer.finish().unwrap().into_inner();

        let extracted = extract_chapter_package(&zip_bytes, "").expect("extracts root layout");
        assert!(!extracted.files.contains_key("META-INF/container.xml"));
        assert!(!extracted.files.contains_key("mimetype"));
        assert!(!extracted.files.contains_key("content.opf"));
        assert!(!extracted.files.contains_key("toc.ncx"));
        assert!(!extracted.files.contains_key("iTunesMetadata.plist"));
        assert!(!extracted.files.contains_key("__MACOSX/coverpage.html"));
        assert!(extracted.files.contains_key("OEBPS/INFO.TXT"));
        assert!(extracted.files.contains_key("coverpage.html"));
        assert!(extracted.files.contains_key("xhtml/ch2.htm"));
        assert!(extracted.files.contains_key("stylesheet.css"));
        assert!(extracted.files.contains_key("cover.jpg"));
        assert!(extracted.files.contains_key("images/pic.png"));
        assert!(extracted.files.contains_key("Fonts/book.ttf"));

        assert_eq!(
            relative_href("coverpage.html", "stylesheet.css"),
            "stylesheet.css"
        );
        assert_eq!(
            relative_href("Text/ch1.xhtml", "stylesheet.css"),
            "../stylesheet.css"
        );
        assert_eq!(
            relative_href("xhtml/ch2.htm", "stylesheet.css"),
            "../stylesheet.css"
        );

        let mut texts = BTreeMap::new();
        texts.insert(
            "coverpage.html".to_string(),
            extracted.files["coverpage.html"].clone(),
        );
        texts.insert(
            "xhtml/ch2.htm".to_string(),
            extracted.files["xhtml/ch2.htm"].clone(),
        );
        let mut assets = BTreeMap::new();
        for key in [
            "stylesheet.css",
            "cover.jpg",
            "images/pic.png",
            "Fonts/book.ttf",
        ] {
            assets.insert(key.to_string(), extracted.files[key].clone());
        }
        let source = EpubSource {
            book_id: "root-layout-1".to_string(),
            title: "Root Layout Book".to_string(),
            chapters: vec![
                EpubChapter {
                    chapter_uid: 1,
                    chapter_idx: 1,
                    title: "Cover".to_string(),
                    files: vec!["coverpage.html".to_string()],
                },
                EpubChapter {
                    chapter_uid: 2,
                    chapter_idx: 2,
                    title: "Chapter 2".to_string(),
                    files: vec!["xhtml/ch2.htm".to_string()],
                },
            ],
            texts,
            assets,
        };
        let built = build_epub(&source).expect("builds root-layout EPUB");
        assert_eq!(built.spine.len(), 2);
        assert_eq!(built.spine[0].href, "coverpage.html");
        assert_eq!(built.spine[1].href, "xhtml/ch2.htm");

        let entries = zip_entries(&built.bytes);
        let cover_html = std::str::from_utf8(entry(&entries, "OEBPS/coverpage.html")).unwrap();
        assert!(cover_html.contains("href=\"stylesheet.css\""));
        parse_xml(cover_html).expect("valid cover XHTML");

        let ch2_html = std::str::from_utf8(entry(&entries, "OEBPS/xhtml/ch2.htm")).unwrap();
        assert!(ch2_html.contains("href=\"../stylesheet.css\""));
        parse_xml(ch2_html).expect("valid ch2 XHTML");

        assert_eq!(
            entry(&entries, "OEBPS/stylesheet.css"),
            b"body { color: #000; }"
        );
        assert_eq!(entry(&entries, "OEBPS/cover.jpg"), b"JPEG-COVER");
        assert_eq!(entry(&entries, "OEBPS/images/pic.png"), b"PNG-PIC");
        assert_eq!(entry(&entries, "OEBPS/Fonts/book.ttf"), b"TTF-FONT");
    }

    #[test]
    fn unclosed_void_html_tags_are_self_closed_in_full_and_fragment_chapters() {
        let mut source = source_from_fixture();
        source.texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<!DOCTYPE html><html><head><meta charset="utf-8"><link rel="stylesheet" href="../Styles/shared.css"><base href="./"></head><body><!-- <br> --><p>Line1<br>Line2&nbsp;<wbr>end</p><hr><img src="../Images/cover.png" alt="a > b"><input type="text" name="x"><table><colgroup><col span="1"></colgroup><tr><td>ok</td></tr></table><img src="../Images/inline.png"></img></body></html>"#.to_vec(),
        );
        source.texts.insert(
            "Text/ch2.xhtml".to_string(),
            br#"<p>Frag<br>Two&mdash;three</p><hr><img src="../Images/cover.png"><source src="a.mp3"><area shape="rect">"#.to_vec(),
        );

        let built = build_epub(&source).expect("void tags repaired");
        let entries = zip_entries(&built.bytes);

        let ch1 = std::str::from_utf8(entry(&entries, "OEBPS/Text/ch1.xhtml")).unwrap();
        parse_xml(ch1).expect("repaired full HTML parses as XML");
        assert!(ch1.contains("<meta charset=\"utf-8\"/>"));
        assert!(ch1.contains("<br/>"));
        assert!(ch1.contains("<hr/>"));
        assert!(ch1.contains("alt=\"a > b\"/>"));
        assert!(ch1.contains("<colgroup><col span=\"1\"/></colgroup>"));

        let ch2 = std::str::from_utf8(entry(&entries, "OEBPS/Text/ch2.xhtml")).unwrap();
        parse_xml(ch2).expect("repaired fragment parses as XML");
        assert!(ch2.contains("<br/>"));
        assert!(ch2.contains("<hr/>"));
        assert!(ch2.contains("<img src=\"../Images/cover.png\"/>"));
        assert!(ch2.contains("&#8212;"));
    }

    #[test]
    fn build_epub_rejects_reserved_and_unsafe_paths() {
        let mut source = source_from_fixture();
        source
            .texts
            .insert("META-INF/evil.html".to_string(), b"<html/>".to_vec());
        assert!(matches!(
            build_epub(&source),
            Err(EpubError::InvalidTextPath(_))
        ));

        let mut source = source_from_fixture();
        source
            .assets
            .insert("mimetype".to_string(), b"application/epub+zip".to_vec());
        assert!(matches!(
            build_epub(&source),
            Err(EpubError::InvalidAssetPath(_))
        ));

        let mut source = source_from_fixture();
        source.assets.insert("info.txt".to_string(), b"{}".to_vec());
        assert!(matches!(
            build_epub(&source),
            Err(EpubError::InvalidAssetPath(_))
        ));

        let mut source = source_from_fixture();
        source
            .assets
            .insert("../escape.css".to_string(), b"body{}".to_vec());
        assert!(matches!(build_epub(&source), Err(EpubError::UnsafePath(_))));
    }
}
