// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto chapter splitting and cached EPUB generation for plain text files.
//!
//! crengine-ng natively supports EPUB with full DOM fragmenting, accurate table
//! of contents, and multi-threaded partial rerendering. By converting TXT files
//! into cached uncompressed EPUB documents (following the same design as
//! `mobi.rs`), we achieve:
//! 1. Native handling of GBK and GB18030 (via `aristotle_library::encoding`).
//! 2. Automatic chapter splitting based on standard Chinese web novel heading patterns.
//! 3. TOC integration into reader's TOC page via standard EPUB `toc.ncx`.
//! 4. Fast opening times with on-disk caching (uncompressed ZIP stored entries).

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use plato_core::anyhow::{format_err, Error};
use regex::Regex;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

/// Current version of the TXT-to-EPUB converter. Bumping this invalidates cached EPUBs.
pub const CONVERTER_VERSION: u32 = 1;

/// Regular expression matching common Chinese chapter headings:
/// - 第 x 章 / 回 / 卷 / 节 (where x is Arabic or Chinese numerals)
/// - 卷 x (e.g. 卷一, 卷三)
/// - 序章, 序言, 前言, 楔子, 后记, 尾声, 番外 (optionally numbered)
pub const CHAPTER_PATTERN: &str = r"^[\s　]*(?:(第[0-9一二两三四五六七八九十百千万零]+[章回卷节]|卷[0-9一二两三四五六七八九十百千万零]+|序章|序言|前言|楔子|后记|尾声|番外(?:[0-9一二两三四五六七八九十百千万零]+)?)(?:[:：\s　]+(.*?))?)[\s　]*$";

/// Maximum allowed length in characters for a chapter title line.
/// Headings longer than this are considered ordinary narrative sentences.
pub const MAX_CHAPTER_TITLE_LEN: usize = 40;

/// Represents a parsed chapter.
#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: Option<String>,
    pub paragraphs: Vec<String>,
}

/// Tests whether a line is a chapter heading. Returns the cleaned title if matched.
pub fn is_chapter_heading(line: &str, re: &Regex) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_CHAPTER_TITLE_LEN {
        return None;
    }
    if let Some(caps) = re.captures(trimmed) {
        let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let title = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let full = if title.trim().is_empty() {
            prefix.trim().to_string()
        } else {
            format!("{} {}", prefix.trim(), title.trim())
        };
        Some(full)
    } else {
        None
    }
}

/// Escapes XML special characters and strips invalid XML 1.0 control codes.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // In XML 1.0, only \t (0x9), \n (0xA), \r (0xD) and >= 0x20 are valid
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Converts a TXT file to a cached EPUB file in `cache_dir`.
/// If a valid cached version already exists, returns its path immediately.
pub fn cached_epub(src: &Path, cache_dir: &Path) -> Result<PathBuf, Error> {
    let meta = fs::metadata(src)
        .map_err(|e| format_err!("can't read metadata for {}: {e}", src.display()))?;
    let mut h = DefaultHasher::new();
    CONVERTER_VERSION.hash(&mut h);
    src.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .hash(&mut h);

    let stem: String = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .take(40)
        .collect();
    let stem_clean = stem.trim();
    let stem_name = if stem_clean.is_empty() {
        "book"
    } else {
        stem_clean
    };
    let out = cache_dir.join(format!("{}-{:016x}.epub", stem_name, h.finish()));
    if out.exists() {
        return Ok(out);
    }

    fs::create_dir_all(cache_dir)
        .map_err(|e| format_err!("can't create cache dir {}: {e}", cache_dir.display()))?;

    static CONVERSIONS: AtomicUsize = AtomicUsize::new(0);
    let n = CONVERSIONS.fetch_add(1, Ordering::Relaxed);
    let tmp = out.with_extension(format!("epub.{}-{n}.part", std::process::id()));

    convert_txt_to_epub(src, &tmp)?;
    fs::rename(&tmp, &out).map_err(|e| format_err!("failed to rename temporary EPUB: {e}"))?;
    Ok(out)
}

/// Converts `src` TXT file into uncompressed EPUB at `dst`.
pub fn convert_txt_to_epub(src: &Path, dst: &Path) -> Result<(), Error> {
    let bytes = fs::read(src).map_err(|e| format_err!("can't read {}: {e}", src.display()))?;
    let text = aristotle_library::encoding::decode_guess(&bytes);

    // Extract title and author heuristics
    let extracted = aristotle_library::txt::extract(src).unwrap_or_default();
    let title = if !extracted.meta.title.is_empty() {
        extracted.meta.title
    } else {
        src.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown".to_string())
    };
    let author = extracted
        .meta
        .authors
        .first()
        .map(|a| a.name.clone())
        .unwrap_or_default();

    let re = Regex::new(CHAPTER_PATTERN).map_err(|e| format_err!("invalid chapter regex: {e}"))?;

    let mut chapters: Vec<Chapter> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_paras: Vec<String> = Vec::new();
    let mut found_any_chapter = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(ch_title) = is_chapter_heading(trimmed, &re) {
            found_any_chapter = true;
            if current_title.is_some() || !current_paras.is_empty() {
                chapters.push(Chapter {
                    title: current_title.take(),
                    paragraphs: std::mem::take(&mut current_paras),
                });
            }
            current_title = Some(ch_title);
        } else {
            current_paras.push(trimmed.to_string());
        }
    }
    if current_title.is_some() || !current_paras.is_empty() {
        chapters.push(Chapter {
            title: current_title,
            paragraphs: current_paras,
        });
    }

    // If no chapters were found, chunk into groups of 500 paragraphs
    // so the EPUB spine has manageable XHTML files without creating false TOC entries.
    let chapters = if found_any_chapter {
        chapters
    } else {
        let all_paras = if chapters.is_empty() {
            Vec::new()
        } else {
            chapters.into_iter().flat_map(|c| c.paragraphs).collect()
        };
        if all_paras.is_empty() {
            vec![Chapter {
                title: None,
                paragraphs: Vec::new(),
            }]
        } else {
            all_paras
                .chunks(500)
                .map(|chunk| Chapter {
                    title: None,
                    paragraphs: chunk.to_vec(),
                })
                .collect()
        }
    };

    let file = File::create(dst).map_err(|e| format_err!("can't create {}: {e}", dst.display()))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    // 1. mimetype (must be first and uncompressed)
    zip.start_file("mimetype", opts)
        .map_err(|e| format_err!("{e}"))?;
    zip.write_all(b"application/epub+zip")
        .map_err(|e| format_err!("{e}"))?;

    // 2. META-INF/container.xml
    zip.start_file("META-INF/container.xml", opts)
        .map_err(|e| format_err!("{e}"))?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#,
    )
    .map_err(|e| format_err!("{e}"))?;

    // 3. Chapter XHTML files
    let mut manifest_items = String::new();
    let mut spine_items = String::new();
    let mut nav_points = String::new();
    let mut play_order = 1;

    for (idx, ch) in chapters.iter().enumerate() {
        let file_name = format!("chapter_{:04}.xhtml", idx);
        zip.start_file(&file_name, opts)
            .map_err(|e| format_err!("{e}"))?;

        manifest_items.push_str(&format!(
            r#"    <item id="c{idx}" href="{file_name}" media-type="application/xhtml+xml"/>
"#
        ));
        spine_items.push_str(&format!(
            r#"    <itemref idref="c{idx}"/>
"#
        ));

        if let Some(t) = &ch.title {
            nav_points.push_str(&format!(
                r#"    <navPoint id="np_{idx}" playOrder="{play_order}">
      <navLabel><text>{}</text></navLabel>
      <content src="{file_name}"/>
    </navPoint>
"#,
                xml_escape(t)
            ));
            play_order += 1;
        }

        let mut content = String::with_capacity(ch.paragraphs.len() * 128 + 256);
        content.push_str(
            r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh">
<head>
  <title>"#,
        );
        content.push_str(&xml_escape(ch.title.as_deref().unwrap_or(&title)));
        content.push_str("</title>\n</head>\n<body>\n");

        if let Some(t) = &ch.title {
            content.push_str("  <h2>");
            content.push_str(&xml_escape(t));
            content.push_str("</h2>\n");
        }

        for p in &ch.paragraphs {
            content.push_str("  <p>");
            content.push_str(&xml_escape(p));
            content.push_str("</p>\n");
        }

        content.push_str("</body>\n</html>\n");
        zip.write_all(content.as_bytes())
            .map_err(|e| format_err!("{e}"))?;
    }

    // 4. toc.ncx
    zip.start_file("toc.ncx", opts)
        .map_err(|e| format_err!("{e}"))?;
    let ncx = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="urn:uuid:txt-auto-convert"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle><text>{}</text></docTitle>
  <navMap>
{nav_points}  </navMap>
</ncx>
"#,
        xml_escape(&title)
    );
    zip.write_all(ncx.as_bytes())
        .map_err(|e| format_err!("{e}"))?;

    // 5. content.opf
    zip.start_file("content.opf", opts)
        .map_err(|e| format_err!("{e}"))?;
    let creator_elem = if !author.is_empty() {
        format!("    <dc:creator>{}</dc:creator>\n", xml_escape(&author))
    } else {
        String::new()
    };
    let opf = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:txt-auto-convert</dc:identifier>
    <dc:title>{}</dc:title>
    <dc:language>zh</dc:language>
{creator_elem}  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
{manifest_items}  </manifest>
  <spine toc="ncx">
{spine_items}  </spine>
</package>
"#,
        xml_escape(&title)
    );
    zip.write_all(opf.as_bytes())
        .map_err(|e| format_err!("{e}"))?;

    zip.finish().map_err(|e| format_err!("{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plato_core::document::Document;

    #[test]
    fn chapter_regex_matches_various_formats() {
        let re = Regex::new(CHAPTER_PATTERN).unwrap();

        assert_eq!(
            is_chapter_heading("第一章 序幕", &re),
            Some("第一章 序幕".to_string())
        );
        assert_eq!(
            is_chapter_heading("第10章 决战之巅", &re),
            Some("第10章 决战之巅".to_string())
        );
        assert_eq!(
            is_chapter_heading("第十二回 甄士隐梦幻识通灵", &re),
            Some("第十二回 甄士隐梦幻识通灵".to_string())
        );
        assert_eq!(
            is_chapter_heading("卷三 风云再起", &re),
            Some("卷三 风云再起".to_string())
        );
        assert_eq!(
            is_chapter_heading("第4节 讨论与结论", &re),
            Some("第4节 讨论与结论".to_string())
        );
        assert_eq!(is_chapter_heading("序章", &re), Some("序章".to_string()));
        assert_eq!(is_chapter_heading("楔子", &re), Some("楔子".to_string()));
        assert_eq!(is_chapter_heading("后记", &re), Some("后记".to_string()));
        assert_eq!(is_chapter_heading("尾声", &re), Some("尾声".to_string()));
        assert_eq!(
            is_chapter_heading("番外 那些年", &re),
            Some("番外 那些年".to_string())
        );
        assert_eq!(
            is_chapter_heading("番外2 往事如烟", &re),
            Some("番外2 往事如烟".to_string())
        );
        assert_eq!(
            is_chapter_heading("　　第一章：新的开始　", &re),
            Some("第一章 新的开始".to_string())
        );
    }

    #[test]
    fn ignores_overlong_and_empty_lines() {
        let re = Regex::new(CHAPTER_PATTERN).unwrap();

        assert_eq!(is_chapter_heading("", &re), None);
        assert_eq!(is_chapter_heading("   \t   ", &re), None);

        let overlong = format!("第一章 {}", "这是一段非常非常长的文字".repeat(10));
        assert!(overlong.chars().count() > MAX_CHAPTER_TITLE_LEN);
        assert_eq!(is_chapter_heading(&overlong, &re), None);

        // Ordinary sentence that happens to contain "第一章"
        assert_eq!(
            is_chapter_heading("他在第一章写道：人生若只如初见。", &re),
            None
        );
    }

    #[test]
    fn xml_escape_handles_entities_and_controls() {
        assert_eq!(
            xml_escape("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
        assert_eq!(
            xml_escape("valid\t\n\rtext\x00\x08null"),
            "valid\t\n\rtextnull"
        );
    }

    #[test]
    fn converts_and_caches_epub() {
        let temp = std::env::temp_dir().join("txt_test_convert_epub");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("test_book.txt");
        let content = "《测试小说》\n作者：测试者\n\n第一章 起步\n这是第一章的正文。\n\n第二章 发展\n这是第二章的内容。";
        fs::write(&src, content.as_bytes()).unwrap();

        let cache = temp.join("cache");
        let epub = cached_epub(&src, &cache).unwrap();
        assert!(epub.exists());

        // Read the generated EPUB using plato_core::document::epub::EpubDocument
        let doc = plato_core::document::epub::EpubDocument::new(&epub).unwrap();
        assert_eq!(doc.title().as_deref(), Some("测试小说"));
        assert_eq!(doc.author().as_deref(), Some("测试者"));

        // Second call should hit cache
        let epub_cached = cached_epub(&src, &cache).unwrap();
        assert_eq!(epub, epub_cached);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn plain_text_without_chapters_has_empty_nav_points() {
        let temp = std::env::temp_dir().join("txt_test_no_chapters_epub");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("plain.txt");
        let content = "这是纯文本短篇。\n第二段落没有章节标记。\n第三段落。";
        fs::write(&src, content.as_bytes()).unwrap();

        let cache = temp.join("cache");
        let epub = cached_epub(&src, &cache).unwrap();
        let mut doc = plato_core::document::epub::EpubDocument::new(&epub).unwrap();
        let toc = doc.toc().unwrap_or_default();
        assert!(toc.is_empty(), "TOC should be empty when no chapters exist");

        let _ = fs::remove_dir_all(&temp);
    }
}
