// SPDX-License-Identifier: AGPL-3.0-or-later
//! The vertical layout engine: top to bottom, right to left.
//!
//! crengine-ng has no `writing-mode: vertical-rl`, so a classical Chinese
//! book set vertically needs an engine of its own. This one is pure Rust: it
//! reads an EPUB (the OPF spine, the navigation document and the XHTML
//! paragraphs) or a TXT file (reusing [`crate::txt`]'s encoding detection and
//! chapter split), lays the text out in columns that run from the right edge
//! of the text block to its left edge, and draws the result with the FreeType
//! rasteriser Plato already links (see [`crate::page`]).
//!
//! # The layout
//!
//! A page is a pure function of the character offset it starts at and of the
//! [`VerticalStyle`]: [`Document::layout_page`] lays one page out from that
//! offset and reports where the next page starts, so pagination is a chain of
//! page layouts ([`Document::next_page`]) and nothing but the page starts is
//! ever stored. A reading position is a character offset into the flattened
//! book, which makes a horizontal↔vertical switch keep its place by fraction
//! ([`Document::fraction`]).
//!
//! # The typography
//!
//! - Columns run from `width - margin_right` to `margin_left`; within a
//!   column, characters run from `margin_top` to `height - margin_bottom`.
//! - The column pitch is [`Style::line_height`] ems, so the panel's line
//!   spacing setting keeps its meaning.
//! - [`Style::cjk_indent`] indents the first column of every paragraph.
//! - A chapter starts a column of its own with a blank column before it.
//! - Punctuation that has a Unicode vertical presentation form is drawn in
//!   that form ([`vertical_form`]); when the font lacks the glyph the
//!   original mark is rotated 90° clockwise instead.
//! - `。，、！？；：` and the closing brackets may not start a column
//!   (避头禁则), and the opening brackets may not end one (避尾禁则): the
//!   break moves a character down to the next column instead.
//! - The terminal marks sit at the top right of their cell, as in a
//!   traditional vertical setting.
//! - Latin letters and digits are rotated 90° clockwise.
//! - The reading guide ([`Underline`]) becomes a vertical rule on the left
//!   side of each column, in [`UNDERLINE_GRAY`]; [`UnderlineScope::FullLine`]
//!   rules the empty column slots as well, like the ruled paper (乌丝栏) of a
//!   woodblock edition.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use plato_core::anyhow::{format_err, Error};
use plato_core::color::Color;
use plato_core::font::{Font, FontOpener, RenderPlan};
use plato_core::framebuffer::Framebuffer;
use plato_core::geom::Point;
use regex::Regex;

use crate::page::Page;
use crate::richtext;
use crate::style::{Style, Underline, UnderlineScope, UNDERLINE_GRAY};
use crate::txt::{is_chapter_heading, CHAPTER_PATTERN};

// -----------------------------------------------------------------------------
// Characters: width, vertical forms, and the line-start prohibitions
// -----------------------------------------------------------------------------

/// The vertical presentation form of `c`, when Unicode has one for it.
///
/// The forms are the ones a font draws upright inside a column: the brackets
/// and quotation marks open upwards and close downwards, the em dash becomes
/// a vertical stroke, and the ellipsis a vertical row of dots.
pub fn vertical_form(c: char) -> Option<char> {
    Some(match c {
        '（' => '︵',
        '）' => '︶',
        '《' => '︽',
        '》' => '︾',
        '〈' => '︿',
        '〉' => '﹀',
        '「' => '﹁',
        '」' => '﹂',
        '『' => '﹃',
        '』' => '﹄',
        '【' => '︻',
        '】' => '︼',
        '—' => '︱',
        '…' => '︙',
        _ => return None,
    })
}

/// Whether `c` takes a full em in a column: the CJK scripts, the vertical
/// forms, the fullwidth forms and the compatibility ideographs.
pub fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE10..=0xFE19
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD)
}

/// Punctuation that may not start a column (避头): the terminal marks and the
/// closing brackets. A break that would leave one of them at the top of a
/// column moves the character before it down as well.
pub fn is_no_start(c: char) -> bool {
    matches!(
        c,
        '。' | '，'
            | '、'
            | '！'
            | '？'
            | '；'
            | '：'
            | '．'
            | '·'
            | '～'
            | '）'
            | '〕'
            | '〉'
            | '》'
            | '」'
            | '』'
            | '】'
            | '〗'
            | '｝'
            | '…'
            | '—'
            | '︶'
            | '︾'
            | '﹀'
            | '﹂'
            | '﹄'
            | '︼'
            | '︙'
            | '︱'
    )
}

/// Punctuation that may not end a column (避尾): the opening brackets. A
/// break that would leave one of them at the bottom of a column moves it down
/// to the next one.
pub fn is_no_end(c: char) -> bool {
    matches!(
        c,
        '（' | '〔'
            | '〈'
            | '《'
            | '「'
            | '『'
            | '【'
            | '〖'
            | '｛'
            | '︵'
            | '︽'
            | '︿'
            | '﹁'
            | '﹃'
            | '︻'
    )
}

/// How a glyph is drawn in its cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// Drawn as the font draws it, in the vertical form when it has one.
    Upright,
    /// Drawn 90° clockwise: the Latin letters and the digits, and the
    /// punctuation whose vertical form the font lacks.
    Rotated,
}

/// The cell height of `c`: a full em for the wide scripts, half an em for the
/// Latin letters and the digits, which are rotated and advance by their
/// horizontal width.
pub fn cell_advance(c: char, em: i32) -> i32 {
    let (ch, _) = glyph_for(c);
    if is_wide(ch) {
        em
    } else {
        (em + 1) / 2
    }
}

/// The character drawn for `source` and how it is drawn: the vertical form
/// upright when Unicode has one, the character itself otherwise, rotated
/// when it is narrow.
pub fn glyph_for(source: char) -> (char, Orientation) {
    if let Some(form) = vertical_form(source) {
        (form, Orientation::Upright)
    } else if is_wide(source) {
        (source, Orientation::Upright)
    } else {
        (source, Orientation::Rotated)
    }
}

/// Whether `c` is a vertical bracket form, which the fonts draw on the
/// baseline rather than centred in the em box.
///
/// A vertical bracket is a wide arc across the column; left where the font
/// draws it, it leans towards the character below it instead of sitting
/// between the two it encloses.
fn is_bracket_form(c: char) -> bool {
    matches!(
        c,
        '︵' | '︶' | '︽' | '︾' | '︿' | '﹀' | '﹁' | '﹂' | '﹃' | '﹄' | '︻' | '︼'
    )
}

/// Whether the punctuation is drawn at the top right of its cell rather than
/// in the middle, as a vertical setting does.
///
/// It is the marks the fonts draw in the bottom left of their em box: moving
/// them half an em right and up swaps that corner for the top right one. The
/// full-height marks (`！？：；`) stay centred, or they would spill into the
/// neighbouring columns.
fn is_shifted(c: char) -> bool {
    matches!(c, '。' | '，' | '、' | '．' | '·')
}

// -----------------------------------------------------------------------------
// The book: chapters, the flattened character stream, the table of contents
// -----------------------------------------------------------------------------

/// One chapter of a book, as the parser found it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chapter {
    pub title: Option<String>,
    pub paragraphs: Vec<String>,
}

/// A line of the table of contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocEntry {
    pub title: String,
    pub depth: usize,
    /// The chapter the entry starts.
    pub chapter: usize,
    /// The character offset the chapter starts at.
    pub start: usize,
}

/// One character of the flattened book, with the boundaries that shape the
/// layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FlatChar {
    ch: char,
    /// First character of a paragraph: it takes the first-column indent.
    paragraph_start: bool,
    /// First character of a chapter: it starts a column of its own, with a
    /// blank column before it. `Some` holds the chapter index.
    chapter_start: Option<usize>,
    /// Part of a chapter title, which gets a blank cell above it.
    title: bool,
}

/// A book laid out vertically: the metadata, the chapters, and the flattened
/// character stream the layout works on.
#[derive(Clone, Debug)]
pub struct Document {
    pub title: Option<String>,
    pub author: Option<String>,
    pub language: Option<String>,
    pub chapters: Vec<Chapter>,
    /// The book as one sequence of characters; a location is an index here.
    chars: Vec<FlatChar>,
    /// The first character of every chapter.
    chapter_starts: Vec<usize>,
    toc: Vec<TocEntry>,
}

impl Document {
    /// Open `path`: an EPUB through its OPF spine, anything else as TXT.
    pub fn open(path: &Path) -> Result<Document, Error> {
        let format = aristotle_library::format::sniff(path).ok().flatten();
        match format {
            Some(aristotle_library::Format::Epub) => Document::from_epub(path),
            Some(aristotle_library::Format::Txt) => Document::from_txt(path),
            // `sniff` reads the magic bytes and knows nothing about an empty
            // or unusual file: fall back on the extension, then on the ZIP
            // magic, then on plain text.
            _ => {
                let extension = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if extension == "epub" || extension == "kepub" || starts_with_zip(path) {
                    Document::from_epub(path)
                } else {
                    Document::from_txt(path)
                }
            }
        }
    }

    /// Build a document from chapters that are already parsed (the tests, and
    /// callers that do their own parsing).
    pub fn from_chapters(chapters: Vec<Chapter>) -> Document {
        Document::assemble(chapters, None, None, None)
    }

    fn assemble(
        chapters: Vec<Chapter>,
        title: Option<String>,
        author: Option<String>,
        language: Option<String>,
    ) -> Document {
        let mut chars: Vec<FlatChar> = Vec::new();
        let mut chapter_starts: Vec<usize> = Vec::new();
        let mut toc: Vec<TocEntry> = Vec::new();

        for (index, chapter) in chapters.iter().enumerate() {
            let start = chars.len();
            chapter_starts.push(start);
            let heading = chapter
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty());
            if let Some(heading) = heading {
                toc.push(TocEntry {
                    title: heading.to_string(),
                    depth: 0,
                    chapter: index,
                    start,
                });
                for (offset, ch) in heading.chars().enumerate() {
                    chars.push(FlatChar {
                        ch,
                        paragraph_start: offset == 0,
                        chapter_start: (offset == 0).then_some(index),
                        title: true,
                    });
                }
            }
            for paragraph in &chapter.paragraphs {
                let paragraph = paragraph.trim();
                if paragraph.is_empty() {
                    continue;
                }
                // A chapter without a heading is started by its first
                // paragraph.
                let starts_chapter = chars.len() == start;
                for (offset, ch) in paragraph.chars().enumerate() {
                    chars.push(FlatChar {
                        ch,
                        paragraph_start: offset == 0,
                        chapter_start: (offset == 0 && starts_chapter).then_some(index),
                        title: false,
                    });
                }
            }
        }

        Document {
            title,
            author,
            language,
            chapters,
            chars,
            chapter_starts,
            toc,
        }
    }

    /// The number of characters in the book: the size of the location space.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The table of contents, in reading order.
    pub fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    /// The chapter a location is in.
    pub fn chapter_of(&self, location: usize) -> Option<usize> {
        self.chapter_starts
            .partition_point(|&start| start <= location)
            .checked_sub(1)
    }

    /// The character range of a chapter: `start..end`.
    pub fn chapter_range(&self, chapter: usize) -> (usize, usize) {
        let start = self.chapter_starts.get(chapter).copied().unwrap_or(0);
        let end = self
            .chapter_starts
            .get(chapter + 1)
            .copied()
            .unwrap_or(self.chars.len());
        (start, end)
    }

    /// The title of the chapter a location is in.
    pub fn chapter_title(&self, location: usize) -> Option<String> {
        let chapter = self.chapter_of(location)?;
        self.chapters
            .get(chapter)?
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    }

    /// The table of contents line that names `chapter`, if it has one.
    pub fn toc_index_of(&self, chapter: usize) -> Option<usize> {
        self.toc.iter().position(|entry| entry.chapter == chapter)
    }

    /// The character offset a table of contents line starts at.
    pub fn toc_start(&self, index: usize) -> Option<usize> {
        self.toc.get(index).map(|entry| entry.start)
    }

    /// The share of the book before `location`, 0 to 1.
    pub fn fraction(&self, location: usize) -> f32 {
        if self.chars.is_empty() {
            return 0.0;
        }
        (location as f32 / self.chars.len() as f32).clamp(0.0, 1.0)
    }

    /// The location at `fraction` of the book.
    pub fn location_at_fraction(&self, fraction: f32) -> usize {
        if self.chars.is_empty() {
            return 0;
        }
        ((fraction.clamp(0.0, 1.0) * self.chars.len() as f32) as usize)
            .min(self.chars.len().saturating_sub(1))
    }

    /// The character offset where the paragraph or chapter holding `i` ends.
    fn paragraph_end(&self, i: usize) -> usize {
        let mut j = i + 1;
        while j < self.chars.len()
            && !self.chars[j].paragraph_start
            && self.chars[j].chapter_start.is_none()
        {
            j += 1;
        }
        j
    }

    /// Lay out the page that starts at `start`, right to left.
    ///
    /// The result is a pure function of `start` and `style`: the next page
    /// starts where [`PageLayout::next`] points, and a page that starts at the
    /// end of the book is empty.
    pub fn layout_page(&self, start: usize, style: &VerticalStyle) -> PageLayout {
        let slots = style.slots();
        let top = style.top();
        let bottom = style.bottom();
        let em = style.em;
        let mut columns: Vec<ColumnSlot> = (0..slots)
            .map(|i| ColumnSlot {
                x: style.column_x(i),
                has_text: false,
                top,
                bottom,
            })
            .collect();
        let mut glyphs: Vec<Glyph> = Vec::new();
        let mut col = 0usize;
        let mut y = top;
        let mut placed = false;
        let mut i = start.min(self.chars.len());
        let chapter = self.chapter_title(i);

        if slots == 0 || i >= self.chars.len() {
            return PageLayout {
                start: i,
                next: i,
                glyphs,
                columns,
                chapter,
            };
        }

        'outer: while i < self.chars.len() {
            let fc = self.chars[i];
            // A chapter starts a column of its own, with a blank column
            // between it and the text before it.
            if fc.chapter_start.is_some() {
                if placed {
                    if !advance(&mut col, &mut y, slots, top) {
                        break;
                    }
                    if !advance(&mut col, &mut y, slots, top) {
                        break;
                    }
                }
                if fc.title {
                    // A blank cell above the title.
                    y += em;
                } else if fc.paragraph_start {
                    y += style.indent;
                }
            } else if fc.paragraph_start {
                if placed && y > top && !advance(&mut col, &mut y, slots, top) {
                    break;
                }
                y += style.indent;
            }

            // Fill the current column with the paragraph, column by column.
            let paragraph_end = self.paragraph_end(i);
            let mut j = i;
            loop {
                let avail = bottom - y;
                if avail <= 0 {
                    if !advance(&mut col, &mut y, slots, top) {
                        i = j;
                        break 'outer;
                    }
                    continue;
                }
                // How many characters of the paragraph fit in what is left
                // of this column?
                let mut fit = 0usize;
                let mut height = 0i32;
                while j + fit < paragraph_end {
                    let advance = cell_advance(self.chars[j + fit].ch, em);
                    if height + advance > avail {
                        break;
                    }
                    height += advance;
                    fit += 1;
                }
                if fit == 0 {
                    // Not even one character fits: a column that the indent
                    // alone filled moves on, but a page always advances.
                    if columns[col].has_text {
                        if !advance(&mut col, &mut y, slots, top) {
                            i = j;
                            break 'outer;
                        }
                        continue;
                    }
                    fit = 1;
                }
                // 避头禁则 and 避尾禁则: move the break so that no terminal
                // punctuation starts the next column and no opening bracket
                // ends this one.
                if j + fit < paragraph_end {
                    let mut keep = fit;
                    while keep > 0
                        && (is_no_start(self.chars[j + keep].ch)
                            || is_no_end(self.chars[j + keep - 1].ch))
                    {
                        keep -= 1;
                    }
                    if keep > 0 {
                        fit = keep;
                    }
                }
                for _ in 0..fit {
                    let fc = self.chars[j];
                    let (ch, orientation) = glyph_for(fc.ch);
                    let advance = cell_advance(fc.ch, em);
                    let slot = &mut columns[col];
                    if slot.has_text {
                        slot.top = slot.top.min(y);
                        slot.bottom = slot.bottom.max(y + advance);
                    } else {
                        slot.top = y;
                        slot.bottom = y + advance;
                        slot.has_text = true;
                    }
                    glyphs.push(Glyph {
                        ch,
                        source: fc.ch,
                        x: slot.x,
                        y,
                        advance,
                        orientation,
                        shifted: is_shifted(fc.ch),
                    });
                    y += advance;
                    placed = true;
                    j += 1;
                }
                if j >= paragraph_end {
                    break;
                }
                if !advance(&mut col, &mut y, slots, top) {
                    i = j;
                    break 'outer;
                }
            }
            i = j;
        }

        PageLayout {
            start,
            next: i,
            glyphs,
            columns,
            chapter,
        }
    }

    /// The start of the page after the one starting at `location`, if there
    /// is one.
    pub fn next_page(&self, location: usize, style: &VerticalStyle) -> Option<usize> {
        let layout = self.layout_page(location, style);
        (layout.next > location && layout.next < self.chars.len()).then_some(layout.next)
    }

    /// The start of the page that holds `location`: the layout is
    /// deterministic, so walking the pages from the chapter's first character
    /// finds it without a page cache.
    pub fn page_start(&self, location: usize, style: &VerticalStyle) -> usize {
        let location = location.min(self.chars.len().saturating_sub(1));
        let chapter = self.chapter_of(location).unwrap_or(0);
        let (start, _) = self.chapter_range(chapter);
        let mut page = start;
        // A chapter of a few thousand characters is a few hundred page
        // layouts at worst, and only when a page is opened at its end.
        while let Some(next) = self.next_page(page, style) {
            if next > location {
                break;
            }
            page = next;
        }
        page
    }
}

/// Advance to the next column, if the page has one.
fn advance(col: &mut usize, y: &mut i32, slots: usize, top: i32) -> bool {
    if *col + 1 >= slots {
        return false;
    }
    *col += 1;
    *y = top;
    true
}

fn starts_with_zip(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 2];
    match fs::File::open(path) {
        Ok(mut file) => file.read_exact(&mut magic).is_ok() && &magic == b"PK",
        Err(_) => false,
    }
}

// -----------------------------------------------------------------------------
// EPUB and TXT parsing
// -----------------------------------------------------------------------------

impl Document {
    /// Read an EPUB: the OPF spine names the chapters, the navigation
    /// document names them, and every spine item's XHTML gives the
    /// paragraphs.
    pub fn from_epub(path: &Path) -> Result<Document, Error> {
        let mut zip = aristotle_library::zip::ZipArchive::open(path)
            .map_err(|e| format_err!("{}: {e}", path.display()))?;
        let container = read_member(&mut zip, "META-INF/container.xml")
            .ok_or_else(|| format_err!("EPUB has no META-INF/container.xml"))?;
        let opf_path = {
            let xml = String::from_utf8_lossy(&container);
            let document =
                roxmltree::Document::parse(&xml).map_err(|e| format_err!("container.xml: {e}"))?;
            document
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "rootfile")
                .and_then(|node| node.attribute("full-path"))
                .map(str::to_string)
                .ok_or_else(|| format_err!("container.xml names no rootfile"))?
        };
        let opf_bytes = read_member(&mut zip, &opf_path)
            .ok_or_else(|| format_err!("EPUB has no {opf_path}"))?;
        let opf = String::from_utf8_lossy(&opf_bytes).into_owned();
        let document =
            roxmltree::Document::parse(&opf).map_err(|e| format_err!("{opf_path}: {e}"))?;

        let base = opf_path
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default();

        let mut manifest: HashMap<String, ManifestItem> = HashMap::new();
        for item in document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "item")
        {
            let Some(id) = item.attribute("id") else {
                continue;
            };
            let Some(href) = item.attribute("href") else {
                continue;
            };
            manifest.insert(
                id.to_string(),
                ManifestItem {
                    href: resolve_href(&base, href),
                    media_type: item.attribute("media-type").unwrap_or("").to_string(),
                    properties: item.attribute("properties").unwrap_or("").to_string(),
                },
            );
        }

        let mut spine: Vec<String> = Vec::new();
        for itemref in document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "itemref")
        {
            if itemref.attribute("linear") == Some("no") {
                continue;
            }
            let Some(idref) = itemref.attribute("idref") else {
                continue;
            };
            if let Some(item) = manifest.get(idref) {
                if !item.properties.split_whitespace().any(|p| p == "nav") {
                    spine.push(item.href.clone());
                }
            }
        }

        // The navigation document, EPUB 3 first, the NCX of EPUB 2 second.
        let mut toc_titles: HashMap<String, String> = HashMap::new();
        let nav = manifest
            .values()
            .find(|item| item.properties.split_whitespace().any(|p| p == "nav"))
            .map(|item| item.href.clone());
        if let Some(nav) = nav {
            if let Some(bytes) = read_member(&mut zip, &nav) {
                let html = String::from_utf8_lossy(&bytes);
                for (href, title) in nav_titles(&html) {
                    let href = resolve_href(&base, &href);
                    toc_titles.entry(href).or_insert(title);
                }
            }
        }
        if toc_titles.is_empty() {
            let ncx = document
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "spine")
                .and_then(|node| node.attribute("toc"))
                .and_then(|id| manifest.get(id))
                .map(|item| item.href.clone());
            if let Some(ncx) = ncx {
                if let Some(bytes) = read_member(&mut zip, &ncx) {
                    let xml = String::from_utf8_lossy(&bytes);
                    for (href, title) in ncx_titles(&xml) {
                        let href = resolve_href(&base, &href);
                        toc_titles.entry(href).or_insert(title);
                    }
                }
            }
        }

        let title = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "title"
                    && node.tag_name().namespace() == Some("http://purl.org/dc/elements/1.1/")
            })
            .and_then(|node| node.text())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let author = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "creator"
                    && node.tag_name().namespace() == Some("http://purl.org/dc/elements/1.1/")
            })
            .and_then(|node| node.text())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let language = document
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "language"
                    && node.tag_name().namespace() == Some("http://purl.org/dc/elements/1.1/")
            })
            .and_then(|node| node.text())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());

        let mut chapters: Vec<Chapter> = Vec::new();
        for href in &spine {
            let Some(bytes) = read_member(&mut zip, href) else {
                continue;
            };
            let html = decode_markup(&bytes);
            let blocks = richtext::from_html(&html);
            let mut heading: Option<String> = None;
            let mut paragraphs: Vec<String> = Vec::new();
            for block in &blocks.blocks {
                let text = block.text().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                if block.heading && heading.is_none() && paragraphs.is_empty() {
                    heading = Some(text);
                } else {
                    paragraphs.push(text);
                }
            }
            let title = toc_titles.get(href).cloned().or(heading);
            if title.is_none() && paragraphs.is_empty() {
                continue;
            }
            chapters.push(Chapter { title, paragraphs });
        }

        Ok(Document::assemble(chapters, title, author, language))
    }

    /// Read a TXT file: the encoding is guessed the way the TXT converter
    /// guesses it, and the chapters are split with the same pattern (see
    /// [`crate::txt`]).
    pub fn from_txt(path: &Path) -> Result<Document, Error> {
        let bytes = fs::read(path).map_err(|e| format_err!("{}: {e}", path.display()))?;
        let text = aristotle_library::encoding::decode_guess(&bytes);
        let extracted = aristotle_library::txt::extract(path).unwrap_or_default();
        let title = Some(extracted.meta.title.clone())
            .filter(|t| !t.trim().is_empty())
            .or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            });
        let author = extracted
            .meta
            .authors
            .first()
            .map(|author| author.name.clone())
            .filter(|name| !name.trim().is_empty());

        let pattern = Regex::new(CHAPTER_PATTERN)
            .map_err(|e| format_err!("the chapter pattern is invalid: {e}"))?;
        let mut chapters: Vec<Chapter> = Vec::new();
        let mut heading: Option<String> = None;
        let mut paragraphs: Vec<String> = Vec::new();
        let mut found_heading = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(chapter_title) = is_chapter_heading(line, &pattern) {
                found_heading = true;
                if heading.is_some() || !paragraphs.is_empty() {
                    chapters.push(Chapter {
                        title: heading.take(),
                        paragraphs: std::mem::take(&mut paragraphs),
                    });
                }
                heading = Some(chapter_title);
            } else {
                paragraphs.push(line.to_string());
            }
        }
        if heading.is_some() || !paragraphs.is_empty() {
            chapters.push(Chapter {
                title: heading,
                paragraphs,
            });
        }
        if !found_heading {
            // No headings at all: one chapter of every paragraph.
            let paragraphs: Vec<String> = chapters
                .into_iter()
                .flat_map(|chapter| chapter.paragraphs)
                .collect();
            chapters = vec![Chapter {
                title: None,
                paragraphs,
            }];
        }
        if chapters.is_empty() {
            chapters.push(Chapter::default());
        }
        Ok(Document::assemble(chapters, title, author, None))
    }
}

/// One item of the OPF manifest.
struct ManifestItem {
    href: String,
    #[allow(dead_code)]
    media_type: String,
    properties: String,
}

/// Read a zip member by name, tolerating percent-encoded names (which some
/// EPUB writers use for the non-ASCII file names of Chinese books).
fn read_member(zip: &mut aristotle_library::zip::ZipArchive, name: &str) -> Option<Vec<u8>> {
    if let Ok(bytes) = zip.read(name) {
        return Some(bytes);
    }
    let decoded = percent_decode_str(name).decode_utf8_lossy().into_owned();
    if decoded != name {
        if let Ok(bytes) = zip.read(&decoded) {
            return Some(bytes);
        }
    }
    None
}

/// Decode an XHTML file: UTF-8, with a fallback to the encoding guesser for
/// the files that declare something else.
fn decode_markup(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => aristotle_library::encoding::decode_guess(bytes),
    }
}

/// Resolve an `href` from a document in `base` to a zip member name, dropping
/// the fragment and the percent-encoding and collapsing `.` and `..`.
fn resolve_href(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let href = percent_decode_str(href).decode_utf8_lossy().into_owned();
    let mut parts: Vec<&str> = Vec::new();
    let joined = if href.starts_with('/') {
        href.trim_start_matches('/').to_string()
    } else if base.is_empty() {
        href
    } else {
        format!("{base}/{href}")
    };
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// The (href, title) pairs of an EPUB 3 navigation document.
fn nav_titles(html: &str) -> Vec<(String, String)> {
    let Ok(document) = roxmltree::Document::parse(html) else {
        return Vec::new();
    };
    document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "a")
        .filter_map(|node| {
            let href = node.attribute("href")?.to_string();
            let title = node.text().unwrap_or_default().trim().to_string();
            (!title.is_empty()).then_some((href, title))
        })
        .collect()
}

/// The (href, title) pairs of an EPUB 2 NCX document, depth first.
fn ncx_titles(xml: &str) -> Vec<(String, String)> {
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(root) = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "navMap")
    {
        collect_nav_points(root, &mut out);
    }
    out
}

fn collect_nav_points(node: roxmltree::Node, out: &mut Vec<(String, String)>) {
    for child in node.children().filter(|n| n.is_element()) {
        if child.tag_name().name() == "navPoint" {
            let title = child
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "text")
                .and_then(|n| n.text())
                .map(|text| text.trim().to_string())
                .unwrap_or_default();
            let href = child
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "content")
                .and_then(|n| n.attribute("src"))
                .map(str::to_string);
            if let (Some(href), false) = (href, title.is_empty()) {
                out.push((href, title));
            }
            collect_nav_points(child, out);
        } else {
            collect_nav_points(child, out);
        }
    }
}

// -----------------------------------------------------------------------------
// Geometry and the page
// -----------------------------------------------------------------------------

/// The page geometry and the typographic settings the vertical layout uses,
/// derived from a [`Style`].
#[derive(Clone, Debug, PartialEq)]
pub struct VerticalStyle {
    pub width: i32,
    pub height: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    pub margin_top: i32,
    pub margin_bottom: i32,
    /// The body size in pixels: one cell of a wide character.
    pub em: i32,
    /// The distance between two columns, in pixels.
    pub pitch: i32,
    /// The first-column indent of a paragraph, in pixels.
    pub indent: i32,
    pub font_weight: u16,
    /// The family and directory the renderer looks for the body face in.
    pub font_family: String,
    pub font_dir: String,
}

impl VerticalStyle {
    /// The vertical geometry of a horizontal style: the same panel, the same
    /// body size, the same margins, read sideways.
    pub fn from_style(style: &Style) -> VerticalStyle {
        let width = style.width as i32;
        let height = style.height as i32;
        let em = ((style.font_size * style.dpi as f32 / 72.0).round() as i32).max(8);
        let pitch = ((em as f32 * style.line_height.max(1.0)).round() as i32).max(em);
        let margin = style.margin_px().max(0);
        let margin_left = margin.min(width / 4);
        let margin_right = margin.min(width / 4);
        let margin_top = (style.text_top as i32).clamp(0, height / 3);
        let margin_bottom = (style.text_bottom as i32).clamp(0, height / 3);
        let block = (height - margin_top - margin_bottom).max(em);
        VerticalStyle {
            width,
            height,
            margin_left,
            margin_right,
            margin_top,
            margin_bottom,
            em,
            pitch,
            indent: ((style.cjk_indent_em() * em as f32).round() as i32).clamp(0, block / 2),
            font_weight: style.font_weight,
            font_family: style.cjk_font_family.clone(),
            font_dir: style.font_dir.clone(),
        }
    }

    /// The top of the text block.
    pub fn top(&self) -> i32 {
        self.margin_top
    }

    /// The bottom of the text block.
    pub fn bottom(&self) -> i32 {
        self.height - self.margin_bottom
    }

    /// The centre of column `index`, counting from the right edge.
    pub fn column_x(&self, index: usize) -> i32 {
        self.width - self.margin_right - self.em / 2 - index as i32 * self.pitch
    }

    /// How many columns fit between the margins.
    pub fn slots(&self) -> usize {
        if self.em <= 0 || self.pitch <= 0 || self.width <= 0 {
            return 0;
        }
        let mut slots = 0usize;
        while slots < 4096 && self.column_x(slots) - self.em / 2 >= self.margin_left {
            slots += 1;
        }
        slots
    }
}

/// One glyph of a laid-out page, in page pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    /// The character to draw: the vertical form for the punctuation that has
    /// one.
    pub ch: char,
    /// The character in the book, for the rotated fallback.
    pub source: char,
    /// The centre of the glyph's column.
    pub x: i32,
    /// The top of the glyph's cell.
    pub y: i32,
    /// The height of the cell.
    pub advance: i32,
    pub orientation: Orientation,
    /// Terminal punctuation, drawn at the top right of its cell.
    pub shifted: bool,
}

/// A column of the page: where its rule goes, and how far its text reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnSlot {
    pub x: i32,
    pub has_text: bool,
    /// The top of the text in this column (the block's top when empty).
    pub top: i32,
    /// The bottom of the text in this column (the block's bottom when empty).
    pub bottom: i32,
}

/// One laid-out page: the glyphs, the column slots and where the next page
/// starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageLayout {
    /// The character offset the page starts at.
    pub start: usize,
    /// The character offset the next page starts at (the book's length on the
    /// last page).
    pub next: usize,
    pub glyphs: Vec<Glyph>,
    /// Every column slot the page has room for, from the right edge left.
    pub columns: Vec<ColumnSlot>,
    /// The title of the chapter the page starts in.
    pub chapter: Option<String>,
}

impl PageLayout {
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

/// A vertical rule: the guide drawn on the left side of a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleLine {
    pub x: i32,
    pub top: i32,
    pub bottom: i32,
}

/// The rules the reading guide draws for a page, clamped to the page.
///
/// [`UnderlineScope::TextOnly`] follows the text of every column that has
/// some; [`UnderlineScope::FullLine`] fills every column slot, the empty ones
/// included, from the top of the text block to its bottom.
pub fn rule_lines(
    layout: &PageLayout,
    style: &VerticalStyle,
    underline: Underline,
    scope: UnderlineScope,
) -> Vec<RuleLine> {
    if underline == Underline::Off {
        return Vec::new();
    }
    let mut out = Vec::new();
    for slot in &layout.columns {
        if !slot.has_text && scope == UnderlineScope::TextOnly {
            continue;
        }
        let (top, bottom) = match scope {
            UnderlineScope::TextOnly => (slot.top, slot.bottom),
            UnderlineScope::FullLine => (style.top(), style.bottom()),
        };
        // The rule sits on the left edge of its column, halfway into the
        // gap between two columns, and never leaves the page.
        let x = (slot.x - style.pitch / 2).clamp(0, (style.width - 1).max(0));
        let top = top.clamp(0, style.height);
        let bottom = bottom.clamp(0, style.height);
        if bottom > top {
            out.push(RuleLine { x, top, bottom });
        }
    }
    out
}

// -----------------------------------------------------------------------------
// Drawing
// -----------------------------------------------------------------------------

/// The gray of the light body weight: a thin face is approximated on the
/// 16-level panel by a lighter ink, the way the engine lightens the guide.
pub const LIGHT_GRAY: u8 = 0x50;

/// The three body weights the layout draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Light,
    Normal,
    Bold,
}

impl Weight {
    /// Map a CSS `font-weight` number onto the three weights.
    pub fn from_font_weight(weight: u16) -> Weight {
        match weight {
            0..=399 => Weight::Light,
            400 => Weight::Normal,
            _ => Weight::Bold,
        }
    }
}

/// A page-sized grayscale buffer, for callers that do not have a [`Page`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrayPage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A grayscale canvas the glyphs are blended into.
struct GrayCanvas<'a> {
    data: &'a mut [u8],
    width: u32,
    height: u32,
    stride: usize,
}

impl Framebuffer for GrayCanvas<'_> {
    fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.width && y < self.height {
            let index = y as usize * self.stride + x as usize;
            if index < self.data.len() {
                self.data[index] = color.gray();
            }
        }
    }

    fn set_blended_pixel(&mut self, x: u32, y: u32, color: Color, alpha: f32) {
        if x < self.width && y < self.height {
            let index = y as usize * self.stride + x as usize;
            if index < self.data.len() {
                let a = self.data[index] as f32;
                let b = color.gray() as f32;
                self.data[index] = (a + (b - a) * alpha.clamp(0.0, 1.0)) as u8;
            }
        }
    }

    fn invert_region(&mut self, _rect: &plato_core::geom::Rectangle) {}
    fn shift_region(&mut self, _rect: &plato_core::geom::Rectangle, _drift: u8) {}
    fn update(
        &mut self,
        _rect: &plato_core::geom::Rectangle,
        _mode: plato_core::framebuffer::UpdateMode,
    ) -> Result<u32, Error> {
        Err(format_err!("an off-screen canvas has no display"))
    }
    fn wait(&self, _token: u32) -> Result<i32, Error> {
        Err(format_err!("an off-screen canvas has no display"))
    }
    fn save(&self, _path: &str) -> Result<(), Error> {
        Ok(())
    }
    fn set_rotation(&mut self, _n: i8) -> Result<(u32, u32), Error> {
        Err(format_err!("an off-screen canvas has no rotation"))
    }
    fn set_monochrome(&mut self, _enable: bool) {}
    fn set_dithered(&mut self, _enable: bool) {}
    fn set_inverted(&mut self, _enable: bool) {}
    fn monochrome(&self) -> bool {
        false
    }
    fn dithered(&self) -> bool {
        false
    }
    fn inverted(&self) -> bool {
        false
    }
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
}

/// A canvas that turns a glyph 90° clockwise on its way to another canvas.
///
/// A point `(x, y)` of the horizontal text space lands at `(pivot.x - y,
/// pivot.y + x)`: the text advances downwards, and the top of the glyph faces
/// right, the way a Latin word sits in a vertical line.
struct Rotated<'a> {
    inner: &'a mut dyn Framebuffer,
    pivot: Point,
}

impl Framebuffer for Rotated<'_> {
    fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if let Some((px, py)) = self.map(x, y) {
            self.inner.set_pixel(px, py, color);
        }
    }

    fn set_blended_pixel(&mut self, x: u32, y: u32, color: Color, alpha: f32) {
        if let Some((px, py)) = self.map(x, y) {
            self.inner.set_blended_pixel(px, py, color, alpha);
        }
    }

    fn invert_region(&mut self, _rect: &plato_core::geom::Rectangle) {}
    fn shift_region(&mut self, _rect: &plato_core::geom::Rectangle, _drift: u8) {}
    fn update(
        &mut self,
        _rect: &plato_core::geom::Rectangle,
        _mode: plato_core::framebuffer::UpdateMode,
    ) -> Result<u32, Error> {
        Err(format_err!("a rotated canvas has no display"))
    }
    fn wait(&self, _token: u32) -> Result<i32, Error> {
        Err(format_err!("a rotated canvas has no display"))
    }
    fn save(&self, _path: &str) -> Result<(), Error> {
        Ok(())
    }
    fn set_rotation(&mut self, _n: i8) -> Result<(u32, u32), Error> {
        Err(format_err!("a rotated canvas has no rotation"))
    }
    fn set_monochrome(&mut self, _enable: bool) {}
    fn set_dithered(&mut self, _enable: bool) {}
    fn set_inverted(&mut self, _enable: bool) {}
    fn monochrome(&self) -> bool {
        self.inner.monochrome()
    }
    fn dithered(&self) -> bool {
        self.inner.dithered()
    }
    fn inverted(&self) -> bool {
        self.inner.inverted()
    }
    fn width(&self) -> u32 {
        self.inner.height()
    }
    fn height(&self) -> u32 {
        self.inner.width()
    }
}

impl Rotated<'_> {
    /// The page point a text-space point lands on, when it is on the page.
    fn map(&self, x: u32, y: u32) -> Option<(u32, u32)> {
        let (x, y) = (x as i32, y as i32);
        let px = self.pivot.x - y;
        let py = self.pivot.y + x;
        if px >= 0
            && py >= 0
            && (px as u32) < self.inner.width()
            && (py as u32) < self.inner.height()
        {
            Some((px as u32, py as u32))
        } else {
            None
        }
    }
}

/// The document `Book` holds for a vertically laid-out book.
///
/// [`Book`] keeps a `Box<dyn Document>` for every engine, and the vertical
/// engine has no Plato document at all: it reads the book itself. The
/// placeholder answers every query with "nothing" and is never asked anything
/// that matters, because [`Book`]'s vertical branches answer first.
///
/// [`Book`]: crate::book::Book
pub struct EmptyDocument;

impl plato_core::document::Document for EmptyDocument {
    fn dims(&self, _index: usize) -> Option<(f32, f32)> {
        None
    }

    fn pages_count(&self) -> usize {
        0
    }

    fn toc(&mut self) -> Option<Vec<plato_core::document::TocEntry>> {
        None
    }

    fn chapter<'a>(
        &mut self,
        _offset: usize,
        _toc: &'a [plato_core::document::TocEntry],
    ) -> Option<(&'a plato_core::document::TocEntry, f32)> {
        None
    }

    fn chapter_relative<'a>(
        &mut self,
        _offset: usize,
        _dir: plato_core::geom::CycleDir,
        _toc: &'a [plato_core::document::TocEntry],
    ) -> Option<&'a plato_core::document::TocEntry> {
        None
    }

    fn words(
        &mut self,
        _loc: plato_core::document::Location,
    ) -> Option<(Vec<plato_core::document::BoundedText>, usize)> {
        None
    }

    fn lines(
        &mut self,
        _loc: plato_core::document::Location,
    ) -> Option<(Vec<plato_core::document::BoundedText>, usize)> {
        None
    }

    fn links(
        &mut self,
        _loc: plato_core::document::Location,
    ) -> Option<(Vec<plato_core::document::BoundedText>, usize)> {
        None
    }

    fn images(
        &mut self,
        _loc: plato_core::document::Location,
    ) -> Option<(Vec<plato_core::geom::Boundary>, usize)> {
        None
    }

    fn pixmap(
        &mut self,
        _loc: plato_core::document::Location,
        _scale: f32,
        _samples: usize,
    ) -> Option<(plato_core::framebuffer::Pixmap, usize)> {
        None
    }

    fn layout(&mut self, _width: u32, _height: u32, _font_size: f32, _dpi: u16) {}

    fn set_font_family(&mut self, _family_name: &str, _search_path: &str) {}

    fn set_margin_width(&mut self, _width: i32) {}

    fn set_text_align(&mut self, _text_align: plato_core::metadata::TextAlign) {}

    fn set_line_height(&mut self, _line_height: f32) {}

    fn set_hyphen_penalty(&mut self, _hyphen_penalty: i32) {}

    fn set_stretch_tolerance(&mut self, _stretch_tolerance: f32) {}

    fn set_ignore_document_css(&mut self, _ignore: bool) {}

    fn title(&self) -> Option<String> {
        None
    }

    fn author(&self) -> Option<String> {
        None
    }

    fn metadata(&self, _key: &str) -> Option<String> {
        None
    }

    fn is_reflowable(&self) -> bool {
        true
    }
}

/// The font and the metrics a page is drawn with.
///
/// FreeType faces belong to the thread that opened them, and a [`Book`] moves
/// between threads (the open worker, then the reader), so the renderer is
/// kept per thread and rebuilt when the style changes the face or its size.
///
/// [`Book`]: crate::book::Book
pub struct Renderer {
    font: Font,
    em: i32,
    ascender: i32,
    descender: i32,
}

impl Renderer {
    /// Load the body face for `style`: the configured CJK family when it can
    /// draw Chinese, else the first font of the data directory that can.
    pub fn load(style: &Style) -> Result<Renderer, Error> {
        let opener = FontOpener::new()?;
        let size = (style.font_size * 64.0).round().max(1.0) as u32;
        let em = ((style.font_size * style.dpi as f32 / 72.0).round() as i32).max(8);
        let mut font = open_body_font(&opener, style, size)?;
        let ascender = font.ascender();
        let descender = font.descender();
        font.set_size(size, style.dpi);
        Ok(Renderer {
            font,
            em,
            ascender,
            descender,
        })
    }

    /// The body size in pixels this renderer draws with.
    pub fn em(&self) -> i32 {
        self.em
    }

    /// Draw `layout` and its reading guide onto `page`.
    pub fn draw_page(
        &mut self,
        page: &mut Page,
        layout: &PageLayout,
        style: &VerticalStyle,
        underline: Underline,
        scope: UnderlineScope,
    ) {
        self.draw(page, layout, style, underline, scope);
    }

    /// Draw `layout` and its reading guide into a new grayscale buffer the
    /// size of the page.
    pub fn render_gray(
        &mut self,
        layout: &PageLayout,
        style: &VerticalStyle,
        underline: Underline,
        scope: UnderlineScope,
    ) -> GrayPage {
        let width = style.width.max(0) as u32;
        let height = style.height.max(0) as u32;
        let mut data = vec![0xffu8; width as usize * height as usize];
        {
            let mut canvas = GrayCanvas {
                data: &mut data,
                width,
                height,
                stride: width as usize,
            };
            self.draw(&mut canvas, layout, style, underline, scope);
        }
        GrayPage {
            width,
            height,
            data,
        }
    }

    /// Draw `layout` and its reading guide into a crengine-shaped gray
    /// buffer, the page's own size.
    #[cfg(feature = "crengine")]
    pub fn render_gray_buffer(
        &mut self,
        layout: &PageLayout,
        style: &VerticalStyle,
        underline: Underline,
        scope: UnderlineScope,
    ) -> inkread_platform::GrayBuffer {
        let width = style.width.max(0) as u32;
        let height = style.height.max(0) as u32;
        let mut buffer = inkread_platform::GrayBuffer::filled(width, height, 0xff);
        {
            let mut canvas = GrayCanvas {
                data: buffer.pixels_mut(),
                width,
                height,
                stride: width as usize,
            };
            self.draw(&mut canvas, layout, style, underline, scope);
        }
        buffer
    }

    fn draw(
        &mut self,
        page: &mut dyn Framebuffer,
        layout: &PageLayout,
        style: &VerticalStyle,
        underline: Underline,
        scope: UnderlineScope,
    ) {
        let weight = Weight::from_font_weight(style.font_weight);
        let color = Color::Gray(match weight {
            Weight::Light => LIGHT_GRAY,
            Weight::Normal | Weight::Bold => 0x00,
        });
        let bold = weight == Weight::Bold;

        for glyph in &layout.glyphs {
            if glyph.ch == ' ' || glyph.ch == '\u{3000}' {
                continue;
            }
            let plan = plan_char(&mut self.font, glyph.ch);
            if plan.width == 0 {
                // The font has no vertical form for this mark: rotate the
                // character itself, as a fallback setting does.
                if glyph.ch != glyph.source {
                    let plan = plan_char(&mut self.font, glyph.source);
                    if plan.width > 0 {
                        self.render_rotated(page, &plan, glyph, color, bold);
                    }
                }
                continue;
            }
            match glyph.orientation {
                Orientation::Upright => self.render_upright(page, &plan, glyph, color, bold),
                Orientation::Rotated => self.render_rotated(page, &plan, glyph, color, bold),
            }
        }

        let dotted = underline == Underline::Dotted;
        for rule in rule_lines(layout, style, underline, scope) {
            draw_rule(page, &rule, dotted);
        }
    }

    fn render_upright(
        &mut self,
        page: &mut dyn Framebuffer,
        plan: &RenderPlan,
        glyph: &Glyph,
        color: Color,
        bold: bool,
    ) {
        let mut x = glyph.x - plan.width / 2;
        // Centre the em box of the glyph in its cell.
        let mut baseline = glyph.y + (glyph.advance + self.ascender + self.descender) / 2;
        if glyph.shifted {
            // The bottom-left corner of the em box becomes the top-right one.
            x += self.em / 2;
            baseline -= self.em / 2;
        } else if is_bracket_form(glyph.ch) {
            // Lift the bracket to the middle of its cell.
            baseline -= self.em / 3;
        }
        self.font.render(page, color, plan, Point::new(x, baseline));
        if bold {
            // Synthetic bold: the same glyph once more, one pixel over, so
            // the strokes thicken by dilation (a real bold face is not
            // available for every font the user can install).
            self.font
                .render(page, color, plan, Point::new(x + 1, baseline));
        }
    }

    fn render_rotated(
        &mut self,
        page: &mut dyn Framebuffer,
        plan: &RenderPlan,
        glyph: &Glyph,
        color: Color,
        bold: bool,
    ) {
        // A Latin glyph sits between its baseline and its cap height, and the
        // ascender of a CJK face is far taller than that, so centring the em
        // box would push the glyph out of its column. Centre the ink
        // instead: the baseline runs a little left of the column's centre,
        // and the glyph rises from it to the right.
        let pivot = Point::new(
            glyph.x - self.em * 3 / 8,
            glyph.y + glyph.advance / 2 - plan.width / 2,
        );
        {
            let mut canvas = Rotated {
                inner: &mut *page,
                pivot,
            };
            self.font.render(&mut canvas, color, plan, Point::new(0, 0));
        }
        if bold {
            let mut canvas = Rotated {
                inner: &mut *page,
                pivot: Point::new(pivot.x + 1, pivot.y),
            };
            self.font.render(&mut canvas, color, plan, Point::new(0, 0));
        }
    }
}

/// Draw a vertical rule: solid, or dotted two pixels on and four off.
fn draw_rule(page: &mut dyn Framebuffer, rule: &RuleLine, dotted: bool) {
    for y in rule.top..rule.bottom {
        if dotted && (y - rule.top) % 6 >= 2 {
            continue;
        }
        page.set_pixel(rule.x as u32, y as u32, Color::Gray(UNDERLINE_GRAY));
    }
}

/// Shape one character, without allocating a string for it.
fn plan_char(font: &mut Font, c: char) -> RenderPlan {
    let mut buffer = [0u8; 4];
    font.plan(c.encode_utf8(&mut buffer), None, None)
}

/// Whether the font file has a glyph for `c`, read from its character map.
///
/// Plato shapes a character its face lacks with an embedded fallback face, so
/// [`Font::plan`] cannot tell a font that has 中 from one that has not: the
/// `cmap` table can. Only the table directory and the `cmap` table are read,
/// not the whole font: the CJK faces of the data directory are megabytes.
fn font_covers(path: &Path, c: char) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 12];
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    let mut offset = 0u64;
    if &head[0..4] == b"ttcf" {
        // A collection: the first font's offset is the first entry of the
        // collection header.
        let mut first = [0u8; 4];
        if file.seek(SeekFrom::Start(12)).is_err() || file.read_exact(&mut first).is_err() {
            return false;
        }
        offset = u32::from_be_bytes(first) as u64;
    }
    if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut head).is_err() {
        return false;
    }
    let tables = u16::from_be_bytes([head[4], head[5]]) as usize;
    let mut records = vec![0u8; tables * 16];
    if file.read_exact(&mut records).is_err() {
        return false;
    }
    for index in 0..tables {
        let record = &records[index * 16..index * 16 + 16];
        if &record[0..4] != b"cmap" {
            continue;
        }
        let at = u32::from_be_bytes([record[8], record[9], record[10], record[11]]) as u64;
        let length = u32::from_be_bytes([record[12], record[13], record[14], record[15]]) as usize;
        // A table this size is not a character map.
        if length == 0 || length > 8 << 20 {
            return false;
        }
        let mut cmap = vec![0u8; length];
        if file.seek(SeekFrom::Start(at)).is_err() || file.read_exact(&mut cmap).is_err() {
            return false;
        }
        return covers_cmap(&cmap, c);
    }
    false
}

/// The `cmap` lookup of a character map table, true when `c` maps to a real
/// glyph.
fn covers_cmap(cmap: &[u8], c: char) -> bool {
    let Some(count) = be16(cmap, 2) else {
        return false;
    };
    let mut unicode_wide = None;
    let mut unicode = None;
    for index in 0..count as usize {
        let record = 4 + index * 8;
        let (Some(platform), Some(encoding), Some(offset)) = (
            be16(cmap, record),
            be16(cmap, record + 2),
            be32(cmap, record + 4),
        ) else {
            continue;
        };
        let subtable = offset as usize;
        match be16(cmap, subtable) {
            // A full Unicode range table: the best one to ask.
            Some(12) => {
                if matches!((platform, encoding), (3, 10) | (0, 4) | (0, 6)) {
                    unicode_wide = Some(subtable);
                    break;
                }
                unicode_wide.get_or_insert(subtable);
            }
            // The BMP table every font has.
            Some(4) => {
                if matches!((platform, encoding), (3, 1) | (0, 3) | (0, 1) | (0, 0)) {
                    unicode = Some(subtable);
                } else {
                    unicode.get_or_insert(subtable);
                }
            }
            _ => {}
        }
    }
    if let Some(subtable) = unicode_wide {
        if let Some(glyph) = cmap_format12(cmap, subtable, c) {
            return glyph != 0;
        }
    }
    if let Some(subtable) = unicode {
        if let Some(glyph) = cmap_format4(cmap, subtable, c) {
            return glyph != 0;
        }
    }
    false
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// A `cmap` format 4 (segment mapping to delta values) lookup.
fn cmap_format4(bytes: &[u8], subtable: usize, c: char) -> Option<u16> {
    let code = u32::from(c);
    if code > 0xFFFF {
        return None;
    }
    let code = code as u16;
    let segments = be16(bytes, subtable + 6)? / 2;
    let ends = subtable + 14;
    let starts = ends + segments as usize * 2 + 2;
    let deltas = starts + segments as usize * 2;
    let ranges = deltas + segments as usize * 2;
    for segment in 0..segments as usize {
        let end = be16(bytes, ends + segment * 2)?;
        if code > end {
            continue;
        }
        let start = be16(bytes, starts + segment * 2)?;
        if code < start {
            return None;
        }
        let delta = be16(bytes, deltas + segment * 2)?;
        let range = be16(bytes, ranges + segment * 2)?;
        if range == 0 {
            return Some(code.wrapping_add(delta));
        }
        let at = ranges + segment * 2 + range as usize + (code - start) as usize * 2;
        let glyph = be16(bytes, at)?;
        if glyph == 0 {
            return Some(0);
        }
        return Some(glyph.wrapping_add(delta));
    }
    None
}

/// A `cmap` format 12 (segmented coverage) lookup.
fn cmap_format12(bytes: &[u8], subtable: usize, c: char) -> Option<u16> {
    let code = u32::from(c);
    let groups = be32(bytes, subtable + 12)?;
    for group in 0..groups.min(0x10000) as usize {
        let at = subtable + 16 + group * 12;
        let start = be32(bytes, at)?;
        let end = be32(bytes, at + 4)?;
        if code < start || code > end {
            continue;
        }
        let glyph = be32(bytes, at + 8)? + (code - start);
        return u16::try_from(glyph).ok().or(Some(0));
    }
    None
}

/// The font files of a directory, sorted by name, so the choice is stable.
fn font_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .map(|e| {
                    let e = e.to_string_lossy().to_ascii_lowercase();
                    e == "ttf" || e == "otf" || e == "ttc"
                })
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// The font search path: the configured data directory, the directories of
/// the data directory, and the crate's own assets for a host build without
/// one.
fn font_search_dirs(style: &Style) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(data) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
        let data = PathBuf::from(data);
        dirs.push(data.join("fonts"));
        dirs.push(data.join("serif"));
    }
    if !style.font_dir.is_empty() {
        dirs.push(PathBuf::from(&style.font_dir));
    }
    dirs.push(PathBuf::from("fonts"));
    dirs.push(PathBuf::from("serif"));
    dirs.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("fonts"),
    );
    dirs
}

/// Open the body face: the configured CJK family first, then every font of
/// the data directory, and the first one that can draw a Chinese character.
fn open_body_font(opener: &FontOpener, style: &Style, size: u32) -> Result<Font, Error> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("ARISTOTLE_VERTICAL_FONT") {
        candidates.push(PathBuf::from(path));
    }
    for dir in font_search_dirs(style) {
        for path in font_files(&dir) {
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
    }
    if candidates.is_empty() {
        return Err(format_err!(
            "no font to draw a vertical page: the data directory has no fonts/ (see docs/reader.md)"
        ));
    }

    let mut fallback: Option<Font> = None;
    for path in candidates {
        let Ok(mut font) = opener.open(&path) else {
            continue;
        };
        font.set_size(size, style.dpi);
        if !font_covers(&path, '中') {
            continue;
        }
        // The family the reader picked wins over whatever the data
        // directory happens to hold first.
        if font.family_name() == Some(style.cjk_font_family.as_str()) {
            return Ok(font);
        }
        if fallback.is_none() {
            fallback = Some(font);
        }
    }
    // The vertical engine is for Chinese, Japanese and Korean books: a face
    // that cannot draw them would only set tofu, so it is an error, and the
    // book falls back to the horizontal engine (see `Book::open`).
    fallback.ok_or_else(|| {
        format_err!(
            "no font to draw a vertical page: no font in the data directory can draw Chinese \
             (see docs/reader.md)"
        )
    })
}

/// The renderer of this thread, with the style it was built for.
struct CachedRenderer {
    key: RendererKey,
    renderer: Renderer,
}

/// What a renderer depends on: a new face or a new size needs a new one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RendererKey {
    font_family: String,
    font_dir: String,
    size: u32,
    dpi: u16,
}

thread_local! {
    /// FreeType faces belong to the thread that opened them and a `Book`
    /// moves between threads, so the vertical renderer lives here rather
    /// than in the book (the same reason `Chrome` is reloaded per thread).
    static RENDERER: RefCell<Option<CachedRenderer>> = const { RefCell::new(None) };
}

/// Run `f` with this thread's renderer for `style`, loading it when the style
/// needs another face or another size.
fn with_renderer<R>(style: &Style, f: impl FnOnce(&mut Renderer) -> R) -> Result<R, Error> {
    let key = RendererKey {
        font_family: style.cjk_font_family.clone(),
        font_dir: style.font_dir.clone(),
        size: (style.font_size * 64.0).round().max(1.0) as u32,
        dpi: style.dpi,
    };
    RENDERER.with(|cell| {
        let mut slot = cell.borrow_mut();
        let rebuild = slot.as_ref().is_none_or(|cached| cached.key != key);
        if rebuild {
            *slot = Some(CachedRenderer {
                key,
                renderer: Renderer::load(style)?,
            });
        }
        let cached = slot.as_mut().expect("the renderer was just loaded");
        Ok(f(&mut cached.renderer))
    })
}

/// Draw a vertical page for `layout` into a new [`Page`] the size of `style`.
///
/// The chrome (header and footer) is not drawn: the caller composes it the
/// way it composes every other engine's page.
pub fn render_page(
    style: &Style,
    layout: &PageLayout,
    underline: Underline,
    scope: UnderlineScope,
) -> Result<Page, Error> {
    let vertical = VerticalStyle::from_style(style);
    let mut page = Page::white(style);
    with_renderer(style, |renderer| {
        renderer.draw_page(&mut page, layout, &vertical, underline, scope);
    })?;
    Ok(page)
}

/// Draw a vertical page into a grayscale buffer, for callers without a
/// [`Page`].
pub fn render_gray(
    style: &Style,
    layout: &PageLayout,
    underline: Underline,
    scope: UnderlineScope,
) -> Result<GrayPage, Error> {
    let vertical = VerticalStyle::from_style(style);
    with_renderer(style, |renderer| {
        renderer.render_gray(layout, &vertical, underline, scope)
    })
}

/// Whether a font that can draw a vertical page is available: a book opened
/// without one falls back to the horizontal engine, and the sample renderer
/// and its tests skip themselves.
pub fn has_font(style: &Style) -> bool {
    let Ok(opener) = FontOpener::new() else {
        return false;
    };
    let size = (style.font_size * 64.0).round().max(1.0) as u32;
    open_body_font(&opener, style, size).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    /// A panel with round numbers, so the assertions read like arithmetic:
    /// 600×800, 40 px margins, a 40 px em and a 52 px column pitch. Ten
    /// columns of 18 cells fit (720 px of text height, 80 px indent).
    fn test_style() -> VerticalStyle {
        VerticalStyle {
            width: 600,
            height: 800,
            margin_left: 40,
            margin_right: 40,
            margin_top: 40,
            margin_bottom: 40,
            em: 40,
            pitch: 52,
            indent: 80,
            font_weight: 400,
            font_family: "Noto Sans SC".into(),
            font_dir: "serif".into(),
        }
    }

    /// A style without the first-column indent, for the tests that count
    /// cells per column.
    fn flat_style() -> VerticalStyle {
        VerticalStyle {
            indent: 0,
            ..test_style()
        }
    }

    fn document(paragraphs: &[&str]) -> Document {
        Document::from_chapters(vec![Chapter {
            title: None,
            paragraphs: paragraphs.iter().map(|p| p.to_string()).collect(),
        }])
    }

    /// The glyphs of each column, right to left.
    fn columns(layout: &PageLayout) -> Vec<(i32, Vec<&Glyph>)> {
        let mut out: Vec<(i32, Vec<&Glyph>)> = Vec::new();
        for glyph in &layout.glyphs {
            match out.last_mut() {
                Some((x, glyphs)) if *x == glyph.x => glyphs.push(glyph),
                _ => out.push((glyph.x, vec![glyph])),
            }
        }
        out
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::var_os("ARISTOTLE_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = dir.join("vertical-tests");
        fs::create_dir_all(&dir).expect("scratch directory");
        dir.join(name)
    }

    #[test]
    fn punctuation_maps_to_the_vertical_forms() {
        let cases = [
            ('（', '︵'),
            ('）', '︶'),
            ('《', '︽'),
            ('》', '︾'),
            ('〈', '︿'),
            ('〉', '﹀'),
            ('「', '﹁'),
            ('」', '﹂'),
            ('『', '﹃'),
            ('』', '﹄'),
            ('【', '︻'),
            ('】', '︼'),
            ('—', '︱'),
            ('…', '︙'),
        ];
        for (source, form) in cases {
            assert_eq!(vertical_form(source), Some(form), "{source} has a form");
            assert_eq!(glyph_for(source), (form, Orientation::Upright));
            // The forms are full-width cells, like the marks they replace.
            assert!(is_wide(form), "{form:?} takes a full em");
            assert_eq!(cell_advance(source, 40), 40);
        }
        assert_eq!(vertical_form('中'), None);
        assert_eq!(vertical_form('A'), None);
    }

    #[test]
    fn latin_and_digits_are_rotated() {
        for c in ['A', 'z', '7', '0', ',', '(', 'a'] {
            assert_eq!(
                glyph_for(c).1,
                Orientation::Rotated,
                "{c:?} should be rotated"
            );
        }
        for c in ['中', '日', '。', '、', '︵'] {
            assert_eq!(glyph_for(c).1, Orientation::Upright, "{c:?} stays upright");
        }
        // A wide character takes a full em, a rotated one half of it.
        assert_eq!(cell_advance('中', 40), 40);
        assert_eq!(cell_advance('A', 40), 20);
        assert_eq!(cell_advance('1', 40), 20);
    }

    #[test]
    fn line_start_and_line_end_prohibitions_cover_the_punctuation() {
        for c in ['。', '，', '、', '！', '？', '；', '：', '》', '」', '）'] {
            assert!(is_no_start(c), "{c:?} may not start a column");
        }
        for c in ['（', '《', '「', '【'] {
            assert!(is_no_end(c), "{c:?} may not end a column");
        }
        assert!(!is_no_start('中'));
        assert!(!is_no_end('。'));
    }

    #[test]
    fn the_first_column_starts_at_the_right_margin_and_columns_run_left() {
        let style = test_style();
        assert_eq!(style.column_x(0), 600 - 40 - 20);
        assert_eq!(style.column_x(1), 600 - 40 - 20 - 52);
        assert!(style.slots() > 1);
        assert!(style.column_x(style.slots() - 1) - style.em / 2 >= style.margin_left);
        assert!(style.column_x(style.slots()) - style.em / 2 < style.margin_left);

        let text = "天地玄黄".repeat(200);
        let document = document(&[&text]);
        let layout = document.layout_page(0, &style);
        let columns = columns(&layout);
        assert!(columns.len() > 1);
        for pair in columns.windows(2) {
            assert!(
                pair[1].0 < pair[0].0,
                "column x must decrease: {} then {}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    #[test]
    fn characters_run_top_to_bottom_in_a_column() {
        let style = flat_style();
        let text = "天地玄黄".repeat(200);
        let document = document(&[&text]);
        let layout = document.layout_page(0, &style);
        for (_, glyphs) in columns(&layout) {
            for pair in glyphs.windows(2) {
                assert!(
                    pair[1].y > pair[0].y,
                    "glyph y must increase: {} then {}",
                    pair[0].y,
                    pair[1].y
                );
                assert_eq!(pair[1].y, pair[0].y + pair[0].advance);
            }
        }
        // The first cell of the page starts at the top margin.
        assert_eq!(layout.glyphs[0].y, style.margin_top);
    }

    #[test]
    fn the_indent_applies_at_the_top_of_a_paragraphs_first_column() {
        let style = test_style();
        let document = document(&["天地玄黄", "宇宙洪荒"]);
        let layout = document.layout_page(0, &style);
        let columns = columns(&layout);
        assert_eq!(columns.len(), 2);
        // The first paragraph is indented; the second starts a column of its
        // own at the top, indented as well.
        assert_eq!(columns[0].1[0].y, style.margin_top + style.indent);
        assert_eq!(columns[1].1[0].y, style.margin_top + style.indent);
        // Four characters of 40 px each.
        assert_eq!(columns[0].1.len(), 4);
        assert_eq!(columns[1].1.len(), 4);
    }

    #[test]
    fn terminal_punctuation_never_starts_a_column() {
        // Eighteen cells fit a column without the indent; put the full stop
        // right after them, where the break would fall, so only 避头禁则 can
        // move it.
        let style = flat_style();
        let mut text = String::new();
        for i in 0..18 {
            text.push(char::from_u32('一' as u32 + i as u32).unwrap());
        }
        text.push('。');
        text.push_str(&"天地玄黄".repeat(40));
        let document = document(&[&text]);
        let layout = document.layout_page(0, &style);
        let columns = columns(&layout);
        assert!(columns.len() > 1);
        for (index, (_, glyphs)) in columns.iter().enumerate() {
            assert!(
                !is_no_start(glyphs[0].source),
                "column {index} starts with {:?}",
                glyphs[0].source
            );
        }
        // The mark is still on the page, one cell into the second column.
        let stop = layout
            .glyphs
            .iter()
            .find(|glyph| glyph.source == '。')
            .expect("the full stop is drawn");
        assert_eq!(stop.y, style.margin_top + style.em);
    }

    #[test]
    fn an_opening_bracket_never_ends_a_column() {
        let style = flat_style();
        // Seventeen characters then 《: the bracket would be the last cell.
        let mut text = String::new();
        for i in 0..17 {
            text.push(char::from_u32('一' as u32 + i as u32).unwrap());
        }
        text.push('《');
        text.push_str(&"天地玄黄".repeat(40));
        let document = document(&[&text]);
        let layout = document.layout_page(0, &style);
        for (index, (_, glyphs)) in columns(&layout).iter().enumerate() {
            let last = glyphs.last().expect("a column has glyphs");
            assert!(
                !is_no_end(last.source),
                "column {index} ends with {:?}",
                last.source
            );
        }
    }

    #[test]
    fn a_chapter_starts_a_new_column_with_a_blank_one_before_it() {
        let style = flat_style();
        let document = Document::from_chapters(vec![
            Chapter {
                title: None,
                paragraphs: vec!["天地玄黄".to_string()],
            },
            Chapter {
                title: Some("第二章".to_string()),
                paragraphs: vec!["宇宙洪荒".to_string()],
            },
        ]);
        let layout = document.layout_page(0, &style);
        let columns = columns(&layout);
        // The first chapter's one column, a blank column (no glyphs, so it
        // does not show up here), the title, then the chapter's first
        // paragraph in a column of its own.
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0].1.len(), 4);
        assert_eq!(columns[1].1.len(), 3);
        assert_eq!(columns[1].0, columns[0].0 - 2 * style.pitch);
        assert_eq!(columns[2].1.len(), 4);
        assert_eq!(columns[2].0, columns[1].0 - style.pitch);
        // The title gets a blank cell above it.
        assert_eq!(columns[1].1[0].y, style.margin_top + style.em);
    }

    #[test]
    fn rules_stay_inside_the_page_and_follow_the_scope() {
        let style = test_style();
        // Short enough to leave empty column slots on the page, which the
        // full-line scope rules as well.
        let text = "天地玄黄".repeat(2);
        let document = document(&[&text]);
        let layout = document.layout_page(0, &style);

        let text_only = rule_lines(&layout, &style, Underline::Solid, UnderlineScope::TextOnly);
        assert_eq!(text_only.len(), columns(&layout).len());
        for rule in &text_only {
            assert!(rule.x >= 0 && rule.x < style.width);
            assert!(rule.top >= 0 && rule.bottom <= style.height);
            assert!(rule.bottom > rule.top);
        }

        let full = rule_lines(&layout, &style, Underline::Solid, UnderlineScope::FullLine);
        assert_eq!(full.len(), layout.columns.len());
        assert!(full.len() > text_only.len());
        for rule in &full {
            assert_eq!(rule.top, style.top());
            assert_eq!(rule.bottom, style.bottom());
            assert!(rule.x >= 0 && rule.x < style.width);
            assert!(rule.bottom <= style.height);
        }

        // Off draws nothing.
        assert!(rule_lines(&layout, &style, Underline::Off, UnderlineScope::FullLine).is_empty());
        // Dotted and solid cover the same columns.
        let dotted = rule_lines(&layout, &style, Underline::Dotted, UnderlineScope::TextOnly);
        assert_eq!(dotted, text_only);
    }

    #[test]
    fn an_empty_document_lays_out_an_empty_page() {
        let style = test_style();
        let document = Document::from_chapters(Vec::new());
        assert!(document.is_empty());
        assert_eq!(document.len(), 0);
        let layout = document.layout_page(0, &style);
        assert!(layout.is_empty());
        assert_eq!(layout.next, 0);
        assert_eq!(document.next_page(0, &style), None);
        assert_eq!(document.page_start(0, &style), 0);
        assert_eq!(document.fraction(0), 0.0);
        assert_eq!(document.location_at_fraction(0.5), 0);
        assert!(document.chapter_of(0).is_none());
        assert!(document.chapter_title(0).is_none());
    }

    #[test]
    fn a_short_chapter_is_one_page() {
        let style = test_style();
        let document = document(&["天地玄黄，宇宙洪荒。"]);
        assert_eq!(document.len(), 10);
        let layout = document.layout_page(0, &style);
        assert_eq!(layout.glyphs.len(), 10);
        assert_eq!(layout.next, 10);
        assert_eq!(document.next_page(0, &style), None);
        assert_eq!(document.page_start(9, &style), 0);
    }

    #[test]
    fn pages_chain_to_the_end_without_gaps_or_overlaps() {
        let style = test_style();
        let text = "天地玄黄".repeat(200);
        let document = document(&[&text]);
        let mut pages = vec![0usize];
        while let Some(next) = document.next_page(*pages.last().unwrap(), &style) {
            assert!(next > *pages.last().unwrap());
            pages.push(next);
        }
        // The first column holds 16 cells (the indent takes two), the other
        // nine hold 18: 178 characters a page.
        assert_eq!(pages, vec![0, 178, 358, 538, 718]);
        assert_eq!(*pages.last().unwrap(), 718);
        assert_eq!(document.len(), 800);
        // Every page starts where the previous one ended and holds its own
        // characters.
        for pair in pages.windows(2) {
            let layout = document.layout_page(pair[0], &style);
            assert_eq!(layout.next, pair[1]);
        }
        // The last page holds the rest of the book.
        let last = document.layout_page(718, &style);
        assert_eq!(last.glyphs.len(), 82);
        assert_eq!(last.next, document.len());
        // And page_start finds every page again.
        for page in &pages {
            assert_eq!(document.page_start(*page, &style), *page);
        }
    }

    #[test]
    fn fraction_and_location_round_trip() {
        let document = document(&["天地玄黄".repeat(100).as_str()]);
        assert_eq!(document.len(), 400);
        assert_eq!(document.fraction(0), 0.0);
        assert_eq!(document.fraction(200), 0.5);
        assert_eq!(document.fraction(400), 1.0);
        assert_eq!(document.location_at_fraction(0.5), 200);
        assert_eq!(document.location_at_fraction(0.0), 0);
        assert_eq!(document.location_at_fraction(1.0), 399);
        for location in [0usize, 1, 99, 200, 399] {
            assert_eq!(
                document.location_at_fraction(document.fraction(location)),
                location
            );
        }
    }

    #[test]
    fn the_weight_maps_onto_three_classes() {
        assert_eq!(Weight::from_font_weight(100), Weight::Light);
        assert_eq!(Weight::from_font_weight(300), Weight::Light);
        assert_eq!(Weight::from_font_weight(399), Weight::Light);
        assert_eq!(Weight::from_font_weight(400), Weight::Normal);
        assert_eq!(Weight::from_font_weight(401), Weight::Bold);
        assert_eq!(Weight::from_font_weight(600), Weight::Bold);
        assert_eq!(Weight::from_font_weight(900), Weight::Bold);
    }

    #[test]
    fn the_style_derives_its_geometry_from_the_panel() {
        let style = Style::pw4();
        let vertical = VerticalStyle::from_style(&style);
        // 10 pt at 300 dpi: 41.67 px, rounded to 42.
        assert_eq!(vertical.em, 42);
        assert_eq!(vertical.pitch, 55);
        assert_eq!(vertical.margin_left, style.margin_px());
        assert_eq!(vertical.margin_right, style.margin_px());
        assert_eq!(vertical.top(), style.text_top as i32);
        assert_eq!(
            vertical.bottom(),
            style.height as i32 - style.text_bottom as i32
        );
        assert_eq!(vertical.indent, 2 * 42);
        assert!(vertical.slots() >= 10);
        // Every column lies inside the text block.
        for index in 0..vertical.slots() {
            assert!(vertical.column_x(index) - vertical.em / 2 >= vertical.margin_left);
            assert!(
                vertical.column_x(index) + vertical.em / 2
                    <= vertical.width - vertical.margin_right
            );
        }
    }

    #[test]
    fn an_epub_yields_its_spine_titles_and_paragraphs() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        use zip::CompressionMethod;

        let path = scratch("vertical.epub");
        {
            let file = fs::File::create(&path).expect("epub file");
            let mut zip = zip::ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            let mut add = |name: &str, data: &str| {
                zip.start_file(name, options).expect("zip member");
                zip.write_all(data.as_bytes()).expect("zip data");
            };
            add("mimetype", "application/epub+zip");
            add(
                "META-INF/container.xml",
                r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
            );
            add(
                "OEBPS/content.opf",
                r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>紅樓夢</dc:title><dc:creator>曹雪芹</dc:creator><dc:language>zh</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"#,
            );
            add(
                "OEBPS/nav.xhtml",
                r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<body><nav epub:type="toc"><ol>
  <li><a href="text/ch1.xhtml">第一回 甄士隱夢幻識通靈</a></li>
  <li><a href="text/ch2.xhtml">第二回 賈夫人仙逝揚州城</a></li>
</ol></nav></body></html>"#,
            );
            add(
                "OEBPS/text/ch1.xhtml",
                r#"<html><body><h1>第一回</h1>
<p>此開卷第一回也。作者自云：因曾歷過一番夢幻之後，故將真事隱去。</p>
<p>列位看官：你道此書從何而來？</p></body></html>"#,
            );
            add(
                "OEBPS/text/ch2.xhtml",
                r#"<html><body><h1>第二回</h1>
<p>詩云：一局輸贏料不真，香銷茶盡尚逡巡。</p></body></html>"#,
            );
            zip.finish().expect("finish the epub");
        }

        let document = Document::open(&path).expect("the epub opens");
        assert_eq!(document.title.as_deref(), Some("紅樓夢"));
        assert_eq!(document.author.as_deref(), Some("曹雪芹"));
        assert_eq!(document.language.as_deref(), Some("zh"));
        assert_eq!(document.chapters.len(), 2);
        assert_eq!(
            document.chapters[0].title.as_deref(),
            Some("第一回 甄士隱夢幻識通靈")
        );
        assert_eq!(document.chapters[0].paragraphs.len(), 2);
        assert!(document.chapters[0].paragraphs[0].starts_with("此開卷第一回也"));
        assert_eq!(document.chapters[1].paragraphs.len(), 1);
        assert_eq!(document.toc().len(), 2);
        assert_eq!(document.toc()[0].title, "第一回 甄士隱夢幻識通靈");
        assert_eq!(document.toc()[0].chapter, 0);
        assert_eq!(document.toc()[1].chapter, 1);
        assert_eq!(document.toc()[1].start, document.chapter_range(1).0);
        assert!(document.len() > 30);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_txt_file_is_decoded_and_split_into_chapters() {
        let path = scratch("vertical.txt");
        let text = "第一章 起\n話說從頭。\n天地玄黃。\n第二章 承\n宇宙洪荒。\n";
        let (bytes, _, _) = encoding_rs::GBK.encode(text);
        fs::write(&path, &bytes).expect("txt file");

        let document = Document::open(&path).expect("the txt opens");
        assert_eq!(document.chapters.len(), 2);
        assert_eq!(document.chapters[0].title.as_deref(), Some("第一章 起"));
        assert_eq!(
            document.chapters[0].paragraphs,
            ["話說從頭。", "天地玄黃。"]
        );
        assert_eq!(document.chapters[1].title.as_deref(), Some("第二章 承"));
        assert_eq!(document.chapters[1].paragraphs, ["宇宙洪荒。"]);
        assert_eq!(document.toc().len(), 2);
        assert_eq!(document.chapter_of(0), Some(0));
        assert_eq!(document.chapter_of(document.chapter_range(1).0), Some(1));
        assert_eq!(
            document
                .chapter_title(document.chapter_range(1).0)
                .as_deref(),
            Some("第二章 承")
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_txt_file_without_headings_is_one_chapter() {
        let path = scratch("vertical-plain.txt");
        fs::write(&path, "天地玄黄。\n宇宙洪荒。\n").expect("txt file");
        let document = Document::open(&path).expect("the txt opens");
        assert_eq!(document.chapters.len(), 1);
        assert_eq!(document.chapters[0].title, None);
        assert_eq!(document.chapters[0].paragraphs.len(), 2);
        assert!(document.toc().is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn hrefs_are_resolved_against_the_package_directory() {
        assert_eq!(
            resolve_href("OEBPS", "text/ch1.xhtml"),
            "OEBPS/text/ch1.xhtml"
        );
        assert_eq!(resolve_href("OEBPS", "../ch1.xhtml"), "ch1.xhtml");
        assert_eq!(resolve_href("", "ch1.xhtml#top"), "ch1.xhtml");
        assert_eq!(
            resolve_href("OEBPS", "./text/../ch1.xhtml"),
            "OEBPS/ch1.xhtml"
        );
        assert_eq!(
            resolve_href("OEBPS", "text/%E7%AC%AC1.xhtml"),
            "OEBPS/text/第1.xhtml"
        );
    }

    #[test]
    fn the_character_map_tells_a_font_that_has_a_glyph_from_one_that_has_not() {
        // The crate's own Latin face: it has A, and no Chinese at all.
        let literata = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("fonts")
            .join("Literata-Regular.ttf");
        assert!(font_covers(&literata, 'A'));
        assert!(font_covers(&literata, 'ł'));
        assert!(!font_covers(&literata, '中'));
        assert!(!font_covers(&literata, '樓'));
        // A missing file, and bytes that are not a font, cover nothing.
        assert!(!font_covers(Path::new("/nonexistent/font.ttf"), 'A'));
        assert!(!covers_cmap(b"not a font at all", 'A'));
        assert!(!covers_cmap(&[], 'A'));
        // The device's CJK face, when the data directory is configured.
        if let Some(data) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
            let data = PathBuf::from(data).join("fonts");
            let cjk = font_files(&data)
                .into_iter()
                .find(|path| font_covers(path, '中'));
            let cjk = cjk.expect("a CJK font in the data directory");
            assert!(font_covers(&cjk, '中'));
            assert!(font_covers(&cjk, '樓'));
        }
    }

    #[test]
    fn a_page_is_drawn_with_the_real_font() {
        let style = Style::pw4();
        if !has_font(&style) {
            eprintln!("no CJK font in the data directory; skipping the raster test");
            return;
        }
        let document = document(&["天地玄黃，宇宙洪荒。《紅樓夢》第一回，1791 年。"]);
        let layout = document.layout_page(0, &VerticalStyle::from_style(&style));
        assert!(!layout.is_empty());
        let gray = render_gray(&style, &layout, Underline::Solid, UnderlineScope::TextOnly)
            .expect("the page renders");
        assert_eq!(gray.width, style.width);
        assert_eq!(gray.height, style.height);
        let dark = gray.data.iter().filter(|&&pixel| pixel < 0x80).count();
        assert!(dark > 200, "the page has glyph pixels ({dark} dark)");
        // The guide is drawn, in the guide's gray.
        let rules = gray
            .data
            .iter()
            .filter(|&&pixel| pixel == UNDERLINE_GRAY)
            .count();
        assert!(rules > 100, "the column rules are drawn ({rules} pixels)");
    }
}
