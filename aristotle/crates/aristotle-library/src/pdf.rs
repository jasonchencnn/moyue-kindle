// SPDX-License-Identifier: AGPL-3.0-or-later
//! Best-effort PDF metadata: the document Info dictionary and the page count.
//!
//! A full PDF crate (lopdf, pdf) is large and parses far more than we need.
//! This module understands just enough of the file structure to find two
//! objects: the classic `xref` table and trailer, PDF 1.5 cross-reference
//! streams and object streams (Flate with PNG predictors, which is what
//! pdfTeX, Word and Ghostscript write), and incremental updates via `/Prev`.
//! When the cross-reference data is damaged it falls back to scanning the
//! tail of the file for the trailer and the body for `N G obj`.
//!
//! Encrypted PDFs (`/Encrypt` in the trailer) encrypt their strings, so for
//! them only the page count is read and the title comes from the file name.
//! XMP metadata is ignored: Info is present in practically every PDF.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::encoding::{decode, Encoding};
use crate::error::{Error, Result};
use crate::metadata::{Author, Extracted};
use crate::text;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Obj {
    Null,
    Bool(bool),
    Num(f64),
    Str(Vec<u8>),
    Name(String),
    Array(Vec<Obj>),
    Dict(Vec<(String, Obj)>),
    Ref(u32, u16),
}

impl Obj {
    fn get(&self, key: &str) -> Option<&Obj> {
        match self {
            Obj::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_u64(&self) -> Option<u64> {
        match self {
            Obj::Num(n) if *n >= 0.0 => Some(*n as u64),
            _ => None,
        }
    }
}

/// Byte-level parser over a slice.
struct Lexer<'a> {
    b: &'a [u8],
    p: usize,
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\n' | b'\r' | b'\t' | b'\x0C' | 0)
}

fn is_delim(c: u8) -> bool {
    is_ws(c)
        || matches!(
            c,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

impl<'a> Lexer<'a> {
    fn new(b: &'a [u8], p: usize) -> Self {
        Lexer { b, p }
    }

    fn skip_ws(&mut self) {
        while self.p < self.b.len() {
            let c = self.b[self.p];
            if is_ws(c) {
                self.p += 1;
            } else if c == b'%' {
                while self.p < self.b.len() && self.b[self.p] != b'\n' && self.b[self.p] != b'\r' {
                    self.p += 1;
                }
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.p).copied()
    }

    fn keyword(&mut self, kw: &[u8]) -> bool {
        self.skip_ws();
        if self.b[self.p..].starts_with(kw)
            && self.b.get(self.p + kw.len()).is_none_or(|&c| is_delim(c))
        {
            self.p += kw.len();
            true
        } else {
            false
        }
    }

    fn token(&mut self) -> &'a [u8] {
        let s = self.p;
        while self.p < self.b.len() && !is_delim(self.b[self.p]) {
            self.p += 1;
        }
        &self.b[s..self.p]
    }

    fn uint(&mut self) -> Option<u64> {
        self.skip_ws();
        let t = self.token();
        if t.is_empty() || !t.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(t).ok()?.parse().ok()
    }

    fn value(&mut self, depth: u32) -> Option<Obj> {
        if depth > 32 {
            return None;
        }
        self.skip_ws();
        let c = self.peek()?;
        match c {
            b'<' if self.b.get(self.p + 1) == Some(&b'<') => {
                self.p += 2;
                let mut d = Vec::new();
                loop {
                    self.skip_ws();
                    if self.b[self.p..].starts_with(b">>") {
                        self.p += 2;
                        return Some(Obj::Dict(d));
                    }
                    let Obj::Name(k) = self.value(depth + 1)? else {
                        return None;
                    };
                    let v = self.value(depth + 1)?;
                    d.push((k, v));
                }
            }
            b'<' => {
                self.p += 1;
                let mut hex = Vec::new();
                while let Some(&c) = self.b.get(self.p) {
                    self.p += 1;
                    if c == b'>' {
                        break;
                    }
                    if let Some(v) = (c as char).to_digit(16) {
                        hex.push(v as u8);
                    }
                }
                if hex.len() % 2 == 1 {
                    hex.push(0);
                }
                Some(Obj::Str(hex.chunks(2).map(|p| p[0] << 4 | p[1]).collect()))
            }
            b'(' => self.literal().map(Obj::Str),
            b'/' => {
                self.p += 1;
                let t = self.token();
                Some(Obj::Name(decode_name(t)))
            }
            b'[' => {
                self.p += 1;
                let mut a = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek()? == b']' {
                        self.p += 1;
                        return Some(Obj::Array(a));
                    }
                    a.push(self.value(depth + 1)?);
                }
            }
            _ => {
                let t = self.token();
                if t.is_empty() {
                    self.p += 1;
                    return None;
                }
                match t {
                    b"true" => return Some(Obj::Bool(true)),
                    b"false" => return Some(Obj::Bool(false)),
                    b"null" => return Some(Obj::Null),
                    _ => {}
                }
                let n: f64 = std::str::from_utf8(t).ok()?.parse().ok()?;
                // "N G R" is a reference; look ahead without consuming otherwise.
                if t.iter().all(u8::is_ascii_digit) {
                    let save = self.p;
                    if let Some(g) = self.uint() {
                        if self.keyword(b"R") {
                            return Some(Obj::Ref(n as u32, g as u16));
                        }
                    }
                    self.p = save;
                }
                Some(Obj::Num(n))
            }
        }
    }

    fn literal(&mut self) -> Option<Vec<u8>> {
        self.p += 1;
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(&c) = self.b.get(self.p) {
            self.p += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(c);
                }
                b'\\' => {
                    let e = *self.b.get(self.p)?;
                    self.p += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.p += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.p += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
        Some(out)
    }
}

fn decode_name(t: &[u8]) -> String {
    let mut out = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if t[i] == b'#' && i + 2 < t.len() {
            if let (Some(h), Some(l)) = (
                (t[i + 1] as char).to_digit(16),
                (t[i + 2] as char).to_digit(16),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(t[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decodes a PDF text string: UTF-16BE or UTF-8 with a BOM, else
/// PDFDocEncoding. Many producers write raw UTF-8 without the BOM (against
/// the spec); a valid non-ASCII UTF-8 string is taken as such, because
/// PDFDocEncoding would turn every Polish letter into two symbols.
pub(crate) fn text_string(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        return decode(b, Encoding::Utf16Be);
    }
    if b.starts_with(&[0xFF, 0xFE]) {
        return decode(b, Encoding::Utf16Le);
    }
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return decode(&b[3..], Encoding::Utf8);
    }
    if !b.is_ascii() {
        if let Ok(s) = std::str::from_utf8(b) {
            return s.to_owned();
        }
    }
    decode(b, Encoding::PdfDoc)
}

#[derive(Clone, Copy, Debug)]
enum XrefEntry {
    Offset(u64),
    InStream(u32, u32),
}

/// A decoded object stream: its data and (object number, offset) pairs.
type ObjStream = (Vec<u8>, Vec<(u32, usize)>);

struct Pdf {
    file: File,
    len: u64,
    xref: HashMap<u32, XrefEntry>,
    trailer: Obj,
    objstm_cache: HashMap<u32, ObjStream>,
}

const TAIL: u64 = 64 * 1024;
const MAX_STREAM: u64 = 16 << 20;

impl Pdf {
    fn open(path: &Path) -> Result<Pdf> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut pdf = Pdf {
            file: file.try_clone()?,
            len,
            xref: HashMap::new(),
            trailer: Obj::Null,
            objstm_cache: HashMap::new(),
        };
        let tail_start = len.saturating_sub(TAIL);
        let tail = read_at(&mut file, tail_start, (len - tail_start) as usize)?;
        let startxref = rfind(&tail, b"startxref").and_then(|i| Lexer::new(&tail, i + 9).uint());
        if let Some(off) = startxref {
            let _ = pdf.load_xref_chain(off);
        }
        if pdf.trailer == Obj::Null {
            // Damaged or missing xref: use the last trailer dictionary in the tail.
            if let Some(i) = rfind(&tail, b"trailer") {
                if let Some(d) = Lexer::new(&tail, i + 7).value(0) {
                    pdf.trailer = d;
                }
            }
        }
        if pdf.trailer == Obj::Null {
            return Err(Error::format("pdf: no trailer"));
        }
        Ok(pdf)
    }

    fn load_xref_chain(&mut self, mut offset: u64) -> Result<()> {
        let mut seen = Vec::new();
        while !seen.contains(&offset) && seen.len() < 64 {
            seen.push(offset);
            let chunk = self.chunk(offset, 256 * 1024)?;
            let mut lx = Lexer::new(&chunk, 0);
            let trailer = if lx.keyword(b"xref") {
                self.parse_xref_table(&mut lx)?
            } else {
                self.parse_xref_stream(offset)?
            };
            // Hybrid files keep the compressed part in a separate stream.
            if let Some(stm) = trailer.get("XRefStm").and_then(Obj::as_u64) {
                let _ = self.parse_xref_stream(stm);
            }
            let prev = trailer.get("Prev").and_then(Obj::as_u64);
            if self.trailer == Obj::Null {
                self.trailer = trailer;
            }
            match prev {
                Some(p) => offset = p,
                None => break,
            }
        }
        Ok(())
    }

    fn parse_xref_table(&mut self, lx: &mut Lexer) -> Result<Obj> {
        loop {
            if lx.keyword(b"trailer") {
                return lx.value(0).ok_or_else(|| Error::format("pdf: bad trailer"));
            }
            let (Some(start), Some(count)) = (lx.uint(), lx.uint()) else {
                return Err(Error::format("pdf: bad xref table"));
            };
            for i in 0..count {
                let (Some(off), Some(_gen)) = (lx.uint(), lx.uint()) else {
                    return Err(Error::format("pdf: bad xref entry"));
                };
                lx.skip_ws();
                let kind = lx.token();
                // Newest section first: keep entries we already have.
                if kind == b"n" {
                    self.xref
                        .entry((start + i) as u32)
                        .or_insert(XrefEntry::Offset(off));
                }
            }
        }
    }

    fn parse_xref_stream(&mut self, offset: u64) -> Result<Obj> {
        let (dict, data) = self.stream_at(offset)?;
        let w: Vec<usize> = match dict.get("W") {
            Some(Obj::Array(a)) => a.iter().map(|v| v.as_u64().unwrap_or(0) as usize).collect(),
            _ => return Err(Error::format("pdf: xref stream without /W")),
        };
        if w.len() != 3 || w.iter().any(|&x| x > 8) {
            return Err(Error::format("pdf: bad /W"));
        }
        let size = dict.get("Size").and_then(Obj::as_u64).unwrap_or(0);
        let index: Vec<u64> = match dict.get("Index") {
            Some(Obj::Array(a)) => a.iter().filter_map(Obj::as_u64).collect(),
            _ => vec![0, size],
        };
        let row = w.iter().sum::<usize>();
        let field = |r: &[u8]| r.iter().fold(0u64, |acc, &b| acc << 8 | b as u64);
        let mut p = 0;
        for pair in index.chunks(2) {
            let [start, count] = pair else { break };
            for i in 0..*count {
                if p + row > data.len() {
                    break;
                }
                let r = &data[p..p + row];
                p += row;
                let t = if w[0] == 0 { 1 } else { field(&r[..w[0]]) };
                let f2 = field(&r[w[0]..w[0] + w[1]]);
                let f3 = field(&r[w[0] + w[1]..]);
                let entry = match t {
                    1 => XrefEntry::Offset(f2),
                    2 => XrefEntry::InStream(f2 as u32, f3 as u32),
                    _ => continue,
                };
                self.xref.entry((start + i) as u32).or_insert(entry);
            }
        }
        Ok(dict)
    }

    fn chunk(&mut self, offset: u64, max: u64) -> Result<Vec<u8>> {
        if offset >= self.len {
            return Err(Error::format("pdf: offset past end"));
        }
        let n = (self.len - offset).min(max) as usize;
        read_at(&mut self.file, offset, n)
    }

    /// Parses "N G obj <value>" at `offset`, returning the value and the
    /// position just after it within the chunk read.
    fn object_at(&mut self, offset: u64) -> Result<(Obj, Vec<u8>, usize)> {
        for size in [8 * 1024, 256 * 1024] {
            let chunk = self.chunk(offset, size)?;
            let mut lx = Lexer::new(&chunk, 0);
            if lx.uint().is_none() || lx.uint().is_none() || !lx.keyword(b"obj") {
                return Err(Error::format("pdf: no object at offset"));
            }
            if let Some(v) = lx.value(0) {
                let end = lx.p;
                return Ok((v, chunk, end));
            }
        }
        Err(Error::format("pdf: unparsable object"))
    }

    fn stream_at(&mut self, offset: u64) -> Result<(Obj, Vec<u8>)> {
        let (dict, raw) = self.raw_stream_at(offset)?;
        let data = decode_stream(&dict, raw)?;
        Ok((dict, data))
    }

    /// The stream's bytes before its filters are undone, for the formats
    /// that are stored as they are (a JPEG cover).
    fn raw_stream_at(&mut self, offset: u64) -> Result<(Obj, Vec<u8>)> {
        let (dict, chunk, end) = self.object_at(offset)?;
        let mut lx = Lexer::new(&chunk, end);
        if !lx.keyword(b"stream") {
            return Err(Error::format("pdf: expected stream"));
        }
        // The keyword is followed by CRLF or LF.
        if chunk.get(lx.p) == Some(&b'\r') {
            lx.p += 1;
        }
        if chunk.get(lx.p) == Some(&b'\n') {
            lx.p += 1;
        }
        let start = offset + lx.p as u64;
        let length = match dict.get("Length") {
            Some(Obj::Ref(n, _)) => {
                let n = *n;
                self.resolve_ref(n, 1)?.as_u64()
            }
            Some(v) => v.as_u64(),
            None => None,
        }
        .ok_or_else(|| Error::format("pdf: stream without length"))?;
        if length > MAX_STREAM || start + length > self.len {
            return Err(Error::format("pdf: stream too long"));
        }
        let raw = read_at(&mut self.file, start, length as usize)?;
        Ok((dict, raw))
    }

    fn resolve(&mut self, o: &Obj) -> Option<Obj> {
        match o {
            Obj::Ref(n, _) => self.resolve_ref(*n, 0).ok(),
            other => Some(other.clone()),
        }
    }

    fn resolve_ref(&mut self, num: u32, depth: u32) -> Result<Obj> {
        if depth > 4 {
            return Err(Error::format("pdf: reference loop"));
        }
        let entry = match self.xref.get(&num).copied() {
            Some(e) => e,
            None => XrefEntry::Offset(self.scan_for_object(num)?),
        };
        match entry {
            XrefEntry::Offset(off) => match self.object_at(off) {
                Ok((v, _, _)) => Ok(v),
                // A stale offset: try the body scan before giving up.
                Err(_) => {
                    let off = self.scan_for_object(num)?;
                    Ok(self.object_at(off)?.0)
                }
            },
            XrefEntry::InStream(stm, idx) => {
                if !self.objstm_cache.contains_key(&stm) {
                    let Some(XrefEntry::Offset(off)) = self.xref.get(&stm).copied() else {
                        return Err(Error::format("pdf: object stream not found"));
                    };
                    let (dict, data) = self.stream_at(off)?;
                    let n = dict.get("N").and_then(Obj::as_u64).unwrap_or(0);
                    let first = dict.get("First").and_then(Obj::as_u64).unwrap_or(0) as usize;
                    let mut lx = Lexer::new(&data, 0);
                    let mut table = Vec::new();
                    for _ in 0..n.min(100_000) {
                        let (Some(o), Some(rel)) = (lx.uint(), lx.uint()) else {
                            break;
                        };
                        table.push((o as u32, first + rel as usize));
                    }
                    self.objstm_cache.insert(stm, (data, table));
                }
                let (data, table) = &self.objstm_cache[&stm];
                let at = table
                    .get(idx as usize)
                    .filter(|(o, _)| *o == num)
                    .or_else(|| table.iter().find(|(o, _)| *o == num))
                    .map(|(_, p)| *p)
                    .ok_or_else(|| Error::format("pdf: object not in its stream"))?;
                Lexer::new(data, at)
                    .value(0)
                    .ok_or_else(|| Error::format("pdf: bad object in stream"))
            }
        }
    }

    /// Where object `num`'s body starts: the cross-reference table's offset
    /// when it leads to an object, else the body scan. A damaged table (a
    /// side-loaded PDF with a stale `startxref`, a truncated update) leaves
    /// the object readable, but not at the offset it claims.
    fn stream_offset(&mut self, num: u32) -> Result<u64> {
        if let Some(XrefEntry::Offset(off)) = self.xref.get(&num).copied() {
            if self.object_at(off).is_ok() {
                return Ok(off);
            }
        }
        // A stream cannot live inside an object stream, so an `InStream`
        // entry cannot be the offset of one; the body is scanned instead.
        self.scan_for_object(num)
    }

    /// Last resort: find "N 0 obj" in the body, reading in 1 MiB windows.
    fn scan_for_object(&mut self, num: u32) -> Result<u64> {
        let needle = format!("{num} 0 obj");
        let needle = needle.as_bytes();
        let mut found = None;
        let mut off = 0u64;
        while off < self.len {
            let chunk = self.chunk(off, 1 << 20)?;
            let mut from = 0;
            while let Some(i) = find(&chunk[from..], needle) {
                let at = from + i;
                if at == 0 && off == 0 || at > 0 && is_ws(chunk[at - 1]) {
                    found = Some(off + at as u64); // keep the last: incremental updates append
                }
                from = at + 1;
            }
            if chunk.len() < 1 << 20 {
                break;
            }
            off += (chunk.len() - needle.len()) as u64;
        }
        found.ok_or_else(|| Error::format("pdf: object not found"))
    }
}

fn decode_stream(dict: &Obj, raw: Vec<u8>) -> Result<Vec<u8>> {
    let filters: Vec<String> = match dict.get("Filter") {
        Some(Obj::Name(n)) => vec![n.clone()],
        Some(Obj::Array(a)) => a
            .iter()
            .filter_map(|f| {
                if let Obj::Name(n) = f {
                    Some(n.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => vec![],
    };
    let mut data = raw;
    for f in &filters {
        match f.as_str() {
            "FlateDecode" | "Fl" => {
                data = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
                    &data,
                    MAX_STREAM as usize,
                )
                .or_else(|_| {
                    miniz_oxide::inflate::decompress_to_vec_with_limit(&data, MAX_STREAM as usize)
                })
                .map_err(|_| Error::format("pdf: bad flate stream"))?;
            }
            other => return Err(Error::format(format!("pdf: unsupported filter {other}"))),
        }
    }
    let parms = match dict.get("DecodeParms").or_else(|| dict.get("DP")) {
        Some(Obj::Array(a)) => a.first().cloned(),
        other => other.cloned(),
    };
    if let Some(p) = parms {
        let predictor = p.get("Predictor").and_then(Obj::as_u64).unwrap_or(1);
        if predictor >= 10 {
            let columns = p.get("Columns").and_then(Obj::as_u64).unwrap_or(1) as usize;
            let colors = p.get("Colors").and_then(Obj::as_u64).unwrap_or(1) as usize;
            let bpc = p.get("BitsPerComponent").and_then(Obj::as_u64).unwrap_or(8) as usize;
            data = png_unpredict(
                &data,
                (columns * colors * bpc).div_ceil(8),
                (colors * bpc).div_ceil(8).max(1),
            );
        }
    }
    Ok(data)
}

/// Undoes PNG row filters (PDF predictors 10–15).
fn png_unpredict(data: &[u8], row: usize, bpp: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        if chunk.len() < row + 1 {
            break;
        }
        let (kind, src) = (chunk[0], &chunk[1..]);
        let mut cur = vec![0u8; row];
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] as i16 } else { 0 };
            let b = prev[i] as i16;
            let c = if i >= bpp { prev[i - bpp] as i16 } else { 0 };
            let pred = match kind {
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => 0,
            };
            cur[i] = src[i].wrapping_add(pred as u8);
        }
        out.extend_from_slice(&cur);
        prev = cur;
    }
    out
}

fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn rfind(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).rposition(|w| w == n)
}

pub fn extract(path: &Path, want_cover: bool) -> Result<Extracted> {
    let mut pdf = Pdf::open(path)?;
    let mut ex = Extracted::default();
    let trailer = pdf.trailer.clone();
    let encrypted = trailer.get("Encrypt").is_some();
    if !encrypted {
        if let Some(Obj::Dict(info)) = trailer.get("Info").and_then(|i| pdf.resolve(i)) {
            let info = Obj::Dict(info);
            let mut field = |k: &str| -> Option<String> {
                match info.get(k).and_then(|v| pdf.resolve(v)) {
                    Some(Obj::Str(s)) => {
                        Some(text::clean(&text_string(&s))).filter(|s| !s.is_empty())
                    }
                    _ => None,
                }
            };
            ex.meta.title = field("Title").unwrap_or_default();
            if let Some(a) = field("Author") {
                ex.meta.authors = a.split([';', '&']).map(|n| Author::new(n, None)).collect();
            }
            ex.meta.description = field("Subject");
            if let Some(k) = field("Keywords") {
                ex.meta.subjects = k
                    .split([',', ';'])
                    .map(text::clean)
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        // Word and LaTeX often leave placeholder titles; they are worse than the file name.
        if is_junk_title(&ex.meta.title) {
            ex.meta.title.clear();
        }
    }
    if want_cover && !encrypted {
        ex.cover = first_page_image(&mut pdf, &trailer);
    }
    ex.meta.pages = page_count(&mut pdf, &trailer);
    Ok(ex)
}

/// The image of the first page, when the page is a scan or a picture: the
/// cover of a comic or a scanned book. Page one is what a PDF reader shows
/// as the book's cover; a page set from type has no image, and the book
/// gets the generated cover instead.
///
/// Rasterising the page would need a PDF renderer; `aristotle-library` stays
/// pure Rust (a static build without a C toolchain), so the largest image
/// object of page one is returned as it is stored — a JPEG stream is handed
/// back undecoded, a Flate one is re-wrapped as a PNG.
fn first_page_image(pdf: &mut Pdf, trailer: &Obj) -> Option<Vec<u8>> {
    let root = pdf.resolve(trailer.get("Root")?)?;
    let mut page = pdf.resolve(root.get("Pages")?)?;
    let mut resources = None;
    for _ in 0..16 {
        if let Some(r) = page.get("Resources") {
            resources = pdf.resolve(r);
        }
        if page
            .get("Type")
            .is_some_and(|t| t == &Obj::Name("Page".into()))
        {
            break;
        }
        let kids = page.get("Kids")?;
        let first = match pdf.resolve(kids)? {
            Obj::Array(a) => a.into_iter().next()?,
            _ => return None,
        };
        page = pdf.resolve(&first)?;
    }
    if page
        .get("Type")
        .is_none_or(|t| t != &Obj::Name("Page".into()))
    {
        return None;
    }
    let xobjects = pdf.resolve(resources?.get("XObject")?)?;
    let Obj::Dict(entries) = xobjects else {
        return None;
    };
    let mut best: Option<(u64, u32)> = None;
    for (_, v) in &entries {
        let num = match v {
            Obj::Ref(n, _) => *n,
            _ => continue,
        };
        let Ok(Obj::Dict(d)) = pdf.resolve_ref(num, 1) else {
            continue;
        };
        if d.iter()
            .any(|(k, v)| k == "Subtype" && v == &Obj::Name("Image".into()))
        {
            let n = |k: &str| d.iter().find(|(a, _)| a == k).and_then(|(_, v)| v.as_u64());
            let area = n("Width")
                .unwrap_or(0)
                .saturating_mul(n("Height").unwrap_or(0));
            if area > best.map_or(0, |(a, _)| a) {
                best = Some((area, num));
            }
        }
    }
    let (area, num) = best?;
    // A logo or a rule is not a cover; a 300 × 400 page image at 300 ppi is.
    if !(100_000..=crate::cover::MAX_PIXELS).contains(&area) {
        return None;
    }
    let offset = pdf.stream_offset(num).ok()?;
    let (dict, raw) = pdf.raw_stream_at(offset).ok()?;
    let filter = match dict.get("Filter") {
        Some(Obj::Name(n)) => n.as_str(),
        Some(Obj::Array(a)) => match a.first() {
            Some(Obj::Name(n)) => n.as_str(),
            _ => return None,
        },
        None => "",
        _ => return None,
    };
    match filter {
        // Handed to the cover pipeline as it is: it decodes JPEG itself, at
        // the scale the thumbnail needs.
        "DCTDecode" | "DCT" if crate::cover::sniff_image(&raw).is_some() => Some(raw),
        "FlateDecode" | "Fl" => {
            let color = match dict.get("ColorSpace") {
                Some(Obj::Name(n)) => n.as_str(),
                _ => return None,
            };
            let (colors, _bpc) = match color {
                "DeviceGray" | "G" => (1, 8),
                "DeviceRGB" | "RGB" => (3, 8),
                _ => return None,
            };
            if dict.get("BitsPerComponent").and_then(Obj::as_u64) != Some(8) {
                return None;
            }
            let (w, h) = (
                dict.get("Width").and_then(Obj::as_u64)? as u32,
                dict.get("Height").and_then(Obj::as_u64)? as u32,
            );
            if w as u64 * h as u64 > crate::cover::MAX_PIXELS {
                return None;
            }
            let pixels = decode_stream(&dict, raw).ok()?;
            if pixels.len() != (w as usize * h as usize * colors) {
                return None;
            }
            encode_png(w, h, colors, &pixels)
        }
        _ => None,
    }
}

/// Wraps raw 8-bit gray or RGB samples as a PNG, the format the cover
/// pipeline decodes besides JPEG.
fn encode_png(w: u32, h: u32, colors: usize, pixels: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(if colors == 1 {
            png::ColorType::Grayscale
        } else {
            png::ColorType::Rgb
        });
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(pixels).ok()?;
    }
    Some(out)
}

fn page_count(pdf: &mut Pdf, trailer: &Obj) -> Option<u32> {
    let root = pdf.resolve(trailer.get("Root")?)?;
    let pages = pdf.resolve(root.get("Pages")?)?;
    pages.get("Count").and_then(Obj::as_u64).map(|n| n as u32)
}

fn is_junk_title(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    l.is_empty()
        || l == "untitled"
        || l.starts_with("microsoft word - ")
        || l.ends_with(".doc")
        || l.ends_with(".docx")
        || l.ends_with(".dvi")
        || l.ends_with(".tex")
}

#[cfg(test)]
pub(crate) mod testutil {
    /// A small classic PDF with an Info dictionary and an xref table.
    pub fn classic(title: &[u8], author: &[u8]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offs = Vec::new();
        let objs: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [] /Count 3 >>".to_vec(),
            [
                b"<< /Title ".as_slice(),
                title,
                b" /Author ",
                author,
                b" /Producer (test) >>",
            ]
            .concat(),
        ];
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// A PDF 1.5 file whose Info dictionary lives in a compressed object
    /// stream, indexed by a cross-reference stream with a PNG Up predictor.
    pub fn compressed(info: &[u8]) -> Vec<u8> {
        let mut out = b"%PDF-1.5\n".to_vec();
        let o1 = out.len();
        out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let o2 = out.len();
        out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 7 >>\nendobj\n");
        // Object stream 4 holds object 3 (the Info dict).
        let header = b"3 0 ";
        let body = [header.as_slice(), info].concat();
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&body, 6);
        let o4 = out.len();
        out.extend_from_slice(format!("4 0 obj\n<< /Type /ObjStm /N 1 /First {} /Filter /FlateDecode /Length {} >>\nstream\n", header.len(), z.len()).as_bytes());
        out.extend_from_slice(&z);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        let o5 = out.len();
        // Rows: type(1) offset(4) gen(1), PNG Up predictor.
        let rows: Vec<[u8; 6]> = vec![
            [0, 0, 0, 0, 0, 255],
            [1, 0, 0, 0, 0, 0],
            [1, 0, 0, 0, 0, 0],
            [2, 0, 0, 0, 4, 0],
            [1, 0, 0, 0, 0, 0],
            [1, 0, 0, 0, 0, 0],
        ];
        let mut rows = rows;
        for (r, off) in [(1, o1), (2, o2), (4, o4), (5, o5)] {
            rows[r][1..5].copy_from_slice(&(off as u32).to_be_bytes());
        }
        let mut raw = Vec::new();
        let mut prev = [0u8; 6];
        for r in &rows {
            raw.push(2);
            for i in 0..6 {
                raw.push(r[i].wrapping_sub(prev[i]));
            }
            prev = *r;
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        out.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /XRef /Size 6 /W [1 4 1] /Root 1 0 R /Info 3 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 6 >> /Length {} >>\nstream\n",
                z.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&z);
        out.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{o5}\n%%EOF\n").as_bytes());
        out
    }
    /// A one-page PDF whose page shows a single image object (object 4).
    pub fn one_page_image(filter: &str, w: u32, h: u32, color: &str, data: &[u8]) -> Vec<u8> {
        let head = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /{color} /BitsPerComponent 8 /Filter /{filter} /Length {} >>",
            data.len()
        );
        let mut image = head.into_bytes();
        image.extend_from_slice(b"\nstream\n");
        image.extend_from_slice(data);
        image.extend_from_slice(b"\nendstream");
        build(vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /XObject << /Im0 4 0 R >> >> >>".to_vec(),
            image,
        ])
    }

    /// A PDF around objects 1..=n, with a classic cross-reference table.
    pub fn build(objs: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        let mut v = b"<FEFF".to_vec();
        for u in s.encode_utf16() {
            v.extend_from_slice(format!("{u:04X}").as_bytes());
        }
        v.push(b'>');
        v
    }

    #[test]
    fn classic_info() {
        let dir = crate::testdir("pdf");
        let p = dir.join("a.pdf");
        std::fs::write(
            &p,
            testutil::classic(
                &utf16("Liczby zespolone — wykład"),
                b"(Jan Kowalski \\(red.\\051 \\101)",
            ),
        )
        .unwrap();
        let ex = extract(&p, false).unwrap();
        assert_eq!(ex.meta.title, "Liczby zespolone — wykład");
        assert_eq!(ex.meta.authors[0].name, "Jan Kowalski (red.) A");
        assert_eq!(ex.meta.pages, Some(3));
    }

    #[test]
    fn object_stream_info() {
        let dir = crate::testdir("pdf");
        let p = dir.join("b.pdf");
        let info = "<< /Title (Łódź nocą) /Author (Anna Nowak) >>".as_bytes();
        std::fs::write(&p, testutil::compressed(info)).unwrap();
        let ex = extract(&p, false).unwrap();
        assert_eq!(ex.meta.title, "Łódź nocą");
        assert_eq!(ex.meta.authors[0].sort, "Nowak, Anna");
        assert_eq!(ex.meta.pages, Some(7));
    }

    #[test]
    fn strings() {
        let mut lx = Lexer::new(b"(a\\(b\\)c (nested) \\101\\\nd)", 0);
        assert_eq!(lx.value(0), Some(Obj::Str(b"a(b)c (nested) Ad".to_vec())));
        let mut lx = Lexer::new(b"<< /A [1 0 R 2.5 /N#20x] /B <4142> >>", 0);
        let d = lx.value(0).unwrap();
        assert_eq!(
            d.get("A"),
            Some(&Obj::Array(vec![
                Obj::Ref(1, 0),
                Obj::Num(2.5),
                Obj::Name("N x".into())
            ]))
        );
        assert_eq!(d.get("B"), Some(&Obj::Str(b"AB".to_vec())));
        assert_eq!(text_string(b"\x95\xf3d"), "Łód");
        assert_eq!(text_string("Łódź".as_bytes()), "Łódź");
    }

    #[test]
    fn first_page_image_becomes_the_cover() {
        let dir = crate::testdir("pdf");
        let p = dir.join("comic.pdf");
        // A 300 × 400 RGB image stored with Flate, the way a scan is kept.
        let (w, h) = (300u32, 400u32);
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 90]);
            }
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6);
        std::fs::write(
            &p,
            testutil::one_page_image("FlateDecode", w, h, "DeviceRGB", &z),
        )
        .unwrap();
        let ex = extract(&p, true).unwrap();
        let cover = ex.cover.expect("the page image is the cover");
        assert_eq!(
            crate::cover::sniff_image(&cover),
            Some(crate::cover::ImageKind::Png)
        );
        let g = crate::cover::decode_gray(&cover, 100).unwrap();
        assert_eq!((g.width, g.height), (w, h));
        // Without the request, the page image is not read.
        assert!(extract(&p, false).unwrap().cover.is_none());
    }

    /// The cross-reference table of a side-loaded PDF is often stale: the
    /// trailer is still there, the table is not. The objects are then found
    /// by scanning the body, and the page image is still a cover.
    #[test]
    fn a_damaged_cross_reference_still_finds_the_page_image() {
        let dir = crate::testdir("pdf");
        let p = dir.join("broken-xref.pdf");
        let (w, h) = (300u32, 400u32);
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 90]);
            }
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6);
        let mut bytes = testutil::one_page_image("FlateDecode", w, h, "DeviceRGB", &z);
        // `startxref` past the end of the file: the table cannot be read, so
        // the trailer comes from the tail and the objects from the body.
        let at = rfind(&bytes, b"startxref").unwrap();
        bytes.truncate(at);
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        std::fs::write(&p, &bytes).unwrap();
        let cover = extract(&p, true)
            .unwrap()
            .cover
            .expect("the page image is found without the table");
        let g = crate::cover::decode_gray(&cover, 100).unwrap();
        assert_eq!((g.width, g.height), (w, h));
    }

    #[test]
    fn jpeg_page_image_is_passed_through() {
        let dir = crate::testdir("pdf");
        let p = dir.join("scan.pdf");
        // The cover pipeline decodes JPEG itself; only the header matters here.
        let jpeg: Vec<u8> = [&[0xFF, 0xD8, 0xFF, 0xE0][..], &[0u8; 512]].concat();
        std::fs::write(
            &p,
            testutil::one_page_image("DCTDecode", 1200, 1800, "DeviceRGB", &jpeg),
        )
        .unwrap();
        assert_eq!(extract(&p, true).unwrap().cover.as_deref(), Some(&jpeg[..]));
    }

    #[test]
    fn a_page_of_type_has_no_cover() {
        let dir = crate::testdir("pdf");
        let p = dir.join("text.pdf");
        let text = testutil::build(vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>".to_vec(),
        ]);
        std::fs::write(&p, text).unwrap();
        assert!(extract(&p, true).unwrap().cover.is_none());
    }

    #[test]
    fn a_logo_is_not_a_cover() {
        let dir = crate::testdir("pdf");
        let p = dir.join("logo.pdf");
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&vec![255u8; 40 * 40 * 3], 6);
        std::fs::write(
            &p,
            testutil::one_page_image("FlateDecode", 40, 40, "DeviceRGB", &z),
        )
        .unwrap();
        assert!(extract(&p, true).unwrap().cover.is_none());
    }
}
