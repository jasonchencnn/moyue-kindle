// SPDX-License-Identifier: AGPL-3.0-or-later
//! Metadata extraction for plain text files.
//!
//! TXT files have no standard metadata container, but many Chinese ebooks
//! contain header lines with title and author (e.g. `书名：...`, `作者：...`,
//! `《...》`). We inspect the first few kilobytes to extract these heuristics,
//! and fall back to the filename stem or directory name.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::encoding;
use crate::error::Result;
use crate::metadata::{Author, Extracted, Metadata, TitleSource};

/// Extracts title and author from the initial portion of a plain text file.
pub fn extract(path: &Path) -> Result<Extracted> {
    let mut file = File::open(path)?;
    // Read up to 8 KiB of header for metadata heuristics.
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf)?;
    let head_bytes = &buf[..n];

    let text = encoding::decode_guess(head_bytes);
    let mut meta = Metadata::default();

    // Inspect first 30 non-empty lines for metadata conventions.
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(30)
    {
        if meta.title.is_empty() {
            if let Some(title) = parse_field(line, &["书名", "書名", "title"]) {
                meta.title = clean_title(title);
                meta.title_source = TitleSource::Embedded;
            } else if line.starts_with('《') && line.contains('》') {
                if let Some(end) = line.find('》') {
                    let title = &line['《'.len_utf8()..end];
                    if !title.trim().is_empty() {
                        meta.title = clean_title(title);
                        meta.title_source = TitleSource::Embedded;
                    }
                }
            }
        }
        if meta.authors.is_empty() {
            if let Some(author) = parse_field(line, &["作者", "著", "author", "by"]) {
                let a = clean_author(author);
                if !a.is_empty() {
                    meta.authors.push(Author::new(&a, None));
                }
            }
        }
    }

    Ok(Extracted {
        meta,
        cover: None,
        dictionary: false,
    })
}

fn parse_field<'a>(line: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for &prefix in prefixes {
        if let Some(rest) = line.strip_prefix(prefix) {
            let trimmed = rest.trim_start();
            if let Some(rest2) = trimmed
                .strip_prefix(':')
                .or_else(|| trimmed.strip_prefix('：'))
            {
                return Some(rest2.trim());
            }
        }
    }
    None
}

fn clean_title(s: &str) -> String {
    s.trim_matches(|c: char| c == '《' || c == '》' || c.is_whitespace())
        .to_string()
}

fn clean_author(s: &str) -> String {
    s.trim_matches(|c: char| {
        c.is_whitespace() || c == ':' || c == '：' || c == '[' || c == ']' || c == '【' || c == '】'
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_chinese_metadata_utf8() {
        let dir = crate::testdir("txt-meta-utf8");
        let path = dir.join("book.txt");
        let content = "《红楼梦》\n作者：曹雪芹\n\n第一回 甄士隐梦幻识通灵";
        std::fs::write(&path, content.as_bytes()).unwrap();

        let ext = extract(&path).unwrap();
        assert_eq!(ext.meta.title, "红楼梦");
        assert_eq!(ext.meta.title_source, TitleSource::Embedded);
        assert_eq!(ext.meta.authors.len(), 1);
        assert_eq!(ext.meta.authors[0].name, "曹雪芹");
    }

    #[test]
    fn extracts_chinese_metadata_gbk() {
        let dir = crate::testdir("txt-meta-gbk");
        let path = dir.join("novel.txt");
        let content = "书名：三国演义\n作者：罗贯中\n\n第一回 宴桃园豪杰三结义";
        let (encoded, _, _) = encoding_rs::GBK.encode(content);
        std::fs::write(&path, encoded).unwrap();

        let ext = extract(&path).unwrap();
        assert_eq!(ext.meta.title, "三国演义");
        assert_eq!(ext.meta.title_source, TitleSource::Embedded);
        assert_eq!(ext.meta.authors.len(), 1);
        assert_eq!(ext.meta.authors[0].name, "罗贯中");
    }

    #[test]
    fn fallback_when_no_header_metadata() {
        let dir = crate::testdir("txt-meta-none");
        let path = dir.join("plain.txt");
        let content = "纯文本内容，无标题作者声明。\n第二行正文。";
        std::fs::write(&path, content.as_bytes()).unwrap();

        let ext = extract(&path).unwrap();
        assert!(ext.meta.title.is_empty());
        assert!(ext.meta.authors.is_empty());
    }
}
