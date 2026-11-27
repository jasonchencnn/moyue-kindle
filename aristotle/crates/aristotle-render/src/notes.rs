// SPDX-License-Identifier: AGPL-3.0-or-later
//! Footnotes: what a tapped link points at, as text for a popup.
//!
//! Plato's engine keeps only a link's `href`, so the note is found again in
//! the book's source: the target file is parsed with Plato's XML parser,
//! the element with the fragment's id is located, and the note's text is
//! taken from it ([`note_text`]). Whether a link is a footnote at all
//! ([`is_footnote`]) is decided from the markup around both ends:
//!
//! - EPUB 3 and DPUB-ARIA semantics: `epub:type="noteref"` or
//!   `role="doc-noteref"` on the link, `footnote`, `endnote`, `rearnote`
//!   or `note` (`doc-footnote`, `doc-endnote`) on the target or an
//!   ancestor;
//! - classes and ids that publishers and converters use (`footnote`,
//!   `fn`, `note`, `przypis`, calibre's `filepos12345`);
//! - the link text: a short marker such as `1`, `[12]`, `iv`, `*` or `†`,
//!   usually set as a superscript.
//!
//! Anything else (a cross reference, a table of contents entry) is a plain
//! link the reader follows.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use plato_core::document::html::dom::NodeRef;
use plato_core::document::html::xml::XmlParser;
use plato_core::helpers::Normalize;
use zip::ZipArchive;

use crate::richtext::{self, Block, RichText};

/// Notes longer than this are cut (with an ellipsis): a popup is for a
/// note, not for a chapter that a link happened to point at.
const MAX_NOTE: usize = 2400;

/// A link split into the archive path it points at and its fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: String,
    pub fragment: Option<String>,
}

/// Resolve `href` (as written in the book) against the spine item `base`.
/// `None` for external links.
pub fn target(base: &str, href: &str) -> Option<Target> {
    let lower = href.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("mailto:") || lower.starts_with("tel:") {
        return None;
    }
    let (file, fragment) = match href.split_once('#') {
        Some((f, frag)) => (f, Some(frag.to_string()).filter(|s| !s.is_empty())),
        None => (href, None),
    };
    let path = if file.is_empty() {
        base.to_string()
    } else {
        let parent = Path::new(base).parent().unwrap_or_else(|| Path::new(""));
        parent.join(file).normalize().to_string_lossy().into_owned()
    };
    Some(Target { path, fragment })
}

/// Publisher CSS locks detected inside an EPUB book's stylesheets.
///
/// When fine typesetting (`fine_typesetting = true`) is active, crengine
/// honours the book's own CSS rules over default reader settings:
/// - `has_embedded_fonts`: the book declares `@font-face` rules so when
///   `embedded_fonts = true`, the book's built-in fonts override the reader's
///   font picker.
/// - `locks_line_height`: the book's CSS sets an explicit `line-height` on
///   body/paragraph selectors, which overrides `setDefaultInterlineSpace`
///   while fine typesetting is enabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FineBookLocks {
    pub has_embedded_fonts: bool,
    pub locks_line_height: bool,
}

/// Strip `/* ... */` comments from CSS text before inspecting rules.
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let bytes = css.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            let ch = css[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Whether a CSS selector targets main body text (`body`, `html`, `p`, `*`,
/// `p.class`, `.body`, `.content`, `.paragraph`, `.maintext`, etc.) rather
/// than isolated headings/captions/footnotes.
fn selector_targets_body(selector_list: &str) -> bool {
    for sel in selector_list.split(',') {
        let sel = sel.trim().to_ascii_lowercase();
        if sel.is_empty() || sel.starts_with('@') {
            continue;
        }
        // Take the last compound selector after combinators.
        let last = sel
            .split(|c: char| c.is_whitespace() || matches!(c, '>' | '+' | '~'))
            .rfind(|s| !s.is_empty())
            .unwrap_or("");
        if matches!(last, "body" | "html" | "p" | "*") {
            return true;
        }
        if last.starts_with("p.") || last.starts_with("body.") {
            // Exclude obvious non-body paragraph classes like caption/title/footnote/quote.
            let cls = last.split_once('.').map(|(_, c)| c).unwrap_or("");
            if !cls.contains("title")
                && !cls.contains("caption")
                && !cls.contains("footnote")
                && !cls.contains("note")
                && !cls.contains("header")
                && !cls.contains("footer")
                && !cls.contains("copyright")
                && !cls.contains("epigraph")
                && !cls.contains("poem")
                && !cls.contains("poetry")
                && !cls.contains("quote")
                && !cls.contains("pic")
                && !cls.contains("img")
            {
                return true;
            }
        }
    }
    false
}

/// Whether a CSS declaration block sets an explicit (non-`normal`/`inherit`)
/// `line-height` value.
fn block_locks_line_height(block: &str) -> bool {
    for decl in block.split(';') {
        let Some((prop, val)) = decl.split_once(':') else {
            continue;
        };
        if prop.trim().eq_ignore_ascii_case("line-height") {
            let v = val
                .split("!important")
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if !v.is_empty() && v != "normal" && v != "inherit" && v != "initial" && v != "unset" {
                return true;
            }
        }
    }
    false
}

/// Inspect CSS text for `@font-face` declarations and body `line-height` locks.
pub fn inspect_css_fine_locks(css: &str) -> FineBookLocks {
    let cleaned = strip_css_comments(css);
    let lower = cleaned.to_ascii_lowercase();
    let mut locks = FineBookLocks {
        has_embedded_fonts: lower.contains("@font-face"),
        locks_line_height: false,
    };

    // Walk top-level selector { declarations } blocks.
    let mut rest = cleaned.as_str();
    while let Some(open) = rest.find('{') {
        let selector = rest[..open].trim();
        let after_open = &rest[open + 1..];
        // Handle nested braces (e.g. @media { ... }) by skipping to matching close.
        let mut depth = 1usize;
        let mut close_idx = None;
        for (idx, ch) in after_open.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close_idx = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close_idx else {
            break;
        };
        let body = &after_open[..close];
        if !locks.locks_line_height
            && selector_targets_body(selector)
            && block_locks_line_height(body)
        {
            locks.locks_line_height = true;
        }
        rest = &after_open[close + 1..];
        if locks.has_embedded_fonts && locks.locks_line_height {
            break;
        }
    }
    locks
}

/// Inspect an EPUB archive's stylesheets for publisher fine-typesetting locks
/// (`@font-face` and body `line-height`). Returns defaults for non-EPUB files.
pub fn inspect_fine_locks(epub: &Path) -> FineBookLocks {
    let Ok(file) = File::open(epub) else {
        return FineBookLocks::default();
    };
    let Ok(mut zip) = ZipArchive::new(file) else {
        return FineBookLocks::default();
    };
    let mut locks = FineBookLocks::default();
    let mut bytes_budget: usize = 512 * 1024;
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let Ok(raw_name) = entry.name() else {
            continue;
        };
        let name = raw_name.to_ascii_lowercase();
        if !name.ends_with(".css") {
            continue;
        }
        let size = entry.size() as usize;
        if size == 0 || size > bytes_budget {
            continue;
        }
        let mut css = String::new();
        if entry.read_to_string(&mut css).is_ok() {
            bytes_budget = bytes_budget.saturating_sub(css.len());
            let part = inspect_css_fine_locks(&css);
            locks.has_embedded_fonts |= part.has_embedded_fonts;
            locks.locks_line_height |= part.locks_line_height;
            if locks.has_embedded_fonts && locks.locks_line_height {
                break;
            }
        }
    }
    locks
}

/// Reads members of an EPUB, keeping the last few files parsed (a chapter
/// and its separate notes file can stay cached together).
pub struct Source {
    zip: ZipArchive<File>,
    cached: Vec<(String, String)>,
}

impl Source {
    pub fn open(epub: &Path) -> Option<Source> {
        let zip = ZipArchive::new(File::open(epub).ok()?).ok()?;
        Some(Source {
            zip,
            cached: Vec::new(),
        })
    }

    fn text(&mut self, path: &str) -> Option<&str> {
        if let Some(pos) = self.cached.iter().position(|(p, _)| p == path) {
            return Some(self.cached[pos].1.as_str());
        }
        let mut s = String::new();
        self.zip.by_name(path).ok()?.read_to_string(&mut s).ok()?;
        if self.cached.len() >= 4 {
            self.cached.remove(0);
        }
        self.cached.push((path.to_string(), s));
        self.cached.last().map(|(_, s)| s.as_str())
    }

    /// Byte offset of the element with id `id` in the file `path`, the way
    /// Plato counts locations inside a spine item.
    pub fn id_offset(&mut self, path: &str, id: &str) -> Option<usize> {
        let src = self.text(path)?;
        let tree = XmlParser::new(src).parse();
        let node = tree.root().find_by_id(id)?;
        Some(node.offset())
    }

    /// The footnote behind a link in spine item `base`: its text, if the
    /// link is a footnote. `link_text` is what the link shows (the marker).
    pub fn footnote(&mut self, base: &str, href: &str, link_text: &str) -> Option<RichText> {
        self.footnote_with_marker(base, href, link_text)
            .map(|(_, text)| text)
    }

    /// Like [`Source::footnote`], also returning the resolved marker text
    /// (falling back to the `<a href>` text/alt in the source XHTML when
    /// `link_text` is empty).
    pub fn footnote_with_marker(
        &mut self,
        base: &str,
        href: &str,
        link_text: &str,
    ) -> Option<(String, RichText)> {
        let t = target(base, href)?;
        let frag = t.fragment.as_deref()?;
        self.footnote_target_with_marker(Some(base), &t.path, frag, link_text)
    }

    /// The footnote behind a link pointing to `target_path` with fragment `frag`.
    pub fn footnote_target(
        &mut self,
        source_base: Option<&str>,
        target_path: &str,
        frag: &str,
        link_text: &str,
    ) -> Option<RichText> {
        self.footnote_target_with_marker(source_base, target_path, frag, link_text)
            .map(|(_, text)| text)
    }

    /// Like [`Source::footnote_target`], also returning the resolved marker
    /// text from `link_text` or the source `<a href>` element.
    pub fn footnote_target_with_marker(
        &mut self,
        source_base: Option<&str>,
        target_path: &str,
        frag: &str,
        link_text: &str,
    ) -> Option<(String, RichText)> {
        let (link_semantics, xml_marker) = source_base
            .and_then(|base| self.text(base))
            .map(|src| {
                let tree = XmlParser::new(src).parse();
                let found = tree
                    .root()
                    .descendants()
                    .find(|n| {
                        n.tag_name() == Some("a")
                            && n.attribute("href")
                                .is_some_and(|h| h.ends_with(&format!("#{frag}")))
                    })
                    .map(|a| {
                        let text = a.text().trim().to_string();
                        let marker = if !text.is_empty() {
                            text
                        } else {
                            a.descendants()
                                .find(|c| c.tag_name() == Some("img"))
                                .and_then(|img| img.attribute("alt"))
                                .map(|alt| alt.trim().to_string())
                                .unwrap_or_default()
                        };
                        (noteref_markup(a), marker)
                    });
                found.unwrap_or((false, String::new()))
            })
            .unwrap_or((false, String::new()));
        let effective_marker = if link_text.trim().is_empty() {
            xml_marker
        } else {
            link_text.trim().to_string()
        };
        let src = self.text(target_path)?;
        let tree = XmlParser::new(src).parse();
        let node = tree.root().find_by_id(frag)?;
        let text = note_text(node);
        if text.is_empty() {
            return None;
        }
        let note_like = link_semantics
            || is_marker(&effective_marker)
            || note_markup(node)
            || note_id(frag)
            || node.ancestor_elements().any(note_markup);
        note_like.then_some((effective_marker, text))
    }
}

fn has_word(value: Option<&str>, words: &[&str]) -> bool {
    value.is_some_and(|v| {
        v.split(|c: char| c.is_whitespace() || c == ',')
            .any(|w| words.contains(&w.to_ascii_lowercase().as_str()))
    })
}

/// `epub:type="noteref"` or publisher-specific footnote link markup
/// (including Duokan `duokan-footnote`, WeRead `qqreader-footnote`/`wr-footnote`,
/// iReader/Zhangyue, and image-icon note links like `<a ...><img alt="注"/></a>`).
fn noteref_markup(a: NodeRef) -> bool {
    has_word(a.attribute("epub:type"), &["noteref"])
        || has_word(a.attribute("role"), &["doc-noteref"])
        || a.classes().any(|c| {
            let c = c.to_ascii_lowercase();
            c.contains("noteref")
                || c.contains("note-ref")
                || c.contains("footnote")
                || c.contains("fnref")
                || c.contains("zhushi")
                || c == "zhu"
        })
        || a.descendants().any(|child| {
            child.tag_name() == Some("img")
                && child.attribute("alt").is_some_and(|alt| {
                    let t = alt.trim();
                    t == "注" || t == "註" || t.eq_ignore_ascii_case("note") || is_marker(t)
                })
        })
}

/// Footnote semantics on a target element.
fn note_markup(n: NodeRef) -> bool {
    has_word(
        n.attribute("epub:type"),
        &[
            "footnote",
            "endnote",
            "rearnote",
            "note",
            "footnotes",
            "endnotes",
            "rearnotes",
        ],
    ) || has_word(
        n.attribute("role"),
        &["doc-footnote", "doc-endnote", "doc-endnotes"],
    ) || n.classes().any(|c| {
        let c = c.to_ascii_lowercase();
        c.contains("footnote")
            || c.contains("endnote")
            || c.contains("przypis")
            || c.contains("zhushi")
            || c == "zhu"
            || c == "annotation"
    })
}

/// Ids publishers and converters give notes: fn3, ftn3, note-3, footnote3,
/// przypis3, calibre's filepos12345, Word's _ftn3, Duokan's df-1/duokan-note-1,
/// WeRead's reader_note_1, Chinese publisher zhu1/zhushi-1.
pub fn note_id(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let id = id.trim_start_matches('_');
    [
        "fn",
        "ftn",
        "note",
        "footnote",
        "endnote",
        "przyp",
        "przypis",
        "filepos",
        "en",
        "nt",
        "df",
        "duokan-note",
        "reader_note",
        "wrnote",
        "zhushi",
        "zhu",
        "annotation",
    ]
    .iter()
    .any(|p| {
        id.strip_prefix(p).is_some_and(|rest| {
            let rest = rest.trim_start_matches(['-', '_', ':', '.']);
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '-' || c == '_')
        })
    })
}

/// A footnote marker: 1, [12], (3), 〔1〕, 【注】, ①, iv, *, †, ※, a.
pub fn is_marker(text: &str) -> bool {
    let t = text
        .trim()
        .trim_matches(|c: char| "[]()<>{}.〔〕【】（）「」『』".contains(c))
        .trim();
    if t.is_empty() || t.chars().count() > 4 {
        return false;
    }
    let zh_stripped = t.strip_prefix('注').or_else(|| t.strip_prefix('註'));
    if let Some(rest) = zh_stripped {
        return rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit());
    }
    t.chars().all(|c| c.is_ascii_digit())
        || t.chars().all(|c| "*†‡§¶#※＊".contains(c))
        || t.chars().all(|c| ('①'..='⑳').contains(&c))
        || t.chars().all(|c| "ivxlcIVXLC".contains(c))
        || (t.chars().count() == 1 && t.chars().all(|c| c.is_alphabetic()))
        || t.chars().all(|c| "⁰¹²³⁴⁵⁶⁷⁸⁹".contains(c))
}

fn is_block(n: &NodeRef) -> bool {
    matches!(
        n.tag_name(),
        Some(
            "p" | "li"
                | "aside"
                | "div"
                | "dd"
                | "dt"
                | "section"
                | "td"
                | "blockquote"
                | "body"
                | "ol"
                | "ul"
                | "dl"
        )
    )
}

fn visible_len(n: NodeRef) -> usize {
    n.text().split_whitespace().map(str::len).sum()
}

/// The text of the note whose anchor is `node`.
///
/// - A block with text of its own (`<aside id>`, `<p id>`, `<li id>`) is
///   the note.
/// - An empty or tiny anchor (`<a id="fn1"/>`, `<sup id><a>1</a></sup>`,
///   calibre's `<a id="filepos…">`) stands at the start of its note: the
///   note is the enclosing paragraph, or, when the enclosing block holds
///   many notes, the siblings after the anchor up to the next anchor.
pub fn note_text(node: NodeRef) -> RichText {
    let own = visible_len(node);
    let text = if is_block(&node) && own > 6 && node.tag_name() != Some("body") {
        richtext::from_node(node)
    } else {
        // Climb to the block that holds the anchor.
        let mut anchor = node;
        let mut block = node.parent_element();
        while let Some(b) = block {
            if is_block(&b) {
                break;
            }
            anchor = b;
            block = b.parent_element();
        }
        // A block holding several anchors holds several notes.
        let anchors = |b: NodeRef| b.descendants().filter(|n| n.id().is_some()).count();
        match block {
            Some(b)
                if visible_len(b) <= MAX_NOTE && b.tag_name() != Some("body") && anchors(b) < 2 =>
            {
                richtext::from_node(b)
            }
            Some(_) => siblings_after(anchor),
            None => richtext::from_node(node),
        }
    };
    cut(text)
}

/// The anchor and what follows it in its parent, up to the next element
/// that carries an id (the next note's anchor).
fn siblings_after(anchor: NodeRef) -> RichText {
    let mut out = RichText::default();
    let mut first = true;
    let mut n = Some(anchor);
    while let Some(node) = n {
        if !first && node.id().is_some() && node.is_element() {
            break;
        }
        first = false;
        out.blocks.extend(richtext::from_node(node).blocks);
        if out.len() > MAX_NOTE {
            break;
        }
        n = node.next_sibling();
    }
    // Inline siblings come out as separate blocks; join them.
    if out.blocks.len() > 1 {
        let mut joined = Block::default();
        for b in out.blocks {
            if !joined.runs.is_empty() {
                if let Some(r) = joined.runs.last_mut() {
                    r.text.push(' ');
                }
            }
            joined.runs.extend(b.runs);
        }
        out.blocks = vec![joined];
    }
    out
}

fn cut(mut text: RichText) -> RichText {
    let mut total = 0;
    let mut keep = 0;
    for b in &text.blocks {
        total += b.text().len();
        keep += 1;
        if total > MAX_NOTE {
            break;
        }
    }
    if keep < text.blocks.len() || total > MAX_NOTE {
        text.blocks.truncate(keep);
        if let Some(r) = text.blocks.last_mut().and_then(|b| b.runs.last_mut()) {
            r.text.push_str(" …");
        }
    }
    // Back links ("↩", "^") at the end of a note mean nothing in a popup.
    if let Some(b) = text.blocks.last_mut() {
        if let Some(r) = b.runs.last_mut() {
            let t = r
                .text
                .trim_end_matches(['↩', '↑', '^', '\u{FE0E}', ' '])
                .len();
            r.text.truncate(t);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(xhtml: &str, id: &str) -> String {
        let tree = XmlParser::new(xhtml).parse();
        let node = tree.root().find_by_id(id).expect("id");
        note_text(node).text()
    }

    #[test]
    fn targets() {
        assert_eq!(
            target("OEBPS/text/ch1.xhtml", "../notes.xhtml#fn3"),
            Some(Target {
                path: "OEBPS/notes.xhtml".into(),
                fragment: Some("fn3".into())
            })
        );
        assert_eq!(
            target("OEBPS/ch1.xhtml", "#n1").unwrap().path,
            "OEBPS/ch1.xhtml"
        );
        assert_eq!(target("a.xhtml", "https://pl.wikipedia.org/#x"), None);
    }

    #[test]
    fn markers_and_ids() {
        for m in ["1", "[12]", "(3)", "iv", "*", "†", "a", "²"] {
            assert!(is_marker(m), "{m}");
        }
        for m in ["Rozdział", "12345", "zob. dalej"] {
            assert!(!is_marker(m), "{m}");
        }
        for id in [
            "fn3",
            "ftn-3",
            "note_12",
            "footnote1",
            "przypis4",
            "filepos12345",
            "_ftn2",
        ] {
            assert!(note_id(id), "{id}");
        }
        for id in ["chapter3", "fnord", "toc", "note"] {
            assert!(!note_id(id), "{id}");
        }
    }

    #[test]
    fn epub3_aside() {
        let x = r##"<html><body><p>Tekst<a epub:type="noteref" href="#n1">1</a>.</p>
            <aside epub:type="footnote" id="n1"><p>Zob. <i>Fajdros</i> 245c.</p></aside></body></html>"##;
        assert_eq!(note(x, "n1"), "Zob. Fajdros 245c.");
    }

    #[test]
    fn calibre_empty_anchor_in_a_paragraph() {
        let x = r#"<html><body><div class="notes">
            <p class="calibre1"><a id="filepos1234"></a><sup>1</sup> Platon, <i>Fedon</i>, 64a.</p>
            <p class="calibre1"><a id="filepos1300"></a><sup>2</sup> Tamże.</p></div></body></html>"#;
        assert_eq!(note(x, "filepos1234"), "1 Platon, Fedon, 64a.");
        assert_eq!(note(x, "filepos1300"), "2 Tamże.");
    }

    #[test]
    fn endnote_list_items_and_backlinks() {
        let x = r#"<html><body><ol><li id="fn1"><p>Pierwsza uwaga. <a href="ch1.xhtml#r1">↩</a></p></li>
            <li id="fn2"><p>Druga.</p></li></ol></body></html>"#;
        assert_eq!(note(x, "fn1"), "Pierwsza uwaga.");
    }

    #[test]
    fn duokan_and_chinese_refined_footnotes() {
        for m in ["①", "⑫", "〔1〕", "【注】", "注1", "※"] {
            assert!(is_marker(m), "{m}");
        }
        for id in ["df-1", "df12", "zhushi-3", "zhu_2", "reader_note_5"] {
            assert!(note_id(id), "{id}");
        }
        let x = r##"<html><body>
            <p>正文内容<a class="duokan-footnote" href="#df-1"><img src="../Images/note.png" alt="注"/></a>。</p>
            <ol class="duokan-footnote-content">
                <li class="duokan-footnote-item" id="df-1"><p>多看富文本注释：支持<b>加粗</b>与多段。</p></li>
            </ol>
        </body></html>"##;
        let tree = XmlParser::new(x).parse();
        let link = tree
            .root()
            .descendants()
            .find(|n| n.tag_name() == Some("a"))
            .expect("duokan link");
        assert!(noteref_markup(link));
        let item = tree.root().find_by_id("df-1").expect("df-1");
        assert!(note_markup(item));
        assert_eq!(note_text(item).text(), "多看富文本注释：支持加粗与多段。");
    }

    #[test]
    fn many_notes_in_one_block() {
        // Anchors and text as siblings directly in one big block.
        let filler = "x ".repeat(1500);
        let x = format!(
            r#"<html><body><div><span id="fn1">1</span> Pierwsza. <span id="fn2">2</span> Druga. {filler}</div></body></html>"#
        );
        assert_eq!(note(&x, "fn1"), "1 Pierwsza.");
    }

    #[test]
    fn inspect_css_fine_locks_detects_font_face_and_body_line_height() {
        let locks = inspect_css_fine_locks(
            r#"
            /* line-height: 2.0; commented out */
            @font-face {
                font-family: "FZShuSong";
                src: url("../Fonts/fz.ttf");
            }
            h1 { line-height: 1.2; }
            p.caption { line-height: 1.3; }
            "#,
        );
        assert!(locks.has_embedded_fonts);
        assert!(
            !locks.locks_line_height,
            "heading/caption line-height should not lock body spacing"
        );

        let locks_body = inspect_css_fine_locks(
            r#"
            body { font-size: 1em; line-height: 1.65; }
            "#,
        );
        assert!(!locks_body.has_embedded_fonts);
        assert!(locks_body.locks_line_height);

        let locks_p = inspect_css_fine_locks(
            r#"
            p { line-height: 1.8 !important; }
            "#,
        );
        assert!(locks_p.locks_line_height);

        let locks_normal = inspect_css_fine_locks(
            r#"
            body { line-height: normal; }
            p { line-height: inherit; }
            "#,
        );
        assert!(!locks_normal.locks_line_height);
    }
}
