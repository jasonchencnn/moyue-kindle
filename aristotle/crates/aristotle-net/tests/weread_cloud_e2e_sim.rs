// SPDX-License-Identifier: AGPL-3.0-or-later
//! Adversarial end-to-end simulation test suite for the WeRead (微信读书) cloud
//! library, multi-batch encrypted EPUB download, progress sync, reading-time
//! upload, and notes mapping stack over real loopback TCP + `ReadestTransport`.
//!
//! Scenarios covered:
//! 1. E-Ink QR login, `-2012` token refresh preserving omitted `refreshToken`,
//!    and Web QR login (`getLoginUid` -> `getLoginInfo` OTP -> `CookieJar`).
//! 2. `/shelf/sync` wire quirks (integer booleans, `archive`, `synckey`,
//!    multiple formats `epub`/`txt`/`pdf`).
//! 3. 25-chapter multi-batch illustrated book download reproducing real WeRead
//!    traps: ZipCrypto envelopes, obfuscated filenames, batch 2..5 per-chapter
//!    images/styles, `ErrLossFileSourcePre.html`, shared XHTML across chapters,
//!    conflicting shared asset bytes, HTML named entities + bare `&` + `<style>`,
//!    dangling external `tar` images, identical duplicate ZIP entries, EPUB 2
//!    integrity, `BookMappingStore`, `PositionMap` & `AnchorMap` round-trips.
//! 4. Batch HTTP 500 single-chapter fallback without double-counted progress,
//!    fatal `-2063`/`-10102` immediate short-circuit, and poisoned cache purge.
//! 5. Progress sync (`getProgress` -> `PositionMap`/`AnchorMap` -> XPointer),
//!    `arbitrate` + `WriteFence`, Web `reader_state` + `enter`/`period` writes
//!    and `-2012` `WebAuthExpired`, signed `batchUploadProgress`, and
//!    `bookmarklist`/`review/list` -> `map_notes`.
//! 6. 60-chapter large-book speed and batch efficiency stress test.
//! 7. Real KO2 Calibre root `coverpage.html`, alternate folders (`.htm`/lowercase
//!    `xhtml/`/`images/`/`Fonts/`/`css/`), nested `OEBPS/Text/ch3.xhtml`, and
//!    dirty HTML void tags + named entities + bare `&`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aristotle_net::{Policy, ReadestTransport};
use inkread_core::hash::partial_md5;
use inkread_core::sync::{HttpRequest, HttpTransport};
use inkread_core::weread::auth::{
    build_login_body, build_login_request, build_qrconnect_url, parse_login_response,
    parse_qr_ticket, parse_qrconnect_response, poll_status, QrPollStatus, DEFAULT_DEVICE_NAME,
    WX_APPID,
};
use inkread_core::weread::download::{
    download_book_with_progress, BookMappingStore, DownloadError, DownloadRequest, SourceFormat,
};
use inkread_core::weread::epub::EPUB_MIMETYPE;
use inkread_core::weread::mock::make_envelope;
use inkread_core::weread::notes::{BookNotes, DegradeReason, NoteAnchor, NoteKind};
use inkread_core::weread::position::{AnchorMap, EpubPosition, PositionMap, Precision};
use inkread_core::weread::readtime::{intervals_for, ReadtimeQueue, StaySpan, READ_TIME_ZONE};
use inkread_core::weread::web::{
    enter_payload, read_payload, sign, sorted_query, we, web_app_id, QueryValue, ReadPayloadInput,
    WeReadWebClient, WebLoginPoll, WebPayload, WebSession, WEB_USER_AGENT,
};
use inkread_core::weread::{
    arbitrate, inner_signature, map_notes, outer_signature, BatchUploadRequest, ChapterInfo,
    ChapterInfos, Credentials, FenceState, PositionStamp, SyncBaseline, Verdict, WeReadClient,
    WeReadCoordinate, WeReadError, WriteFence,
};
use roxmltree::{Document, ParsingOptions};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TEST_VID: &str = "1234567890";
const TEST_DEVICE_ID: &str = "eink-ko2-e2e-0001";
const TEST_INSTALL_ID: &str = "eink31000000000000000000000042";
const TEST_ZIP_PASSWORD: &str = "e2e-zipcrypto-secret-pass";
const TEST_CONFIG_TOKEN: &str = "e2e-config-token-99";
const TEST_GUEST_TOKEN: &str = "e2e-guest-token-88";
const TEST_WEB_READER_TOKEN: &str = "e2e-web-reader-token-777";
const TEST_PSVTS: &str = "psvts-e2e-state-01";
const TEST_PCLTS: &str = "pclts-e2e-state-02";

// ---------------------------------------------------------------------------
// Minimal pure-Rust CRC-32 + PKWARE ZipCrypto writer (for encrypted test ZIPs)
// ---------------------------------------------------------------------------

const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut c = i;
        let mut j = 0;
        while j < 8 {
            if (c & 1) != 0 {
                c = 0xEDB8_8320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
            j += 1;
        }
        table[i as usize] = c;
        i += 1;
    }
    table
}

const CRC32_TABLE: [u32; 256] = build_crc32_table();

fn crc32_update_byte(crc: u32, byte: u8) -> u32 {
    (crc >> 8) ^ CRC32_TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize]
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc = crc32_update_byte(crc, b);
    }
    !crc
}

struct ZipCrypto {
    k0: u32,
    k1: u32,
    k2: u32,
}

impl ZipCrypto {
    fn new(password: &str) -> Self {
        let mut state = Self {
            k0: 0x1234_5678,
            k1: 0x2345_6789,
            k2: 0x3456_7890,
        };
        for &b in password.as_bytes() {
            state.update_keys(b);
        }
        state
    }

    fn update_keys(&mut self, byte: u8) {
        self.k0 = crc32_update_byte(self.k0, byte);
        self.k1 = self
            .k1
            .wrapping_add(self.k0 & 0xFF)
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        self.k2 = crc32_update_byte(self.k2, (self.k1 >> 24) as u8);
    }

    fn decrypt_byte(&self) -> u8 {
        let temp = (self.k2 & 0xFFFF) | 2;
        ((temp.wrapping_mul(temp ^ 1)) >> 8) as u8
    }

    fn encrypt_byte(&mut self, plain: u8) -> u8 {
        let keystream = self.decrypt_byte();
        self.update_keys(plain);
        plain ^ keystream
    }
}

/// Build a valid PKWARE ZipCrypto-encrypted ZIP archive (method 0 / Stored)
/// containing the exact ordered `entries` (allowing intentional duplicate names).
fn build_zipcrypto_zip(entries: &[(&str, &[u8])], password: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central_dir = Vec::new();

    for (idx, (name, data)) in entries.iter().enumerate() {
        let name_bytes = name.as_bytes();
        let data_crc = crc32(data);
        let mut crypto = ZipCrypto::new(password);

        let mut encrypted = Vec::with_capacity(12 + data.len());
        let seed = (idx as u8).wrapping_mul(31).wrapping_add(7);
        for i in 0u8..11 {
            let salt = seed.wrapping_add(i.wrapping_mul(17));
            encrypted.push(crypto.encrypt_byte(salt));
        }
        let check_byte = ((data_crc >> 24) & 0xFF) as u8;
        encrypted.push(crypto.encrypt_byte(check_byte));
        for &b in *data {
            encrypted.push(crypto.encrypt_byte(b));
        }

        let local_offset = out.len() as u32;
        let flags: u16 = 0x0801; // Encrypted + UTF-8 filename
        let comp_size = encrypted.len() as u32;
        let uncomp_size = data.len() as u32;
        let name_len = name_bytes.len() as u16;

        // Local File Header
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // Stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x5421u16.to_le_bytes()); // mod date
        out.extend_from_slice(&data_crc.to_le_bytes());
        out.extend_from_slice(&comp_size.to_le_bytes());
        out.extend_from_slice(&uncomp_size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&encrypted);

        // Central Directory Header
        central_dir.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        central_dir.extend_from_slice(&((3u16 << 8) | 20u16).to_le_bytes()); // Unix 2.0
        central_dir.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central_dir.extend_from_slice(&flags.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // Stored
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // mod time
        central_dir.extend_from_slice(&0x5421u16.to_le_bytes()); // mod date
        central_dir.extend_from_slice(&data_crc.to_le_bytes());
        central_dir.extend_from_slice(&comp_size.to_le_bytes());
        central_dir.extend_from_slice(&uncomp_size.to_le_bytes());
        central_dir.extend_from_slice(&name_len.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // extra len
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // comment len
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // disk start
        central_dir.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central_dir.extend_from_slice(&(0o100644u32 << 16).to_le_bytes()); // external attrs
        central_dir.extend_from_slice(&local_offset.to_le_bytes());
        central_dir.extend_from_slice(name_bytes);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central_dir.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central_dir);

    // End of Central Directory
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

// ---------------------------------------------------------------------------
// Minimal pure-Rust ZIP + RFC 1951 Inflate reader (for verifying rebuilt EPUBs)
// ---------------------------------------------------------------------------

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit_buf: u32,
    bit_count: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            bit_buf: 0,
            bit_count: 0,
        }
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, String> {
        while self.bit_count < count {
            let byte = *self
                .data
                .get(self.pos)
                .ok_or_else(|| "unexpected EOF in deflate stream".to_string())?;
            self.pos += 1;
            self.bit_buf |= u32::from(byte) << self.bit_count;
            self.bit_count += 8;
        }
        let mask = (1u32 << count) - 1;
        let val = self.bit_buf & mask;
        self.bit_buf >>= count;
        self.bit_count -= count;
        Ok(val)
    }

    fn align_byte(&mut self) {
        self.bit_buf = 0;
        self.bit_count = 0;
    }

    fn read_u16_le(&mut self) -> Result<u16, String> {
        self.align_byte();
        let lo = *self
            .data
            .get(self.pos)
            .ok_or_else(|| "unexpected EOF reading u16 lo".to_string())?;
        let hi = *self
            .data
            .get(self.pos + 1)
            .ok_or_else(|| "unexpected EOF reading u16 hi".to_string())?;
        self.pos += 2;
        Ok(u16::from_le_bytes([lo, hi]))
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], String> {
        self.align_byte();
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| "slice length overflow".to_string())?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| "unexpected EOF reading stored block".to_string())?;
        self.pos = end;
        Ok(slice)
    }
}

struct HuffmanTree {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl HuffmanTree {
    fn from_lengths(lengths: &[u8]) -> Result<Self, String> {
        let mut counts = [0u16; 16];
        for &len in lengths {
            if len as usize >= counts.len() {
                return Err(format!("invalid huffman bit length {len}"));
            }
            if len > 0 {
                counts[len as usize] += 1;
            }
        }
        let mut offsets = [0u16; 16];
        let mut total = 0u16;
        for len in 1..16 {
            offsets[len] = total;
            total += counts[len];
        }
        let mut symbols = vec![0u16; total as usize];
        for (sym, &len) in lengths.iter().enumerate() {
            if len > 0 {
                let slot = offsets[len as usize] as usize;
                offsets[len as usize] += 1;
                symbols[slot] = sym as u16;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, String> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0usize;
        for len in 1..=15 {
            code |= reader.read_bits(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return Ok(self.symbols[index + (code - first) as usize]);
            }
            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("invalid huffman code".to_string())
    }
}

fn inflate_raw(compressed: &[u8]) -> Result<Vec<u8>, String> {
    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    const CL_ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    let mut reader = BitReader::new(compressed);
    let mut out = Vec::new();

    loop {
        let bfinal = reader.read_bits(1)?;
        let btype = reader.read_bits(2)?;
        match btype {
            0 => {
                let len = reader.read_u16_le()?;
                let nlen = reader.read_u16_le()?;
                if len != !nlen {
                    return Err("stored block len/nlen mismatch".to_string());
                }
                out.extend_from_slice(reader.read_slice(len as usize)?);
            }
            1 | 2 => {
                let (litlen_tree, dist_tree) = if btype == 1 {
                    let mut lit_lens = [0u8; 288];
                    lit_lens[..=143].fill(8);
                    lit_lens[144..=255].fill(9);
                    lit_lens[256..=279].fill(7);
                    lit_lens[280..=287].fill(8);
                    let dist_lens = [5u8; 32];
                    (
                        HuffmanTree::from_lengths(&lit_lens)?,
                        HuffmanTree::from_lengths(&dist_lens)?,
                    )
                } else {
                    let hlit = reader.read_bits(5)? as usize + 257;
                    let hdist = reader.read_bits(5)? as usize + 1;
                    let hclen = reader.read_bits(4)? as usize + 4;
                    let mut cl_lens = [0u8; 19];
                    for i in 0..hclen {
                        cl_lens[CL_ORDER[i]] = reader.read_bits(3)? as u8;
                    }
                    let cl_tree = HuffmanTree::from_lengths(&cl_lens)?;
                    let total = hlit + hdist;
                    let mut lengths = Vec::with_capacity(total);
                    while lengths.len() < total {
                        let sym = cl_tree.decode(&mut reader)?;
                        match sym {
                            0..=15 => lengths.push(sym as u8),
                            16 => {
                                let prev = *lengths
                                    .last()
                                    .ok_or_else(|| "repeat 16 without previous".to_string())?;
                                let repeat = reader.read_bits(2)? as usize + 3;
                                lengths.resize(lengths.len() + repeat, prev);
                            }
                            17 => {
                                let repeat = reader.read_bits(3)? as usize + 3;
                                lengths.resize(lengths.len() + repeat, 0);
                            }
                            18 => {
                                let repeat = reader.read_bits(7)? as usize + 11;
                                lengths.resize(lengths.len() + repeat, 0);
                            }
                            other => return Err(format!("invalid code length sym {other}")),
                        }
                    }
                    (
                        HuffmanTree::from_lengths(&lengths[..hlit])?,
                        HuffmanTree::from_lengths(&lengths[hlit..hlit + hdist])?,
                    )
                };

                loop {
                    let sym = litlen_tree.decode(&mut reader)?;
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let len_idx = (sym as usize)
                            .checked_sub(257)
                            .filter(|&i| i < LEN_BASE.len())
                            .ok_or_else(|| format!("invalid length sym {sym}"))?;
                        let length = usize::from(LEN_BASE[len_idx])
                            + reader.read_bits(LEN_EXTRA[len_idx])? as usize;
                        let dist_sym = dist_tree.decode(&mut reader)? as usize;
                        if dist_sym >= DIST_BASE.len() {
                            return Err(format!("invalid dist sym {dist_sym}"));
                        }
                        let distance = usize::from(DIST_BASE[dist_sym])
                            + reader.read_bits(DIST_EXTRA[dist_sym])? as usize;
                        if distance == 0 || distance > out.len() {
                            return Err(format!("invalid back-reference distance {distance}"));
                        }
                        for _ in 0..length {
                            let byte = out[out.len() - distance];
                            out.push(byte);
                        }
                    }
                }
            }
            other => return Err(format!("invalid deflate btype {other}")),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok(out)
}

#[derive(Debug)]
struct EpubZipEntry {
    name: String,
    compression: u16,
    local_offset: usize,
    local_extra_len: usize,
    data: Vec<u8>,
}

fn read_epub_zip(path: &Path) -> Vec<EpubZipEntry> {
    let bytes = fs::read(path).expect("read epub file");
    let eocd_pos = bytes
        .windows(4)
        .rposition(|w| w == 0x0605_4B50u32.to_le_bytes())
        .expect("EOCD signature");
    let total_entries = u16::from_le_bytes([bytes[eocd_pos + 10], bytes[eocd_pos + 11]]) as usize;
    let cd_offset = u32::from_le_bytes([
        bytes[eocd_pos + 16],
        bytes[eocd_pos + 17],
        bytes[eocd_pos + 18],
        bytes[eocd_pos + 19],
    ]) as usize;

    let mut entries = Vec::with_capacity(total_entries);
    let mut cursor = cd_offset;
    for _ in 0..total_entries {
        assert_eq!(
            &bytes[cursor..cursor + 4],
            &0x0201_4B50u32.to_le_bytes(),
            "CD entry signature"
        );
        let compression = u16::from_le_bytes([bytes[cursor + 10], bytes[cursor + 11]]);
        let expected_crc = u32::from_le_bytes([
            bytes[cursor + 16],
            bytes[cursor + 17],
            bytes[cursor + 18],
            bytes[cursor + 19],
        ]);
        let comp_size = u32::from_le_bytes([
            bytes[cursor + 20],
            bytes[cursor + 21],
            bytes[cursor + 22],
            bytes[cursor + 23],
        ]) as usize;
        let name_len = u16::from_le_bytes([bytes[cursor + 28], bytes[cursor + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[cursor + 30], bytes[cursor + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[cursor + 32], bytes[cursor + 33]]) as usize;
        let local_offset = u32::from_le_bytes([
            bytes[cursor + 42],
            bytes[cursor + 43],
            bytes[cursor + 44],
            bytes[cursor + 45],
        ]) as usize;
        let name = String::from_utf8(bytes[cursor + 46..cursor + 46 + name_len].to_vec())
            .expect("UTF-8 ZIP entry name");
        cursor += 46 + name_len + extra_len + comment_len;

        assert_eq!(
            &bytes[local_offset..local_offset + 4],
            &0x0403_4B50u32.to_le_bytes(),
            "local header signature for {name}"
        );
        let local_name_len =
            u16::from_le_bytes([bytes[local_offset + 26], bytes[local_offset + 27]]) as usize;
        let local_extra_len =
            u16::from_le_bytes([bytes[local_offset + 28], bytes[local_offset + 29]]) as usize;
        let data_start = local_offset + 30 + local_name_len + local_extra_len;
        let raw = &bytes[data_start..data_start + comp_size];
        let data = match compression {
            0 => raw.to_vec(),
            8 => inflate_raw(raw).unwrap_or_else(|e| panic!("inflate failed for {name}: {e}")),
            other => panic!("unsupported ZIP compression method {other} for {name}"),
        };
        assert_eq!(
            crc32(&data),
            expected_crc,
            "CRC-32 mismatch for EPUB entry {name}"
        );
        entries.push(EpubZipEntry {
            name,
            compression,
            local_offset,
            local_extra_len,
            data,
        });
    }
    entries
}

fn find_epub_entry<'a>(entries: &'a [EpubZipEntry], name: &str) -> &'a [u8] {
    entries
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.data.as_slice())
        .unwrap_or_else(|| panic!("missing EPUB entry {name}"))
}

// ---------------------------------------------------------------------------
// Scratch directory helper
// ---------------------------------------------------------------------------

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aristotle-net-weread-e2e-{label}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Strict upstream-exact WeRead E-Ink + Web loopback TCP mock server
// ---------------------------------------------------------------------------

type ChapterZipBuilder = Arc<dyn Fn(&[i64]) -> Vec<u8> + Send + Sync>;

#[derive(Clone)]
enum ChapterFault {
    Http500OnBatch(Vec<i64>),
    ApiError { status: u16, code: i64, msg: String },
}

struct MockState {
    vid: String,
    device_id: String,
    access_token: String,
    refresh_token: String,
    zip_password: String,
    config_token: String,
    guest_token: String,
    web_vid: String,
    web_skey: String,
    web_reader_token: String,

    force_eink_expire_once: bool,
    force_web_expire: bool,
    force_web_unconfirmed: bool,
    omit_refresh_token_on_refresh: bool,

    eink_refresh_calls: usize,
    web_login_polls: usize,
    chapter_requests: Vec<Vec<i64>>,
    chapter_raw_paths: Vec<String>,
    chapter_fault: Option<ChapterFault>,
    chapter_fault_hits: usize,
    chapter_builder: Option<ChapterZipBuilder>,

    progress_response: Value,
    shelf_response: Value,
    bookmark_response: Value,
    review_response: Value,

    batch_uploads: Vec<Value>,
    eink_reads: Vec<Value>,
    web_reads: Vec<Value>,
}

impl MockState {
    fn new() -> Self {
        Self {
            vid: TEST_VID.to_string(),
            device_id: TEST_DEVICE_ID.to_string(),
            access_token: "access-initial-1".to_string(),
            refresh_token: "rt-keep-original-1".to_string(),
            zip_password: TEST_ZIP_PASSWORD.to_string(),
            config_token: TEST_CONFIG_TOKEN.to_string(),
            guest_token: TEST_GUEST_TOKEN.to_string(),
            web_vid: TEST_VID.to_string(),
            web_skey: "web-skey-e2e".to_string(),
            web_reader_token: TEST_WEB_READER_TOKEN.to_string(),
            force_eink_expire_once: false,
            force_web_expire: false,
            force_web_unconfirmed: false,
            omit_refresh_token_on_refresh: true,
            eink_refresh_calls: 0,
            web_login_polls: 0,
            chapter_requests: Vec::new(),
            chapter_raw_paths: Vec::new(),
            chapter_fault: None,
            chapter_fault_hits: 0,
            chapter_builder: None,
            progress_response: json!({
                "bookId": "100001",
                "book": {
                    "appId": "eink-app-id-42",
                    "bookVersion": 42,
                    "chapterUid": 1,
                    "chapterIdx": 1,
                    "chapterOffset": 12,
                    "summary": "第一章开头摘要文本",
                    "progress": 18,
                    "readingTime": 420
                },
                "tts": {
                    "appId": "eink-tts-id",
                    "chapterUid": 1,
                    "chapterIdx": 1,
                    "chapterOffset": 6
                },
                "canFreeRead": 1,
                "timestamp": 1_795_006_800i64
            }),
            shelf_response: json!({
                "synckey": 777,
                "books": [],
                "archive": []
            }),
            bookmark_response: json!({"synckey": 10, "updated": [], "chapters": []}),
            review_response: json!({"synckey": 10, "totalCount": 0, "hasMore": false, "reviews": []}),
            batch_uploads: Vec::new(),
            eink_reads: Vec::new(),
            web_reads: Vec::new(),
        }
    }
}

struct MockWeReadServer {
    port: u16,
    state: Arc<Mutex<MockState>>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockWeReadServer {
    fn start(state: MockState) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(state));
        let running = Arc::new(AtomicBool::new(true));

        let srv_state = Arc::clone(&state);
        let srv_running = Arc::clone(&running);
        let handle = thread::spawn(move || {
            while srv_running.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if !srv_running.load(Ordering::Relaxed) {
                    break;
                }
                handle_connection(&mut stream, &srv_state);
            }
        });

        Self {
            port,
            state,
            running,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn policy(&self) -> Policy {
        let mut policy = Policy::open();
        policy.scoped = true;
        policy.allow(&aristotle_net::Url::parse(&self.base_url()).unwrap());
        policy
    }

    fn transport(&self) -> ReadestTransport {
        ReadestTransport::with_timeout_and_limit(self.policy(), 60, 64 * 1024 * 1024)
    }
}

impl Drop for MockWeReadServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct ParsedHttpRequest {
    method: String,
    path_and_query: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ParsedHttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn query_param(&self, key: &str) -> Option<String> {
        for pair in self.query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == key {
                    return Some(v.to_string());
                }
            }
        }
        None
    }
}

fn read_http_request(stream: &mut TcpStream) -> Option<ParsedHttpRequest> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
            }
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.lines();
    let first_line = lines.next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path_and_query = parts.next()?.to_string();
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (path_and_query.clone(), String::new()),
    };

    let mut headers = Vec::new();
    let mut content_len = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_len = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_len {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }
    body.truncate(content_len);

    Some(ParsedHttpRequest {
        method,
        path_and_query,
        path,
        query,
        headers,
        body,
    })
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    extra_headers: &[(&str, String)],
    body: &[u8],
) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Response",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn write_json(stream: &mut TcpStream, status: u16, extra_headers: &[(&str, String)], val: &Value) {
    let body = serde_json::to_vec(val).unwrap();
    let mut headers = vec![("Content-Type", "application/json".to_string())];
    headers.extend_from_slice(extra_headers);
    write_http_response(stream, status, &headers, &body);
}

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn verify_eink_auth(state: &mut MockState, req: &ParsedHttpRequest) -> Result<(), (u16, Value)> {
    if req.header("baseapi") != Some("30") || req.header("appver").is_none() {
        return Err((
            400,
            json!({"errcode": -1, "errmsg": "missing version headers"}),
        ));
    }
    if state.force_eink_expire_once {
        state.force_eink_expire_once = false;
        return Err((
            200,
            json!({"errcode": -2012, "errmsg": "accessToken expired"}),
        ));
    }
    if req.header("vid") != Some(state.vid.as_str())
        || req.header("accessToken") != Some(state.access_token.as_str())
    {
        return Err((
            401,
            json!({"errcode": -2012, "errmsg": "invalid vid/accessToken"}),
        ));
    }
    Ok(())
}

fn verify_batch_upload_signatures(state: &MockState, body: &Value) -> Result<(), String> {
    let book = body
        .get("books")
        .and_then(Value::as_array)
        .and_then(|books| books.first())
        .ok_or_else(|| "missing books[0]".to_string())?;
    let text = |k: &str| {
        book.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let num = |k: &str| book.get(k).and_then(Value::as_i64).unwrap_or(0);
    let mut parts = vec![
        state.vid.clone(),
        text("deviceId"),
        text("appId"),
        text("bookId"),
        num("risk").to_string(),
        text("recordCreateTimeZone"),
        num("readingTime").to_string(),
        "0".to_string(),
    ];
    let hours = book
        .get("hours")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing hours".to_string())?;
    let mut hour_sum = 0i64;
    for hour in hours {
        let rt = hour.get("readingTime").and_then(Value::as_i64).unwrap_or(0);
        hour_sum += rt;
        parts.push(format!(
            "{}_{}_{}_{}",
            hour.get("startOfHour").and_then(Value::as_i64).unwrap_or(0),
            hour.get("timeZone").and_then(Value::as_str).unwrap_or(""),
            rt,
            hour.get("ttsTime").and_then(Value::as_i64).unwrap_or(0),
        ));
    }
    if hour_sum != num("readingTime") {
        return Err(format!(
            "hours sum {hour_sum} != readingTime {}",
            num("readingTime")
        ));
    }
    let expected_inner = inner_signature(&[
        state.guest_token.clone(),
        num("random").to_string(),
        num("timestamp").to_string(),
        parts.join("_"),
    ]);
    if text("signature") != expected_inner {
        return Err("inner signature mismatch".to_string());
    }
    let expected_outer = outer_signature(
        body.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        &state.config_token,
        body.get("random").and_then(Value::as_u64).unwrap_or(0) as u16,
    );
    if body.get("signature").and_then(Value::as_str) != Some(expected_outer.as_str()) {
        return Err("outer signature mismatch".to_string());
    }
    Ok(())
}

fn verify_eink_read_signature(state: &MockState, body: &Value) -> Result<(), String> {
    let text = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let num = |k: &str| body.get(k).and_then(Value::as_i64).unwrap_or(0);
    let parts = [
        state.vid.clone(),
        text("deviceId"),
        text("appId"),
        text("bookId"),
        num("risk").to_string(),
        text("recordCreateTimeZone"),
        num("readingTime").to_string(),
        "0".to_string(),
    ];
    let expected = inner_signature(&[
        state.guest_token.clone(),
        num("random").to_string(),
        num("timestamp").to_string(),
        parts.join("_"),
    ]);
    if text("signature") != expected {
        return Err("eink read signature mismatch".to_string());
    }
    Ok(())
}

fn verify_web_read_payload(state: &MockState, body: &Value) -> Result<(), String> {
    let obj = body
        .as_object()
        .ok_or_else(|| "web read body must be JSON object".to_string())?;
    let mut payload = WebPayload::new();
    for (k, v) in obj {
        if k == "s" {
            continue;
        }
        let qv = match v {
            Value::String(s) => QueryValue::Text(s.clone()),
            Value::Number(n) => QueryValue::Number(n.as_i64().unwrap_or(0)),
            Value::Bool(b) => QueryValue::Bool(*b),
            Value::Null => QueryValue::Null,
            _ => return Err(format!("unsupported JSON value for key {k}")),
        };
        payload.insert(k.clone(), qv);
    }
    let expected_s = sign(&sorted_query(&payload));
    let actual_s = obj.get("s").and_then(Value::as_str).unwrap_or("");
    if actual_s != expected_s {
        return Err(format!("web s mismatch: got {actual_s}, want {expected_s}"));
    }
    // Period payload includes `sg`: verify sha256(ts + rn + token).
    if let Some(sg) = obj.get("sg").and_then(Value::as_str) {
        let ts = obj.get("ts").and_then(Value::as_i64).unwrap_or(0);
        let rn = obj.get("rn").and_then(Value::as_i64).unwrap_or(0);
        let expected_sg = sha256_hex(&format!("{ts}{rn}{}", state.web_reader_token));
        if sg != expected_sg {
            return Err(format!("web sg mismatch: got {sg}, want {expected_sg}"));
        }
    }
    Ok(())
}

fn cookie_has(cookie_header: Option<&str>, key: &str, expected: &str) -> bool {
    let Some(raw) = cookie_header else {
        return false;
    };
    raw.split(';').any(|pair| {
        pair.trim()
            .split_once('=')
            .is_some_and(|(k, v)| k.trim() == key && v.trim() == expected)
    })
}

fn handle_connection(stream: &mut TcpStream, state_arc: &Arc<Mutex<MockState>>) {
    let Some(req) = read_http_request(stream) else {
        return;
    };
    let mut state = state_arc.lock().unwrap();

    match (req.method.as_str(), req.path.as_str()) {
        // ---- E-Ink QR Login Flow ----
        ("GET", "/wxticket") => {
            if req.query_param("nonceStr").as_deref() != Some("weread") {
                write_json(
                    stream,
                    400,
                    &[],
                    &json!({"errcode": -1, "errmsg": "bad nonceStr"}),
                );
                return;
            }
            write_json(
                stream,
                200,
                &[],
                &json!({"signature": "sig+ticket/=", "timeStamp": 1_795_000_000i64}),
            );
        }
        ("GET", "/connect/sdk/qrconnect") => {
            if req.query_param("appid").as_deref() != Some(WX_APPID)
                || req.query_param("noncestr").as_deref() != Some("weread")
            {
                write_json(stream, 400, &[], &json!({"errcode": -1}));
                return;
            }
            write_json(
                stream,
                200,
                &[],
                &json!({"errcode": 0, "uuid": "e2e-qr-uuid-01"}),
            );
        }
        ("GET", "/connect/l/qrconnect") => {
            if req.query_param("uuid").as_deref() != Some("e2e-qr-uuid-01") {
                write_json(stream, 400, &[], &json!({"wx_errcode": 402}));
                return;
            }
            match req.query_param("last").as_deref() {
                None => write_json(stream, 200, &[], &json!({"wx_errcode": 408})),
                Some("408") => write_json(stream, 200, &[], &json!({"wx_errcode": 404})),
                Some("404") => write_json(
                    stream,
                    200,
                    &[],
                    &json!({"wx_errcode": 405, "wx_code": "wx-e2e-code-777"}),
                ),
                _ => write_json(stream, 200, &[], &json!({"wx_errcode": 402})),
            }
        }
        ("POST", "/login") => {
            let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
                write_json(
                    stream,
                    400,
                    &[],
                    &json!({"errcode": -1, "errmsg": "bad json"}),
                );
                return;
            };
            let timestamp = body.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
            let random = body.get("random").and_then(Value::as_u64).unwrap_or(0);
            let device_id = body.get("deviceId").and_then(Value::as_str).unwrap_or("");
            let expected_sig = sha256_hex(&format!("{timestamp}{device_id}{random}"));
            if body.get("deviceType").and_then(Value::as_u64) != Some(3)
                || device_id != state.device_id
                || body.get("signature").and_then(Value::as_str) != Some(expected_sig.as_str())
            {
                write_json(
                    stream,
                    401,
                    &[],
                    &json!({"errcode": -2012, "errmsg": "bad login signature"}),
                );
                return;
            }
            if let Some(rt) = body.get("refreshToken").and_then(Value::as_str) {
                if rt != state.refresh_token {
                    write_json(
                        stream,
                        401,
                        &[],
                        &json!({"errcode": -2012, "errmsg": "refreshToken mismatch"}),
                    );
                    return;
                }
                state.eink_refresh_calls += 1;
                state.access_token = format!("access-refreshed-{}", state.eink_refresh_calls);
                state.force_eink_expire_once = false;
                let resp = if state.omit_refresh_token_on_refresh {
                    // Upstream WeRead quirk: refresh returns new accessToken WITHOUT refreshToken.
                    json!({
                        "vid": state.vid.parse::<i64>().unwrap(),
                        "accessToken": state.access_token,
                    })
                } else {
                    state.refresh_token = format!("rt-rotated-{}", state.eink_refresh_calls);
                    json!({
                        "vid": state.vid.parse::<i64>().unwrap(),
                        "accessToken": state.access_token,
                        "refreshToken": state.refresh_token,
                    })
                };
                write_json(stream, 200, &[], &resp);
            } else {
                if body.get("code").and_then(Value::as_str) != Some("wx-e2e-code-777")
                    || body.get("isFromQrcode").and_then(Value::as_u64) != Some(1)
                {
                    write_json(
                        stream,
                        401,
                        &[],
                        &json!({"errcode": -2012, "errmsg": "bad wx_code"}),
                    );
                    return;
                }
                write_json(
                    stream,
                    200,
                    &[],
                    &json!({
                        "vid": state.vid.parse::<i64>().unwrap(),
                        "accessToken": state.access_token,
                        "refreshToken": state.refresh_token,
                    }),
                );
            }
        }

        // ---- Web QR Login + Reader State + Progress Write ----
        ("GET", "/r/weread-skills") => {
            let cookies = vec![(
                "Set-Cookie",
                "ptcz=ptcz-val; Path=/, RK=rk-val; Path=/, pgv_pvid=pvid-val; Path=/, tmp_ephemeral=dropme; Path=/".to_string(),
            )];
            write_http_response(stream, 200, &cookies, b"<html>skills</html>");
        }
        ("GET", "/api/auth/getLoginUid") => {
            if !cookie_has(req.header("cookie"), "ptcz", "ptcz-val") {
                write_json(
                    stream,
                    400,
                    &[],
                    &json!({"errcode": -1, "errmsg": "missing seed cookies"}),
                );
                return;
            }
            write_json(stream, 200, &[], &json!({"uid": "web-uid-999"}));
        }
        ("GET", "/api/auth/getLoginInfo") => {
            if req.query_param("uid").as_deref() != Some("web-uid-999") {
                write_json(stream, 400, &[], &json!({"logicCode": "LOGIN_TIMEOUT"}));
                return;
            }
            state.web_login_polls += 1;
            let otp = req.query_param("otp").unwrap_or_default();
            match (state.web_login_polls, otp.as_str()) {
                (1, "") => write_json(stream, 200, &[], &json!({"logicCode": "WAIT"})),
                (2, "") => write_json(stream, 200, &[], &json!({"logicCode": "NEED_OTP"})),
                (_, "4829") => {
                    let cookies = vec![(
                        "Set-Cookie",
                        format!(
                            "wr_vid={}; Path=/; Expires=Wed, 21 Oct 2030 07:28:00 GMT, wr_skey={}; Path=/",
                            state.web_vid, state.web_skey
                        ),
                    )];
                    write_json(
                        stream,
                        200,
                        &cookies,
                        &json!({
                            "succeed": true,
                            "webLoginVid": state.web_vid.parse::<i64>().unwrap(),
                            "accessToken": state.web_skey,
                            "refreshToken": "web-rt/e2e+1"
                        }),
                    );
                }
                (_, _) => write_json(stream, 200, &[], &json!({"logicCode": "OTP_NOT_MATCH"})),
            }
        }
        ("GET", path) if path.starts_with("/web/reader/") => {
            if !cookie_has(req.header("cookie"), "wr_vid", &state.web_vid)
                || !cookie_has(req.header("cookie"), "wr_skey", &state.web_skey)
            {
                write_json(
                    stream,
                    401,
                    &[],
                    &json!({"errcode": -2012, "errmsg": "no web cookies"}),
                );
                return;
            }
            let html = format!(
                "<!doctype html><html><head><script>window.__INITIAL_STATE__={{\"reader\":{{\"psvts\":\"{TEST_PSVTS}\",\"pclts\":\"{TEST_PCLTS}\",\"token\":\"{}\"}}}};</script></head><body>reader</body></html>",
                state.web_reader_token
            );
            write_http_response(
                stream,
                200,
                &[("Content-Type", "text/html; charset=utf-8".to_string())],
                html.as_bytes(),
            );
        }
        ("POST", "/web/book/read") => {
            if state.force_web_expire {
                write_json(
                    stream,
                    200,
                    &[],
                    &json!({"errcode": -2012, "errmsg": "web skey expired"}),
                );
                return;
            }
            if state.force_web_unconfirmed {
                state.force_web_unconfirmed = false;
                write_json(
                    stream,
                    200,
                    &[],
                    &json!({"succ": 0, "msg": "upstream write unconfirmed"}),
                );
                return;
            }
            if !cookie_has(req.header("cookie"), "wr_vid", &state.web_vid)
                || !cookie_has(req.header("cookie"), "wr_skey", &state.web_skey)
            {
                write_json(stream, 401, &[], &json!({"errcode": -2012}));
                return;
            }
            let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
                write_json(stream, 400, &[], &json!({"succ": 0, "msg": "bad json"}));
                return;
            };
            if let Err(msg) = verify_web_read_payload(&state, &body) {
                write_json(stream, 200, &[], &json!({"succ": 0, "msg": msg}));
                return;
            }
            state.web_reads.push(body);
            write_json(stream, 200, &[], &json!({"succ": 1}));
        }

        // ---- Authenticated E-Ink Endpoints ----
        _ => {
            if let Err((status, val)) = verify_eink_auth(&mut state, &req) {
                write_json(stream, status, &[], &val);
                return;
            }
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/shelf/sync") => {
                    let resp = state.shelf_response.clone();
                    write_json(stream, 200, &[], &resp);
                }
                ("GET", "/book/getProgress") => {
                    let resp = state.progress_response.clone();
                    write_json(stream, 200, &[], &resp);
                }
                ("GET", "/config") => {
                    let token = state.config_token.clone();
                    write_json(stream, 200, &[], &json!({"token": token}));
                }
                ("GET", "/feature") => {
                    let token = state.guest_token.clone();
                    write_json(
                        stream,
                        200,
                        &[],
                        &json!({"feature": {"guest_token": token}}),
                    );
                }
                ("GET", "/book/bookmarklist") => {
                    let resp = state.bookmark_response.clone();
                    write_json(stream, 200, &[], &resp);
                }
                ("GET", "/review/list") => {
                    let resp = state.review_response.clone();
                    write_json(stream, 200, &[], &resp);
                }
                ("POST", "/book/batchUploadProgress") => {
                    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
                        write_json(stream, 400, &[], &json!({"errcode": -1}));
                        return;
                    };
                    if let Err(msg) = verify_batch_upload_signatures(&state, &body) {
                        write_json(stream, 400, &[], &json!({"errcode": -1, "errmsg": msg}));
                        return;
                    }
                    state.batch_uploads.push(body);
                    write_json(stream, 200, &[], &json!({"succ": 1}));
                }
                ("POST", "/book/read") => {
                    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
                        write_json(stream, 400, &[], &json!({"errcode": -1}));
                        return;
                    };
                    if let Err(msg) = verify_eink_read_signature(&state, &body) {
                        write_json(stream, 400, &[], &json!({"errcode": -1, "errmsg": msg}));
                        return;
                    }
                    state.eink_reads.push(body);
                    write_json(stream, 200, &[], &json!({"succ": 1}));
                }
                ("GET", "/book/chapterdownload") => {
                    let chapters: Vec<i64> = req
                        .query_param("chapters")
                        .unwrap_or_default()
                        .split(',')
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    state.chapter_requests.push(chapters.clone());
                    state.chapter_raw_paths.push(req.path_and_query.clone());

                    if let Some(fault) = state.chapter_fault.clone() {
                        match fault {
                            ChapterFault::Http500OnBatch(target_batch) => {
                                if chapters == target_batch {
                                    state.chapter_fault_hits += 1;
                                    write_http_response(
                                        stream,
                                        500,
                                        &[("Content-Type", "text/plain".to_string())],
                                        b"upstream batch gateway 500",
                                    );
                                    return;
                                }
                            }
                            ChapterFault::ApiError { status, code, msg } => {
                                state.chapter_fault_hits += 1;
                                write_json(
                                    stream,
                                    status,
                                    &[],
                                    &json!({"errcode": code, "errmsg": msg}),
                                );
                                return;
                            }
                        }
                    }

                    let builder = state
                        .chapter_builder
                        .clone()
                        .expect("chapter_builder must be configured");
                    let zip_bytes = builder(&chapters);
                    let envelope = make_envelope(&state.vid, &state.zip_password);
                    write_http_response(
                        stream,
                        200,
                        &[
                            ("Content-Type", "application/zip".to_string()),
                            ("encryptkey", envelope),
                        ],
                        &zip_bytes,
                    );
                }
                _ => {
                    write_http_response(stream, 404, &[], b"not found");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scenario 3 & 4 fixture builder: 25-chapter illustrated book with all traps
// ---------------------------------------------------------------------------

fn obfuscated_chapter_href(uid: i64) -> String {
    if uid == 10 || uid == 11 {
        // Trap (b): Two TOC chapters (10 & 11) share the SAME XHTML file.
        "Text/:::**:**::ch10_shared.xhtml".to_string()
    } else {
        format!("Text/:::**:**::ch{uid:02}.xhtml")
    }
}

fn obfuscated_image_href(uid: i64) -> String {
    format!("Images/:******:img{uid:02}.jpg")
}

fn chapter_css_href(uid: i64) -> String {
    format!("Styles/ch{uid:02}.css")
}

fn build_25_chapter_toc() -> ChapterInfos {
    let mut chapters = vec![ChapterInfo {
        chapter_uid: Some(0),
        chapter_idx: Some(0),
        title: Some("Public Resources".to_string()),
        word_count: Some(0),
        price: Some(0.0),
        paid: Some(0),
        tar: None,
        files: vec!["Text/0.xhtml".to_string()],
    }];
    for uid in 1..=25 {
        chapters.push(ChapterInfo {
            chapter_uid: Some(uid),
            chapter_idx: Some(uid),
            title: Some(format!("第{uid}章 真实排版测试")),
            word_count: Some(120 + uid),
            price: Some(if uid <= 20 { 0.0 } else { 15.0 }),
            paid: Some(if uid <= 20 { 0 } else { 1 }),
            tar: None,
            files: vec![obfuscated_chapter_href(uid)],
        });
    }
    // 2 paid/ineligible chapters (price > 0 && paid == 0): MUST NEVER be requested!
    for uid in [26i64, 27i64] {
        chapters.push(ChapterInfo {
            chapter_uid: Some(uid),
            chapter_idx: Some(uid),
            title: Some(format!("付费锁定第{uid}章")),
            word_count: Some(300),
            price: Some(99.0),
            paid: Some(0),
            tar: None,
            files: vec![format!("Text/:::**:**::paid{uid}.xhtml")],
        });
    }
    ChapterInfos {
        book_id: "300025".to_string(),
        synckey: Some(2501),
        book_version: Some(77),
        chapters,
    }
}

fn make_25_chapter_zip_builder() -> ChapterZipBuilder {
    Arc::new(|requested: &[i64]| {
        let mut owned_entries: Vec<(String, Vec<u8>)> = Vec::new();
        let mut manifest_chapters: Vec<Value> = Vec::new();
        let mut added_paths: BTreeSet<String> = BTreeSet::new();

        // Trap (a): Placeholder error file injected inside ZIPs.
        owned_entries.push((
            "Text/ErrLossFileSourcePre.html".to_string(),
            b"<html><body><p>ErrLossFileSourcePre placeholder</p></body></html>".to_vec(),
        ));

        let includes_public = requested.contains(&0);
        if includes_public {
            manifest_chapters.push(json!({
                "chapterUid": 0,
                "files": ["Text/0.xhtml"]
            }));
            owned_entries.push((
                "Text/0.xhtml".to_string(),
                b"<html><body>public</body></html>".to_vec(),
            ));
            owned_entries.push(("Images/cover.png".to_string(), b"PNG-COVER-BYTES".to_vec()));
            owned_entries.push((
                "Styles/base.css".to_string(),
                b"body { margin: 0; line-height: 1.6; }".to_vec(),
            ));
            // Trap (c) part 1: Batch 1's version of Images/shared.png (MUST win).
            owned_entries.push((
                "Images/shared.png".to_string(),
                b"PNG-SHARED-BATCH-1-FIRST".to_vec(),
            ));
            // Trap (f): Identical duplicate asset entry inside the same batch ZIP!
            owned_entries.push((
                "Images/dup_in_batch.png".to_string(),
                b"IDENTICAL-DUP-ASSET-PAYLOAD".to_vec(),
            ));
            owned_entries.push((
                "Images/dup_in_batch.png".to_string(),
                b"IDENTICAL-DUP-ASSET-PAYLOAD".to_vec(),
            ));
        }

        // Trap (c) part 2: Batch 3 (chapters 11..=15) ships conflicting bytes for Images/shared.png.
        if requested.iter().any(|&uid| (11..=15).contains(&uid)) && !includes_public {
            owned_entries.push((
                "Images/shared.png".to_string(),
                b"PNG-SHARED-BATCH-3-CONFLICTING".to_vec(),
            ));
            // Also duplicate an identical asset in batch 3 to verify dedup across batches.
            owned_entries.push((
                "Images/dup_batch3.png".to_string(),
                b"DUP-BATCH-3-BYTES".to_vec(),
            ));
            owned_entries.push((
                "Images/dup_batch3.png".to_string(),
                b"DUP-BATCH-3-BYTES".to_vec(),
            ));
        }

        for &uid in requested {
            if uid == 0 {
                continue;
            }
            assert!(
                uid <= 25,
                "SERVER RED-LINE VIOLATION: paid/ineligible chapter {uid} was requested!"
            );

            let href = obfuscated_chapter_href(uid);
            let img_href = obfuscated_image_href(uid);
            let css_href = chapter_css_href(uid);

            // Include ErrLossFileSourcePre.html in chapter 3's manifest to prove
            // select_chapter_files explicitly strips it even when listed in info.txt.
            let files_json = if uid == 3 {
                json!(["Text/ErrLossFileSourcePre.html", href])
            } else {
                json!([href])
            };
            manifest_chapters.push(json!({
                "chapterUid": uid,
                "files": files_json,
            }));

            // Every chapter in batches 1..=5 brings its own per-chapter image & CSS.
            owned_entries.push((
                img_href.clone(),
                format!("JPEG-CHAPTER-{uid:02}-ILLUSTRATION").into_bytes(),
            ));
            owned_entries.push((
                css_href.clone(),
                format!(".ch{uid:02} {{ text-indent: 2em; }}").into_bytes(),
            ));

            if added_paths.insert(href.clone()) {
                let xhtml = match uid {
                    // Trap (d): Full HTML with <head><style> + named HTML entities + unknown entity + bare `&`.
                    4 => format!(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                         <html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
                         <head><title>Ch4</title><style>.keep{{color:#000}}</style></head>\n\
                         <body>\n\
                           <p>A & B &nbsp; arrow &rarr; dot &middot; custom &unknownEntity; end.</p>\n\
                           <p>第二段正文用于锚点定位测试。</p>\n\
                           <img src=\"../{img_href}\"/>\n\
                         </body>\n\
                         </html>"
                    ),
                    // Trap (e): Dangling external `tar` image reference not present in any ZIP.
                    7 => format!(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                         <html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
                         <head><title>Ch7</title></head>\n\
                         <body>\n\
                           <p>Chapter 7 has a dangling external tar image and a real image.</p>\n\
                           <img src=\"../Images/external_tar_only.jpg\"/>\n\
                           <img src=\"../{img_href}\"/>\n\
                         </body>\n\
                         </html>"
                    ),
                    // Trap (b): Shared XHTML used by TOC chapters 10 and 11.
                    10 | 11 => format!(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                         <html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
                         <head><title>Shared 10/11</title></head>\n\
                         <body>\n\
                           <p>第十章与第十一章共享同一个文档。</p>\n\
                           <p>第二段共享正文内容。</p>\n\
                           <img src=\"../{img_href}\"/>\n\
                         </body>\n\
                         </html>"
                    ),
                    _ => format!(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                         <html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
                         <head><title>Ch{uid}</title></head>\n\
                         <body>\n\
                           <p>第{uid}章第一段正文内容用于验证完整阅读流程。</p>\n\
                           <p>第{uid}章第二段包含插图与样式引用。</p>\n\
                           <img src=\"../{img_href}\"/>\n\
                         </body>\n\
                         </html>"
                    ),
                };
                owned_entries.push((href, xhtml.into_bytes()));
            }
        }

        let info_bytes = serde_json::to_vec(&json!({ "chapters": manifest_chapters })).unwrap();
        owned_entries.push(("info.txt".to_string(), info_bytes));

        let refs: Vec<(&str, &[u8])> = owned_entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        build_zipcrypto_zip(&refs, TEST_ZIP_PASSWORD)
    })
}

fn parse_xml_strict(xml: &str) -> Document<'_> {
    Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )
    .expect("valid XML")
}

// ---------------------------------------------------------------------------
// Scenario 1: E-Ink QR Login + -2012 Token Refresh + Web QR Login
// ---------------------------------------------------------------------------

#[test]
fn scenario_1_eink_qr_login_token_refresh_and_web_qr_login() {
    let mut state = MockState::new();
    state.shelf_response = json!({
        "synckey": 101,
        "books": [
            {"bookId": "b-1", "title": "After Refresh", "format": "epub", "finished": 0, "secret": 0}
        ]
    });
    let server = MockWeReadServer::start(state);
    let base = server.base_url();
    let transport = server.transport();

    // 1A. Full E-Ink QR login flow over real loopback TCP:
    //     /wxticket -> qrconnect -> long-poll (408 -> 404 -> 405) -> /login.
    let ticket_resp = transport
        .send(HttpRequest::new(
            "GET",
            format!("{base}/wxticket?nonceStr=weread"),
        ))
        .expect("wxticket");
    let ticket = parse_qr_ticket(&ticket_resp).expect("parse wxticket");
    assert_eq!(ticket.signature, "sig+ticket/=");
    assert_eq!(ticket.time_stamp, 1_795_000_000);

    let canonical_qr_url = build_qrconnect_url(&ticket);
    assert!(canonical_qr_url.starts_with("https://open.weixin.qq.com/connect/sdk/qrconnect?"));
    assert!(canonical_qr_url.contains("signature=sig%2Bticket%2F%3D"));

    let local_qr_url = canonical_qr_url.replacen("https://open.weixin.qq.com", &base, 1);
    let qr_resp = transport
        .send(HttpRequest::new("GET", local_qr_url))
        .expect("qrconnect");
    let uuid = parse_qrconnect_response(&qr_resp).expect("uuid");
    assert_eq!(uuid, "e2e-qr-uuid-01");

    let poll1 = poll_status(
        &transport
            .send(HttpRequest::new(
                "GET",
                format!("{base}/connect/l/qrconnect?f=json&uuid={uuid}"),
            ))
            .expect("poll 1"),
    )
    .expect("poll 408");
    assert_eq!(poll1, QrPollStatus::Continue);

    let poll2 = poll_status(
        &transport
            .send(HttpRequest::new(
                "GET",
                format!("{base}/connect/l/qrconnect?f=json&uuid={uuid}&last=408"),
            ))
            .expect("poll 2"),
    )
    .expect("poll 404");
    assert_eq!(poll2, QrPollStatus::Scanned);

    let poll3 = poll_status(
        &transport
            .send(HttpRequest::new(
                "GET",
                format!("{base}/connect/l/qrconnect?f=json&uuid={uuid}&last=404"),
            ))
            .expect("poll 3"),
    )
    .expect("poll 405");
    let QrPollStatus::Confirmed(wx_code) = poll3 else {
        panic!("expected Confirmed, got {poll3:?}");
    };
    assert_eq!(wx_code, "wx-e2e-code-777");

    let login_body = build_login_body(
        wx_code,
        TEST_DEVICE_ID,
        DEFAULT_DEVICE_NAME,
        TEST_INSTALL_ID,
        1_795_006_800_000,
        321,
    );
    let login_req = build_login_request(&base, &login_body).expect("login request");
    let login_resp = transport.send(login_req).expect("POST /login");
    let creds = parse_login_response(&login_resp, TEST_DEVICE_ID).expect("credentials");
    assert_eq!(creds.vid, TEST_VID);
    assert_eq!(creds.access_token, "access-initial-1");
    assert_eq!(creds.refresh_token, "rt-keep-original-1");
    assert!(!format!("{creds:?}").contains("access-initial-1"));
    assert!(!format!("{creds:?}").contains("rt-keep-original-1"));

    // 1B. Token expiry (-2012) -> automatic /login refresh omitting refreshToken ->
    //     preserves original refreshToken and retries original request!
    server.state.lock().unwrap().force_eink_expire_once = true;
    let mut client = WeReadClient::new(server.transport(), creds)
        .with_base_url(&base)
        .with_time_provider(|| 1_795_006_900_000)
        .with_random_provider(|| 654);

    let shelf = client
        .shelf_sync()
        .expect("shelf_sync must succeed after -2012 silent refresh");
    assert_eq!(shelf.books.len(), 1);
    assert_eq!(shelf.books[0].book_id, "b-1");
    assert_eq!(server.state.lock().unwrap().eink_refresh_calls, 1);

    let renewed = client.credentials().expect("renewed credentials");
    assert_eq!(renewed.access_token, "access-refreshed-1");
    assert_eq!(
        renewed.refresh_token, "rt-keep-original-1",
        "refresh omitting refreshToken MUST preserve original refresh_token"
    );

    // 1C. Full Web QR login flow via WeReadWebClient:
    //     login_page -> login_uid -> login_poll (Continue -> NeedOtp -> OtpMismatch -> Confirmed)
    //     setting wr_vid/wr_skey/wr_rt in CookieJar!
    let mut web = WeReadWebClient::new(server.transport()).with_base_url(&base);
    web.login_page().expect("GET /r/weread-skills");
    assert_eq!(web.jar().get("ptcz"), Some("ptcz-val"));
    assert_eq!(web.jar().get("tmp_ephemeral"), Some("dropme"));

    let web_uid = web.login_uid().expect("GET /api/auth/getLoginUid");
    assert_eq!(web_uid, "web-uid-999");
    assert!(web
        .confirm_url(&web_uid)
        .ends_with("/web/confirm?uid=web-uid-999"));

    assert_eq!(
        web.login_poll(&web_uid, "").expect("poll 1"),
        WebLoginPoll::Continue
    );
    assert_eq!(
        web.login_poll(&web_uid, "").expect("poll 2"),
        WebLoginPoll::NeedOtp
    );
    assert_eq!(
        web.login_poll(&web_uid, "0000").expect("poll wrong otp"),
        WebLoginPoll::OtpMismatch
    );
    let confirmed = web.login_poll(&web_uid, "4829").expect("poll right otp");
    let WebLoginPoll::Confirmed(success) = confirmed else {
        panic!("expected WebLoginPoll::Confirmed, got {confirmed:?}");
    };
    assert_eq!(success.vid, TEST_VID);
    assert_eq!(success.access_token, "web-skey-e2e");
    assert!(!format!("{success:?}").contains("web-skey-e2e"));

    assert_eq!(web.jar().get("wr_vid"), Some(TEST_VID));
    assert_eq!(web.jar().get("wr_skey"), Some("web-skey-e2e"));
    assert_eq!(web.jar().get("wr_rt"), Some("web-rt%2Fe2e%2B1"));
    assert!(!format!("{:?}", web.jar()).contains("web-skey-e2e"));

    let persistent = web.jar().persistent();
    assert_eq!(persistent.get("wr_vid"), Some(TEST_VID));
    assert_eq!(persistent.get("wr_skey"), Some("web-skey-e2e"));
    assert_eq!(persistent.get("ptcz"), Some("ptcz-val"));
    assert_eq!(
        persistent.get("tmp_ephemeral"),
        None,
        "ephemeral cookies must be stripped by persistent()"
    );
    let session = WebSession {
        cookies: persistent,
        vid: success.vid,
        logged_at: 1_795_007_000,
    };
    assert!(session.is_authenticated());
    assert!(!format!("{session:?}").contains("web-skey-e2e"));
}

// ---------------------------------------------------------------------------
// Scenario 2: Shelf Sync with Real Wire Quirks
// ---------------------------------------------------------------------------

#[test]
fn scenario_2_shelf_sync_with_real_wire_quirks() {
    let mut state = MockState::new();
    state.shelf_response = json!({
        "synckey": 987_654,
        "bookCount": 4,
        "archive": [
            {"archiveId": 10, "name": "历史", "bookIds": ["100001", "100003"]}
        ],
        "books": [
            {
                "bookId": "100001",
                "title": "EPUB 已读完",
                "author": "作者甲",
                "format": "epub",
                "finished": 1,
                "secret": 0,
                "progress": 100,
                "readingTime": 3600,
                "archive": ["历史"],
                "unknownServerField": {"nested": true}
            },
            {
                "bookId": "100002",
                "title": "TXT 私密在读",
                "format": "txt",
                "finished": 0,
                "secret": 1,
                "progress": 45,
                "readingTime": 850
            },
            {
                "bookId": "100003",
                "title": "PDF 文档",
                "format": "pdf",
                "finished": "1",
                "secret": "0",
                "progress": 10,
                "readingTime": 120,
                "archive": [{"id": 10, "name": "历史"}]
            },
            {
                "bookId": "100004",
                "title": "布尔兼容书",
                "format": "EPUB",
                "finished": false,
                "secret": true,
                "progress": 0,
                "readingTime": 0
            }
        ]
    });
    let server = MockWeReadServer::start(state);
    let creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let mut client = WeReadClient::new(server.transport(), creds).with_base_url(server.base_url());

    let shelf = client
        .shelf_sync()
        .expect("shelf_sync with real wire quirks");
    assert_eq!(shelf.synckey, Some(987_654));
    assert_eq!(shelf.books.len(), 4);

    assert_eq!(shelf.books[0].book_id, "100001");
    assert_eq!(shelf.books[0].finished, Some(true));
    assert_eq!(shelf.books[0].secret, Some(false));
    assert_eq!(
        SourceFormat::from_shelf(shelf.books[0].format.as_deref()).unwrap(),
        SourceFormat::Epub
    );
    assert!(shelf.books[0].archive.is_some());

    assert_eq!(shelf.books[1].finished, Some(false));
    assert_eq!(shelf.books[1].secret, Some(true));
    assert_eq!(
        SourceFormat::from_shelf(shelf.books[1].format.as_deref()).unwrap(),
        SourceFormat::Txt
    );

    assert_eq!(shelf.books[2].finished, Some(true));
    assert_eq!(shelf.books[2].secret, Some(false));
    assert_eq!(
        SourceFormat::from_shelf(shelf.books[2].format.as_deref()).unwrap(),
        SourceFormat::Pdf
    );
    assert!(shelf.books[2].archive.is_some());

    assert_eq!(shelf.books[3].finished, Some(false));
    assert_eq!(shelf.books[3].secret, Some(true));
    assert_eq!(
        SourceFormat::from_shelf(shelf.books[3].format.as_deref()).unwrap(),
        SourceFormat::Epub
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: 25-Chapter Multi-Batch Illustrated Book — Exact User Failure
// ---------------------------------------------------------------------------

#[test]
fn scenario_3_25_chapter_multi_batch_illustrated_book_with_all_real_traps() {
    let mut state = MockState::new();
    state.chapter_builder = Some(make_25_chapter_zip_builder());
    let server = MockWeReadServer::start(state);

    let creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let transport = ReadestTransport::with_timeout_and_limit(server.policy(), 60, 64 * 1024 * 1024);
    let mut client = WeReadClient::new(transport, creds).with_base_url(server.base_url());

    let toc = build_25_chapter_toc();
    let dir = ScratchDir::new("25ch-illustrated");
    // Request all 27 TOC chapters (25 eligible + 2 paid/ineligible).
    let targets: Vec<i64> = (1..=27).collect();
    let request = DownloadRequest {
        book_id: "300025".to_string(),
        title: "二十五章图文全陷阱书".to_string(),
        format: SourceFormat::Epub,
        book_version: "77".to_string(),
        chapters: targets,
        epub_path: dir.path().join("books/weread/300025.epub"),
        mapping_path: dir.path().join("state/books.json"),
        work_dir: dir.path().join("books/weread/.cache/300025"),
    };

    let mut progress_events: Vec<(usize, usize)> = Vec::new();
    let outcome = download_book_with_progress(&mut client, &toc, &request, |done, total| {
        progress_events.push((done, total));
    })
    .expect("25-chapter multi-batch illustrated download must succeed");

    // 1. Entitlement red line: paid chapters 26 & 27 were skipped and NEVER requested.
    let skipped_uids: Vec<i64> = outcome.skipped.iter().map(|s| s.chapter_uid).collect();
    assert_eq!(skipped_uids, vec![26, 27]);
    assert_eq!(outcome.downloaded, (1..=25).collect::<Vec<i64>>());
    assert!(outcome.cached.is_empty());

    let reqs = server.state.lock().unwrap().chapter_requests.clone();
    assert_eq!(
        reqs.len(),
        5,
        "25 chapters in batches of 5 = 5 HTTP requests"
    );
    assert_eq!(reqs[0], vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(reqs[1], vec![6, 7, 8, 9, 10]);
    assert_eq!(reqs[2], vec![11, 12, 13, 14, 15]);
    assert_eq!(reqs[3], vec![16, 17, 18, 19, 20]);
    assert_eq!(reqs[4], vec![21, 22, 23, 24, 25]);
    for r in &reqs {
        assert!(
            !r.contains(&26) && !r.contains(&27),
            "paid chapters leaked: {r:?}"
        );
    }
    let ch0_count = reqs.iter().filter(|r| r.contains(&0)).count();
    assert_eq!(ch0_count, 1, "chapter 0 must be fetched ONLY once");

    // 2. Progress callback strictly monotonic 1..=25, never exceeds 25, never double-counts.
    let expected_progress: Vec<(usize, usize)> = (1..=25).map(|i| (i, 25)).collect();
    assert_eq!(progress_events, expected_progress);

    // 3. Rebuilt EPUB verification (real ZIP + DEFLATE parse + CRC-32 check).
    let entries = read_epub_zip(&outcome.epub_path);
    assert_eq!(entries[0].name, "mimetype");
    assert_eq!(entries[0].compression, 0, "mimetype must be Stored");
    assert_eq!(
        entries[0].local_offset, 0,
        "mimetype must be first at offset 0"
    );
    assert_eq!(
        entries[0].local_extra_len, 0,
        "mimetype must have no extra field"
    );
    assert_eq!(entries[0].data, EPUB_MIMETYPE.as_bytes());

    let entry_names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    // Every single batch 1..5 per-chapter obfuscated image & stylesheet MUST be present!
    for uid in 1..=25 {
        let img = format!("OEBPS/{}", obfuscated_image_href(uid));
        let css = format!("OEBPS/{}", chapter_css_href(uid));
        assert_eq!(
            find_epub_entry(&entries, &img),
            format!("JPEG-CHAPTER-{uid:02}-ILLUSTRATION").as_bytes(),
            "batch image missing or corrupted for chapter {uid}"
        );
        assert!(
            entry_names.contains(&css.as_str()),
            "batch stylesheet missing for chapter {uid}"
        );
    }

    // Trap (a): ErrLossFileSourcePre.html MUST NOT be in the EPUB or spine.
    assert!(
        !entry_names
            .iter()
            .any(|n| n.contains("ErrLossFileSourcePre")),
        "placeholder error page leaked into EPUB"
    );

    // Trap (c): Conflicting Images/shared.png keeps batch 1's copy cleanly.
    assert_eq!(
        find_epub_entry(&entries, "OEBPS/Images/shared.png"),
        b"PNG-SHARED-BATCH-1-FIRST"
    );
    // Trap (f): Identical duplicate asset appears exactly once in the EPUB.
    assert_eq!(
        entry_names
            .iter()
            .filter(|&&n| n == "OEBPS/Images/dup_in_batch.png")
            .count(),
        1
    );

    // Verify META-INF/container.xml, OEBPS/content.opf, OEBPS/toc.ncx.
    let container_xml =
        std::str::from_utf8(find_epub_entry(&entries, "META-INF/container.xml")).unwrap();
    let container_doc = parse_xml_strict(container_xml);
    assert_eq!(
        container_doc
            .descendants()
            .find(|n| n.has_tag_name("rootfile"))
            .and_then(|n| n.attribute("full-path")),
        Some("OEBPS/content.opf")
    );

    let opf_xml = std::str::from_utf8(find_epub_entry(&entries, "OEBPS/content.opf")).unwrap();
    let opf_doc = parse_xml_strict(opf_xml);
    let mut manifest: BTreeMap<&str, &str> = BTreeMap::new();
    for item in opf_doc.descendants().filter(|n| n.has_tag_name("item")) {
        manifest.insert(
            item.attribute("id").unwrap(),
            item.attribute("href").unwrap(),
        );
    }
    let spine_hrefs: Vec<&str> = opf_doc
        .descendants()
        .filter(|n| n.has_tag_name("itemref"))
        .map(|n| manifest[n.attribute("idref").unwrap()])
        .collect();
    // 25 eligible chapters with ch10 & ch11 sharing 1 XHTML => 24 unique spine entries!
    assert_eq!(spine_hrefs.len(), 24);
    assert_eq!(outcome.spine.len(), 24);
    assert!(!spine_hrefs
        .iter()
        .any(|h| h.contains("ErrLossFileSourcePre")));

    // Trap (b): Both chapters 10 & 11 appear in toc.ncx pointing at the shared XHTML!
    let ncx_xml = std::str::from_utf8(find_epub_entry(&entries, "OEBPS/toc.ncx")).unwrap();
    let ncx_doc = parse_xml_strict(ncx_xml);
    let nav_points: Vec<(&str, &str)> = ncx_doc
        .descendants()
        .filter(|n| n.has_tag_name("navPoint"))
        .map(|np| {
            let label = np
                .descendants()
                .find(|n| n.has_tag_name("text"))
                .and_then(|n| n.text())
                .unwrap_or("");
            let src = np
                .descendants()
                .find(|n| n.has_tag_name("content"))
                .and_then(|n| n.attribute("src"))
                .unwrap_or("");
            (label, src)
        })
        .collect();
    assert_eq!(nav_points.len(), 25, "NCX must preserve all 25 TOC entries");
    assert_eq!(nav_points[9].1, "Text/:::**:**::ch10_shared.xhtml");
    assert_eq!(nav_points[10].1, "Text/:::**:**::ch10_shared.xhtml");
    assert_ne!(nav_points[9].0, nav_points[10].0);

    // Every spine chapter must parse as well-formed XML.
    for href in &spine_hrefs {
        let path = format!("OEBPS/{href}");
        let text = std::str::from_utf8(find_epub_entry(&entries, &path)).unwrap();
        parse_xml_strict(text);
    }

    // Trap (d): Full HTML chapter 4 preserved <style>.keep{color:#000}</style> and repaired entities & bare `&`.
    let ch4_text =
        std::str::from_utf8(find_epub_entry(&entries, "OEBPS/Text/:::**:**::ch04.xhtml")).unwrap();
    assert!(ch4_text.contains(".keep{color:#000}"));
    assert!(ch4_text.contains("A &amp; B"));
    assert!(ch4_text.contains("&#160;"));
    assert!(ch4_text.contains("&#8594;"));
    assert!(ch4_text.contains("&#183;"));
    assert!(ch4_text.contains("&amp;unknownEntity;"));
    assert!(ch4_text.contains("../Styles/base.css"));

    // 4. partialMD5 matches file on disk AND BookMappingStore.
    let mut epub_file = File::open(&outcome.epub_path).unwrap();
    let disk_md5 = partial_md5(&mut epub_file).unwrap();
    assert_eq!(disk_md5, outcome.hash);
    let store = BookMappingStore::load(&request.mapping_path).expect("load BookMappingStore");
    let mapped = store.get("300025").expect("book mapping entry");
    assert_eq!(mapped.hash, outcome.hash);
    assert_eq!(mapped.format, "epub");
    assert_eq!(mapped.synckey, Some(2501));

    // 5. PositionMap + AnchorMap round-trip (including xpointer: prefix, element .0, whitespace-clamped offset).
    let mut pos_map = PositionMap::from_spine("300025", "77", &outcome.spine, &toc.chapters);
    let anchors = AnchorMap::from_epub(&outcome.epub_path, &pos_map).expect("AnchorMap::from_epub");
    pos_map.apply_anchor_lengths(&anchors);
    assert_eq!(anchors.chapters.len(), 24);

    // Exact text node round-trip on chapter 4 (spine_index 3).
    let ch4_coord = WeReadCoordinate {
        chapter_uid: 4,
        chapter_idx: 4,
        chapter_offset: 6,
    };
    let target = pos_map
        .xpointer_for(ch4_coord, Some(&anchors))
        .expect("xpointer_for ch4");
    assert_eq!(target.precision, Precision::Exact);
    let back = pos_map
        .from_xpointer(&target.xpointer, Some(&anchors))
        .expect("from_xpointer");
    assert_eq!(
        back.position,
        EpubPosition {
            spine_index: 3,
            char_offset: 6
        }
    );

    // `xpointer:` prefix round-trip.
    let prefixed = format!("xpointer:{}", target.xpointer);
    let back_prefixed = pos_map
        .from_xpointer(&prefixed, Some(&anchors))
        .expect("from_xpointer with xpointer: prefix");
    assert_eq!(back_prefixed.position, back.position);

    // Element `.0` pointer: second `<p>` in chapter 1 (spine_index 0).
    let ch1_anchors = anchors.chapter(1).expect("ch1 anchors");
    assert!(ch1_anchors.anchors.len() >= 2);
    let p2_start = ch1_anchors.anchors[1].char_start;
    let elem_pos = anchors
        .char_offset_of("/body/DocFragment[1]/body/p[2].0")
        .expect("element .0 pointer resolves to p[2] start");
    assert_eq!(
        elem_pos,
        EpubPosition {
            spine_index: 0,
            char_offset: p2_start
        }
    );

    // Whitespace-clamped offset beyond a text anchor's char_len.
    let first_anchor = &ch1_anchors.anchors[0];
    let oversized_xp = format!("{}.{}", first_anchor.path, first_anchor.char_len + 25);
    let clamped_pos = anchors
        .char_offset_of(&oversized_xp)
        .expect("oversized text offset must clamp, not return None");
    assert_eq!(
        clamped_pos,
        EpubPosition {
            spine_index: 0,
            char_offset: first_anchor.char_start + first_anchor.char_len,
        }
    );

    // 6. Warm-cache re-run makes 0 HTTP calls and retains ALL batch 1..5 images/styles identically!
    let prev_req_count = server.state.lock().unwrap().chapter_requests.len();
    let warm = download_book_with_progress(&mut client, &toc, &request, |_, _| {})
        .expect("warm cache rebuild");
    assert_eq!(
        server.state.lock().unwrap().chapter_requests.len(),
        prev_req_count
    );
    assert!(warm.downloaded.is_empty());
    assert_eq!(warm.cached.len(), 25);
    assert_eq!(
        warm.hash, outcome.hash,
        "warm cache rebuild must be byte-identical"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: Batch Fallback, Fatal Short-Circuit & Poisoned Cache Recovery
// ---------------------------------------------------------------------------

#[test]
fn scenario_4_batch_fallback_fatal_short_circuit_and_poisoned_cache_recovery() {
    // ---- 4A: HTTP 500 on batch 2 -> single-chapter fallback ONLY for batch 2, monotonic progress 1..=25 ----
    let mut state = MockState::new();
    state.chapter_builder = Some(make_25_chapter_zip_builder());
    state.chapter_fault = Some(ChapterFault::Http500OnBatch(vec![6, 7, 8, 9, 10]));
    let server = MockWeReadServer::start(state);

    let creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let mut client = WeReadClient::new(server.transport(), creds).with_base_url(server.base_url());
    let toc = build_25_chapter_toc();
    let dir = ScratchDir::new("batch-fallback");
    let request = DownloadRequest {
        book_id: "300025".to_string(),
        title: "批次回退测试".to_string(),
        format: SourceFormat::Epub,
        book_version: "77".to_string(),
        chapters: (1..=25).collect(),
        epub_path: dir.path().join("books/300025.epub"),
        mapping_path: dir.path().join("state/books.json"),
        work_dir: dir.path().join("books/.cache/300025"),
    };

    let mut progress = Vec::new();
    let outcome = download_book_with_progress(&mut client, &toc, &request, |done, total| {
        progress.push((done, total));
    })
    .expect("download must recover via single-chapter fallback on batch 2 HTTP 500");

    assert_eq!(
        progress,
        (1..=25).map(|i| (i, 25)).collect::<Vec<_>>(),
        "progress must be strictly monotonic 1..=25 with NO double counting"
    );
    assert_eq!(outcome.downloaded, (1..=25).collect::<Vec<i64>>());

    let reqs = server.state.lock().unwrap().chapter_requests.clone();
    assert_eq!(server.state.lock().unwrap().chapter_fault_hits, 1);
    // Expected sequence:
    // batch 1: [0,1,2,3,4,5]
    // batch 2 (500): [6,7,8,9,10] -> fallback singles: [6], [7], [8], [9], [10]
    // batch 3..5: [11..15], [16..20], [21..25]
    assert_eq!(
        reqs,
        vec![
            vec![0, 1, 2, 3, 4, 5],
            vec![6, 7, 8, 9, 10],
            vec![6],
            vec![7],
            vec![8],
            vec![9],
            vec![10],
            vec![11, 12, 13, 14, 15],
            vec![16, 17, 18, 19, 20],
            vec![21, 22, 23, 24, 25],
        ]
    );

    // ---- 4B: Fatal -2063 (version expired) & -10102 (rate limited) on batch 1 -> IMMEDIATE error, 1 HTTP request ----
    for (label, code, expect_rate_limit) in [
        ("fatal-2063", -2063i64, false),
        ("fatal-10102", -10102i64, true),
    ] {
        let mut s = MockState::new();
        s.chapter_builder = Some(make_25_chapter_zip_builder());
        s.chapter_fault = Some(ChapterFault::ApiError {
            status: 200,
            code,
            msg: format!("injected fatal {code}"),
        });
        let srv = MockWeReadServer::start(s);
        let c = Credentials::new(
            TEST_VID,
            "access-initial-1",
            "rt-keep-original-1",
            TEST_DEVICE_ID,
        );
        let mut cli = WeReadClient::new(srv.transport(), c).with_base_url(srv.base_url());
        let d = ScratchDir::new(label);
        let req = DownloadRequest {
            book_id: "300025".to_string(),
            title: "致命错误短路测试".to_string(),
            format: SourceFormat::Epub,
            book_version: "77".to_string(),
            chapters: (1..=10).collect(),
            epub_path: d.path().join("book.epub"),
            mapping_path: d.path().join("books.json"),
            work_dir: d.path().join("cache"),
        };

        let err = download_book_with_progress(&mut cli, &toc, &req, |_, _| {})
            .expect_err("fatal batch error must abort immediately");
        if expect_rate_limit {
            assert!(
                matches!(err, DownloadError::WeRead(WeReadError::RateLimited(-10102))),
                "expected RateLimited(-10102), got {err:?}"
            );
        } else {
            assert!(
                matches!(
                    err,
                    DownloadError::WeRead(WeReadError::Api { code: -2063, .. })
                ),
                "expected Api(-2063), got {err:?}"
            );
        }
        let sent = srv.state.lock().unwrap().chapter_requests.clone();
        assert_eq!(
            sent.len(),
            1,
            "{label} must make EXACTLY 1 HTTP request with NO single-chapter retries, got {sent:?}"
        );
    }

    // ---- 4C: Poisoned cache recovery ----
    // Corrupt chapter 2's cached XHTML on disk so build_epub fails with MalformedXhtml.
    // Verify download_book_with_progress automatically purges packages/ and public/,
    // so the next retry downloads fresh chapters and succeeds!
    server.state.lock().unwrap().chapter_fault = None;
    server.state.lock().unwrap().chapter_requests.clear();

    let ch2_cache_dir = request.work_dir.join("packages/2/Text");
    let ch2_entries: Vec<PathBuf> = fs::read_dir(&ch2_cache_dir)
        .expect("read cached ch2 Text dir")
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(ch2_entries.len(), 1);
    fs::write(
        &ch2_entries[0],
        b"<html><body><p>corrupted-unclosed-tag</body></html>",
    )
    .unwrap();

    let poison_err = download_book_with_progress(&mut client, &toc, &request, |_, _| {})
        .expect_err("corrupted cached XHTML must fail build_epub");
    assert!(
        matches!(poison_err, DownloadError::Epub(_)),
        "expected Epub error on poisoned cache, got {poison_err:?}"
    );
    assert!(
        !request.work_dir.join("packages").exists(),
        "poisoned packages/ cache MUST be purged automatically"
    );
    assert!(
        !request.work_dir.join("public").exists(),
        "poisoned public/ cache MUST be purged automatically"
    );

    // Retry now fetches fresh chapters and succeeds cleanly!
    let recovered = download_book_with_progress(&mut client, &toc, &request, |_, _| {})
        .expect("retry after automatic cache purge must succeed");
    assert_eq!(recovered.downloaded, (1..=25).collect::<Vec<i64>>());
    assert!(recovered.cached.is_empty());
    assert_eq!(server.state.lock().unwrap().chapter_requests.len(), 5);

    // ---- 4D: Partial-batch missing chapter fallback (batch omits ch8 -> ONLY ch8 retried) ----
    let base_builder = make_25_chapter_zip_builder();
    let mut partial_state = MockState::new();
    partial_state.chapter_builder = Some(Arc::new(move |requested: &[i64]| {
        if requested.len() > 1 && requested.contains(&8) {
            let filtered: Vec<i64> = requested.iter().copied().filter(|&u| u != 8).collect();
            base_builder(&filtered)
        } else {
            base_builder(requested)
        }
    }));
    let partial_srv = MockWeReadServer::start(partial_state);
    let partial_creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let mut partial_client = WeReadClient::new(partial_srv.transport(), partial_creds)
        .with_base_url(partial_srv.base_url());
    let partial_dir = ScratchDir::new("partial-batch");
    let partial_req = DownloadRequest {
        book_id: "300025".to_string(),
        title: "部分缺失批次回退".to_string(),
        format: SourceFormat::Epub,
        book_version: "77".to_string(),
        chapters: (1..=10).collect(),
        epub_path: partial_dir.path().join("book.epub"),
        mapping_path: partial_dir.path().join("books.json"),
        work_dir: partial_dir.path().join("cache"),
    };
    let mut partial_prog = Vec::new();
    let partial_out =
        download_book_with_progress(&mut partial_client, &toc, &partial_req, |done, total| {
            partial_prog.push((done, total));
        })
        .expect("partial batch missing ch8 must recover with single retry of ch8");
    assert_eq!(
        partial_prog,
        (1..=10).map(|i| (i, 10)).collect::<Vec<_>>(),
        "partial batch fallback must keep progress strictly monotonic 1..=10"
    );
    assert_eq!(
        partial_srv.state.lock().unwrap().chapter_requests,
        vec![
            vec![0, 1, 2, 3],
            vec![4, 5, 6],
            vec![7, 8, 9],
            vec![8],
            vec![10],
        ]
    );
    assert_eq!(partial_out.downloaded.len(), 10);

    // ---- 4E: Transport TooLarge on batch -> falls back cleanly to single-chapter fetches ----
    let mut big_state = MockState::new();
    big_state.chapter_builder = Some(Arc::new(|requested: &[i64]| {
        let mut owned: Vec<(String, Vec<u8>)> = Vec::new();
        let mut manifest = Vec::new();
        if requested.contains(&0) {
            manifest.push(json!({"chapterUid": 0, "files": ["Text/0.xhtml"]}));
            owned.push((
                "Text/0.xhtml".to_string(),
                b"<html><body>pub</body></html>".to_vec(),
            ));
        }
        for &uid in requested {
            if uid == 0 {
                continue;
            }
            let href = format!("Text/:::**:**::ch{uid:02}.xhtml");
            let img = format!("Images/:******:img{uid:02}.jpg");
            manifest.push(json!({"chapterUid": uid, "files": [href]}));
            // 3 KiB per chapter: adaptive 3-chapter batch (~10 KiB) exceeds 8 KiB
            // transport limit, while single-chapter (~3.5 KiB) & 2-chapter (~6.8 KiB) fit!
            owned.push((img.clone(), vec![b'J'; 3 * 1024]));
            owned.push((
                href,
                format!(
                    "<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\">\
                     <body><p>Big ch {uid}</p><img src=\"../{img}\"/></body></html>"
                )
                .into_bytes(),
            ));
        }
        owned.push((
            "info.txt".to_string(),
            serde_json::to_vec(&json!({"chapters": manifest})).unwrap(),
        ));
        let refs: Vec<(&str, &[u8])> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        build_zipcrypto_zip(&refs, TEST_ZIP_PASSWORD)
    }));
    let big_srv = MockWeReadServer::start(big_state);
    let tight_transport = ReadestTransport::with_timeout_and_limit(big_srv.policy(), 15, 8 * 1024);
    let big_creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let mut big_client =
        WeReadClient::new(tight_transport, big_creds).with_base_url(big_srv.base_url());
    let big_dir = ScratchDir::new("toolarge-fallback");
    let big_req = DownloadRequest {
        book_id: "300025".to_string(),
        title: "超限批次回退".to_string(),
        format: SourceFormat::Epub,
        book_version: "77".to_string(),
        chapters: (1..=5).collect(),
        epub_path: big_dir.path().join("book.epub"),
        mapping_path: big_dir.path().join("books.json"),
        work_dir: big_dir.path().join("cache"),
    };
    let mut big_prog = Vec::new();
    let big_out = download_book_with_progress(&mut big_client, &toc, &big_req, |done, total| {
        big_prog.push((done, total));
    })
    .expect("TooLarge batch must fall back to single-chapter fetches");
    assert_eq!(big_prog, (1..=5).map(|i| (i, 5)).collect::<Vec<_>>());
    assert_eq!(big_out.downloaded, vec![1, 2, 3, 4, 5]);
    assert_eq!(
        big_srv.state.lock().unwrap().chapter_requests,
        vec![vec![0, 1, 2, 3], vec![0, 1], vec![2], vec![3], vec![4, 5],]
    );
}

// ---------------------------------------------------------------------------
// Scenario 5: Progress Sync, Arbiter/WriteFence, Web Writes, Reading Time & Notes
// ---------------------------------------------------------------------------

#[test]
fn scenario_5_progress_sync_arbiter_write_fence_readtime_and_notes() {
    let mut state = MockState::new();
    // Build a clean 2-chapter book so we have a real EPUB for PositionMap/AnchorMap/map_notes.
    state.chapter_builder = Some(Arc::new(|requested: &[i64]| {
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        let mut manifest = Vec::new();
        if requested.contains(&0) {
            manifest.push(json!({"chapterUid": 0, "files": ["Text/0.xhtml"]}));
            entries.push((
                "Text/0.xhtml".to_string(),
                b"<html><body>pub</body></html>".to_vec(),
            ));
            entries.push(("Styles/main.css".to_string(), b"p{margin:0;}".to_vec()));
        }
        if requested.contains(&1) {
            manifest.push(json!({"chapterUid": 1, "files": ["Text/ch1.xhtml"]}));
            entries.push((
                "Text/ch1.xhtml".to_string(),
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                 <html xmlns=\"http://www.w3.org/1999/xhtml\">\
                 <head><title>Ch1</title></head>\
                 <body>\
                   <p>春眠不觉晓，处处闻啼鸟。</p>\
                   <p>夜来风雨声，花落知多少。</p>\
                 </body>\
                 </html>"
                    .as_bytes()
                    .to_vec(),
            ));
        }
        if requested.contains(&2) {
            manifest.push(json!({"chapterUid": 2, "files": ["Text/ch2.xhtml"]}));
            entries.push((
                "Text/ch2.xhtml".to_string(),
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                 <html xmlns=\"http://www.w3.org/1999/xhtml\">\
                 <head><title>Ch2</title></head>\
                 <body>\
                   <p>白日依山尽，黄河入海流。</p>\
                   <p>欲穷千里目，更上一层楼。</p>\
                 </body>\
                 </html>"
                    .as_bytes()
                    .to_vec(),
            ));
        }
        entries.push((
            "info.txt".to_string(),
            serde_json::to_vec(&json!({"chapters": manifest})).unwrap(),
        ));
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        build_zipcrypto_zip(&refs, TEST_ZIP_PASSWORD)
    }));
    state.progress_response = json!({
        "bookId": "500001",
        "book": {
            "appId": "eink-app-500",
            "bookVersion": 42,
            "chapterUid": 1,
            "chapterIdx": 1,
            "chapterOffset": 14,
            "summary": "夜来风雨声，花落知多少。",
            "progress": 50,
            "readingTime": 600
        },
        "canFreeRead": 1,
        "timestamp": 1_795_006_800i64
    });
    state.bookmark_response = json!({
        "synckey": 55,
        "chapters": [
            {"chapterUid": 1, "chapterIdx": 1, "title": "春晓"},
            {"chapterUid": 2, "chapterIdx": 2, "title": "登鹳雀楼"}
        ],
        "updated": [
            {
                "bookmarkId": "bm-1",
                "bookId": "500001",
                "chapterUid": 1,
                "chapterIdx": 1,
                "range": "0-12",
                "markText": "春眠不觉晓，处处闻啼鸟。",
                "createTime": 1_795_000_100i64,
                "style": 0,
                "colorStyle": 1,
                "type": 1
            },
            {
                "bookmarkId": "bm-2",
                "bookId": "500001",
                "chapterUid": 2,
                "chapterIdx": 2,
                "range": "6-18",
                "markText": "黄河入海流。欲穷千里目，",
                "createTime": 1_795_000_200i64,
                "style": 0,
                "colorStyle": 2,
                "type": 1
            }
        ]
    });
    state.review_response = json!({
        "synckey": 88,
        "totalCount": 3,
        "hasMore": false,
        "reviews": [
            {
                // Attached thought matching bm-1's range (chapter 1, 0-12).
                "review": {
                    "reviewId": "rv-attached",
                    "content": "经典五言绝句开篇",
                    "abstract": "春眠不觉晓，处处闻啼鸟。",
                    "range": "0-12",
                    "chapterUid": 1,
                    "chapterIdx": 1,
                    "chapterName": "春晓",
                    "createTime": 1_795_000_150i64
                }
            },
            {
                // Standalone chapter thought without range -> degrades to Chapter anchor.
                "review": {
                    "reviewId": "rv-chapter",
                    "content": "这一章意境极佳",
                    "abstract": "",
                    "range": "",
                    "chapterUid": 2,
                    "chapterIdx": 2,
                    "chapterName": "登鹳雀楼",
                    "createTime": 1_795_000_250i64
                }
            },
            {
                // Whole-book review (chapterUid=0) -> Unplaced.
                "review": {
                    "reviewId": "rv-book",
                    "content": "全书总评：唐诗精选",
                    "abstract": "",
                    "range": "",
                    "chapterUid": 0,
                    "chapterIdx": 0,
                    "createTime": 1_795_000_300i64,
                    "star": 5,
                    "isFinish": 1
                }
            }
        ]
    });

    let server = MockWeReadServer::start(state);
    let base = server.base_url();
    let creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let mut client = WeReadClient::new(server.transport(), creds).with_base_url(&base);

    let toc = ChapterInfos {
        book_id: "500001".to_string(),
        synckey: Some(12),
        book_version: Some(42),
        chapters: vec![
            ChapterInfo {
                chapter_uid: Some(1),
                chapter_idx: Some(1),
                title: Some("春晓".to_string()),
                word_count: Some(24),
                price: Some(0.0),
                paid: Some(0),
                ..Default::default()
            },
            ChapterInfo {
                chapter_uid: Some(2),
                chapter_idx: Some(2),
                title: Some("登鹳雀楼".to_string()),
                word_count: Some(24),
                price: Some(0.0),
                paid: Some(0),
                ..Default::default()
            },
        ],
    };
    let dir = ScratchDir::new("sync-stack");
    let dl_req = DownloadRequest {
        book_id: "500001".to_string(),
        title: "唐诗两首".to_string(),
        format: SourceFormat::Epub,
        book_version: "42".to_string(),
        chapters: vec![1, 2],
        epub_path: dir.path().join("500001.epub"),
        mapping_path: dir.path().join("books.json"),
        work_dir: dir.path().join("cache"),
    };
    let outcome =
        download_book_with_progress(&mut client, &toc, &dl_req, |_, _| {}).expect("download");

    // 5A. E-Ink get_progress -> PositionMap/AnchorMap -> XPointer.
    let mut pos_map = PositionMap::from_spine("500001", "42", &outcome.spine, &toc.chapters);
    let anchors = AnchorMap::from_epub(&outcome.epub_path, &pos_map).expect("anchors");
    pos_map.apply_anchor_lengths(&anchors);

    let progress = client.get_progress("500001").expect("get_progress");
    let remote_pos = progress.book.as_ref().unwrap().position().unwrap();
    assert_eq!(remote_pos.chapter_uid, 1);
    assert_eq!(remote_pos.chapter_offset, 14);
    let remote_coord = WeReadCoordinate {
        chapter_uid: remote_pos.chapter_uid,
        chapter_idx: remote_pos.chapter_idx,
        chapter_offset: remote_pos.chapter_offset,
    };
    let xp = pos_map
        .xpointer_for(remote_coord, Some(&anchors))
        .expect("xpointer_for remote progress");
    assert_eq!(xp.precision, Precision::Exact);
    assert_eq!(xp.xpointer, "/body/DocFragment[1]/body/p[2]/text().2");
    let roundtrip = pos_map
        .from_xpointer(&xp.xpointer, Some(&anchors))
        .expect("from_xpointer");
    assert_eq!(
        roundtrip.position,
        EpubPosition {
            spine_index: 0,
            char_offset: 14
        }
    );

    // 5B. arbitrate + WriteFence across LocalNewer, RemoteNewer, Conflict, Aligned.
    let base_stamp = PositionStamp::new(1, 1, 14);
    let baseline = SyncBaseline::new(base_stamp, base_stamp);
    let local_moved = PositionStamp::new(2, 2, 10);
    let remote_moved = PositionStamp::new(1, 1, 20);

    assert_eq!(
        arbitrate(base_stamp, base_stamp, Some(baseline)),
        Verdict::Aligned
    );
    assert_eq!(
        arbitrate(local_moved, base_stamp, Some(baseline)),
        Verdict::LocalNewer
    );
    assert_eq!(
        arbitrate(base_stamp, remote_moved, Some(baseline)),
        Verdict::RemoteNewer
    );
    assert_eq!(
        arbitrate(local_moved, remote_moved, Some(baseline)),
        Verdict::Conflict
    );
    assert_eq!(arbitrate(local_moved, base_stamp, None), Verdict::Conflict);

    for non_write in [Verdict::Aligned, Verdict::RemoteNewer, Verdict::Conflict] {
        let mut f = WriteFence::new(non_write, true);
        assert_eq!(f.state(), FenceState::Closed);
        assert!(!f.begin_write());
    }
    let mut fence = WriteFence::new(Verdict::LocalNewer, true);
    assert_eq!(fence.state(), FenceState::Armed);
    assert!(fence.begin_write());
    assert_eq!(fence.state(), FenceState::Sent);
    assert!(
        !fence.begin_write(),
        "WriteFence must forbid duplicate in-flight write"
    );
    fence.unconfirmed();
    assert_eq!(fence.state(), FenceState::Unconfirmed);
    assert!(
        !fence.begin_write(),
        "Unconfirmed write must NEVER be blindly resent"
    );
    fence.reset(Verdict::LocalNewer, true);
    assert!(fence.begin_write());

    // 5C. Web reader_state -> enter + period /web/book/read -> confirmed write; then -2012 -> WebAuthExpired.
    let mut web = WeReadWebClient::new(server.transport()).with_base_url(&base);
    web.login_page().unwrap();
    let web_uid = web.login_uid().unwrap();
    let _ = web.login_poll(&web_uid, "").unwrap();
    let _ = web.login_poll(&web_uid, "").unwrap();
    assert!(matches!(
        web.login_poll(&web_uid, "4829").unwrap(),
        WebLoginPoll::Confirmed(_)
    ));

    let reader_state = web.reader_state("500001").expect("reader_state");
    assert_eq!(reader_state.psvts, TEST_PSVTS);
    assert_eq!(reader_state.pclts.as_deref(), Some(TEST_PCLTS));
    assert_eq!(reader_state.token.as_deref(), Some(TEST_WEB_READER_TOKEN));

    let write_input = ReadPayloadInput {
        book_id: "500001".to_string(),
        chapter_uid: 2,
        chapter_idx: 2,
        chapter_offset: 10,
        progress: 75,
        summary: "白日依山尽，黄河入海流。欲穷千里目，更上一层楼。".to_string(),
        psvts: reader_state.psvts.clone(),
        pclts: reader_state.pclts.clone(),
        app_id: web_app_id(WEB_USER_AGENT),
        now_s: 1_795_007_100,
        elapsed_s: 45,
        ts_ms: 1_795_007_100_456,
        rn: 456,
        token: reader_state.token.clone().unwrap(),
    };
    let enter_pl = enter_payload(&write_input);
    let period_pl = read_payload(&write_input);
    web.write_enter("500001", 2, &enter_pl)
        .expect("Web enter write confirmed");
    web.write_period("500001", 2, &period_pl)
        .expect("Web period write confirmed");
    fence.confirmed();
    assert_eq!(fence.state(), FenceState::Confirmed);
    assert!(!fence.is_dirty());

    let recorded_web_reads = server.state.lock().unwrap().web_reads.clone();
    assert_eq!(recorded_web_reads.len(), 2);
    assert!(recorded_web_reads[0].get("sg").is_none(), "enter has no sg");
    assert!(recorded_web_reads[1].get("sg").is_some(), "period has sg");
    assert_eq!(recorded_web_reads[1]["b"], we("500001"));
    assert_eq!(recorded_web_reads[1]["ps"], TEST_PSVTS);

    // Inject succ:0 on Web write -> MUST return UnconfirmedWrite (never silently ignored).
    server.state.lock().unwrap().force_web_unconfirmed = true;
    let unconfirmed_err = web
        .write_period("500001", 2, &period_pl)
        .expect_err("succ:0 must fail as UnconfirmedWrite");
    assert!(
        matches!(unconfirmed_err, WeReadError::UnconfirmedWrite(_)),
        "expected UnconfirmedWrite, got {unconfirmed_err:?}"
    );

    // Inject -2012 on Web write -> MUST return WebAuthExpired and lock fence.
    server.state.lock().unwrap().force_web_expire = true;
    let expired_err = web
        .write_period("500001", 2, &period_pl)
        .expect_err("expired web skey must fail");
    assert!(
        matches!(expired_err, WeReadError::WebAuthExpired),
        "expected WebAuthExpired, got {expired_err:?}"
    );
    fence.expired();
    assert_eq!(fence.state(), FenceState::Expired);

    // Also verify E-Ink heartbeat book_read_echo carries valid inner signature & echoed coords.
    client
        .book_read_echo("500001", 1_795_010_650, 789, READ_TIME_ZONE)
        .expect("signed book_read_echo accepted by strict server");
    let echoed = server.state.lock().unwrap().eink_reads[0].clone();
    assert_eq!(echoed["appId"], "eink-app-500");
    assert_eq!(echoed["chapterUid"], 1);
    assert_eq!(echoed["chapterOffset"], 14);
    assert_eq!(echoed["summary"], "夜来风雨声，花落知多少。");

    // 5D. Reading time queue + signed batchUploadProgress (server verifies both inner & outer signatures!).
    let mut rt_queue = ReadtimeQueue::new();
    // Span crossing a GMT+8 hour boundary: 1_795_008_600 + 500s across boundary or 2 spans.
    rt_queue.enqueue(
        "500001",
        &[
            StaySpan::new(1_795_010_100, 300).unwrap(),
            StaySpan::new(1_795_010_400, 240).unwrap(),
        ],
    );
    assert!(rt_queue.ready(1_795_010_700));
    let batch = rt_queue.take_batch("500001", 600).expect("take batch");
    assert!(rt_queue.begin_send(batch.clone(), 1_795_010_700));
    let mut upload_req = BatchUploadRequest::new(
        "500001",
        42,
        intervals_for(&batch),
        1_795_010_700,
        READ_TIME_ZONE,
    );
    upload_req.random = 123;
    upload_req.outer_timestamp_ms = 1_795_010_700_000;
    upload_req.outer_random = 456;
    client
        .batch_upload_progress(&upload_req)
        .expect("signed batchUploadProgress accepted by strict server");
    rt_queue.confirm();
    assert_eq!(rt_queue.pending_seconds(), 0);
    assert_eq!(server.state.lock().unwrap().batch_uploads.len(), 1);
    let uploaded_book = server.state.lock().unwrap().batch_uploads[0]["books"][0].clone();
    assert_eq!(uploaded_book["readingTime"], 540);
    assert_eq!(uploaded_book["hours"].as_array().unwrap().len(), 2);

    // 5E. bookmarklist + review/list -> BookNotes -> map_notes with valid XPointer ranges!
    let bm_list = client.bookmark_list("500001", 0).expect("bookmark_list");
    let rv_list = client.review_list("500001", 0, 50).expect("review_list");
    let mut book_notes = BookNotes::default();
    assert!(book_notes.merge_bookmarks(&bm_list, 1_795_010_800));
    assert!(book_notes.merge_reviews(&rv_list, 1_795_010_800));
    assert_eq!(book_notes.bookmark_count(), 2);
    assert_eq!(book_notes.review_count(), 3);

    let composed = book_notes.compose();
    // 2 highlights (bm-1 absorbs rv-attached) + 2 standalone thoughts (rv-chapter, rv-book) = 4 notes.
    assert_eq!(composed.len(), 4);
    let bm1 = composed.iter().find(|n| n.id == "bm-1").expect("bm-1");
    assert_eq!(bm1.kind, NoteKind::Highlight);
    assert_eq!(bm1.thought.as_deref(), Some("经典五言绝句开篇"));

    let mapped = map_notes(&composed, &pos_map, &anchors);
    assert_eq!(mapped.stats.total, 4);
    assert_eq!(
        mapped.stats.exact, 2,
        "both ranged highlights must map exactly"
    );
    assert_eq!(mapped.stats.cross_segment, 1, "bm-2 crosses p[1] -> p[2]");
    assert_eq!(
        mapped.stats.chapter, 1,
        "rv-chapter degrades to Chapter anchor"
    );
    assert_eq!(mapped.stats.unplaced, 1, "rv-book (uid 0) is Unplaced");

    let mapped_bm1 = mapped.notes.iter().find(|m| m.note.id == "bm-1").unwrap();
    assert_eq!(
        mapped_bm1.anchor,
        NoteAnchor::Exact {
            start: "/body/DocFragment[1]/body/p[1]/text().0".to_string(),
            end: "/body/DocFragment[1]/body/p[1]/text().11".to_string(),
        }
    );
    let mapped_bm2 = mapped.notes.iter().find(|m| m.note.id == "bm-2").unwrap();
    assert!(mapped_bm2.cross_segment);
    assert_eq!(
        mapped_bm2.anchor,
        NoteAnchor::Exact {
            start: "/body/DocFragment[2]/body/p[1]/text().6".to_string(),
            end: "/body/DocFragment[2]/body/p[2]/text().5".to_string(),
        }
    );
    let mapped_rv_ch = mapped
        .notes
        .iter()
        .find(|m| m.note.id == "rv-chapter")
        .unwrap();
    assert_eq!(mapped_rv_ch.reason, Some(DegradeReason::NoRange));
    assert!(matches!(mapped_rv_ch.anchor, NoteAnchor::Chapter { .. }));
    let mapped_rv_book = mapped
        .notes
        .iter()
        .find(|m| m.note.id == "rv-book")
        .unwrap();
    assert_eq!(mapped_rv_book.reason, Some(DegradeReason::UnknownChapter));
    assert_eq!(mapped_rv_book.anchor, NoteAnchor::Unplaced);
}

// ---------------------------------------------------------------------------
// Scenario 6: Large 60-Chapter Book Speed & Batch Efficiency Stress Test
// ---------------------------------------------------------------------------

#[test]
fn scenario_6_large_60_chapter_book_speed_and_batch_efficiency() {
    let mut state = MockState::new();
    state.chapter_builder = Some(Arc::new(|requested: &[i64]| {
        let mut owned: Vec<(String, Vec<u8>)> = Vec::new();
        let mut manifest = Vec::new();
        if requested.contains(&0) {
            manifest.push(json!({"chapterUid": 0, "files": ["Text/0.xhtml"]}));
            owned.push((
                "Text/0.xhtml".to_string(),
                b"<html><body>pub</body></html>".to_vec(),
            ));
            owned.push((
                "Styles/book.css".to_string(),
                b"body { font-size: 1em; }".to_vec(),
            ));
        }
        for &uid in requested {
            if uid == 0 {
                continue;
            }
            let href = format!("Text/:::**:**::large_{uid:03}.xhtml");
            let img = format!("Images/:******:fig_{uid:03}.jpg");
            manifest.push(json!({"chapterUid": uid, "files": [href]}));
            owned.push((img.clone(), format!("IMG-60CH-{uid:03}").into_bytes()));
            owned.push((
                href,
                format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                     <html xmlns=\"http://www.w3.org/1999/xhtml\">\
                     <head><title>Ch{uid}</title></head>\
                     <body><p>第六十章压力测试正文第{uid}节。</p><img src=\"../{img}\"/></body>\
                     </html>"
                )
                .into_bytes(),
            ));
        }
        owned.push((
            "info.txt".to_string(),
            serde_json::to_vec(&json!({"chapters": manifest})).unwrap(),
        ));
        let refs: Vec<(&str, &[u8])> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        build_zipcrypto_zip(&refs, TEST_ZIP_PASSWORD)
    }));

    let server = MockWeReadServer::start(state);
    let creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let mut client = WeReadClient::new(server.transport(), creds).with_base_url(server.base_url());

    let mut toc_chapters = Vec::with_capacity(61);
    toc_chapters.push(ChapterInfo {
        chapter_uid: Some(0),
        chapter_idx: Some(0),
        title: Some("Public".to_string()),
        price: Some(0.0),
        paid: Some(0),
        ..Default::default()
    });
    for uid in 1..=60 {
        toc_chapters.push(ChapterInfo {
            chapter_uid: Some(uid),
            chapter_idx: Some(uid),
            title: Some(format!("第{uid}章")),
            word_count: Some(200),
            price: Some(0.0),
            paid: Some(0),
            ..Default::default()
        });
    }
    let toc = ChapterInfos {
        book_id: "600060".to_string(),
        synckey: Some(6000),
        book_version: Some(60),
        chapters: toc_chapters,
    };

    let dir = ScratchDir::new("60ch-stress");
    let request = DownloadRequest {
        book_id: "600060".to_string(),
        title: "六十章高速批次测试".to_string(),
        format: SourceFormat::Epub,
        book_version: "60".to_string(),
        chapters: (1..=60).collect(),
        epub_path: dir.path().join("600060.epub"),
        mapping_path: dir.path().join("books.json"),
        work_dir: dir.path().join("cache"),
    };

    let started = Instant::now();
    let mut progress = Vec::new();
    let outcome = download_book_with_progress(&mut client, &toc, &request, |done, total| {
        progress.push((done, total));
    })
    .expect("60-chapter download must succeed");
    let elapsed = started.elapsed();

    let reqs = server.state.lock().unwrap().chapter_requests.clone();
    assert_eq!(
        reqs.len(),
        6,
        "60 chapters / adaptive batch_size 10 = 6 requests (10x fewer round-trips than single-chapter), got {}",
        reqs.len()
    );
    let ch0_reqs = reqs.iter().filter(|r| r.contains(&0)).count();
    assert_eq!(ch0_reqs, 1, "chapter 0 must be requested ONLY ONCE");
    assert_eq!(reqs[0], vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    assert_eq!(progress, (1..=60).map(|i| (i, 60)).collect::<Vec<_>>());
    assert_eq!(outcome.spine.len(), 60);
    assert!(
        elapsed < Duration::from_secs(10),
        "60-chapter loopback download took too long: {elapsed:?}"
    );

    let entries = read_epub_zip(&outcome.epub_path);
    for uid in 1..=60 {
        let img = format!("OEBPS/Images/:******:fig_{uid:03}.jpg");
        assert_eq!(
            find_epub_entry(&entries, &img),
            format!("IMG-60CH-{uid:03}").as_bytes()
        );
    }
}

// ---------------------------------------------------------------------------
// Scenario 7: Real KO2 Calibre Root coverpage.html, Alternate Folders & Dirty HTML
// ---------------------------------------------------------------------------

#[test]
fn scenario_7_real_ko2_calibre_root_coverpage_and_dirty_html() {
    const ROOT_CSS: &[u8] = b"body { font-family: serif; line-height: 1.6; }\n";
    const ROOT_COVER_JPG: &[u8] = b"\xFF\xD8\xFF\xE0CALIBRE-ROOT-COVER-JPG-BYTES";
    const FIG1_PNG: &[u8] = b"\x89PNG\r\n\x1a\nALT-FOLDER-FIG1-PNG-BYTES";
    const CUSTOM_TTF: &[u8] = b"\x00\x01\x00\x00CUSTOM-PUBLISHER-FONT-TTF";
    const ALT_BOOK_CSS: &[u8] =
        b"@font-face { font-family: 'CustomBook'; src: url('../Fonts/custom.ttf'); }\nbody { font-family: 'CustomBook'; }\n";

    let mut state = MockState::new();
    state.chapter_builder = Some(Arc::new(|requested: &[i64]| {
        let mut owned: Vec<(String, Vec<u8>)> = Vec::new();
        let mut manifest = Vec::new();

        if requested.contains(&0) {
            owned.push(("stylesheet.css".to_string(), ROOT_CSS.to_vec()));
            owned.push(("cover.jpg".to_string(), ROOT_COVER_JPG.to_vec()));
            owned.push(("Fonts/custom.ttf".to_string(), CUSTOM_TTF.to_vec()));
            owned.push(("css/book.css".to_string(), ALT_BOOK_CSS.to_vec()));
        }

        if requested.contains(&1) {
            // Reproduces exact live KO2 failure: MissingPackageFile { chapter_uid: 1, href: "coverpage.html" }
            // Root coverpage.html referencing root stylesheet.css & root cover.jpg with unclosed HTML void tags.
            manifest.push(json!({
                "chapterUid": 1,
                "files": ["coverpage.html"]
            }));
            owned.push((
                "coverpage.html".to_string(),
                b"<html>\n\
                  <head>\n\
                    <title>Cover</title>\n\
                    <link rel=\"stylesheet\" type=\"text/css\" href=\"stylesheet.css\">\n\
                    <meta charset=\"utf-8\">\n\
                  </head>\n\
                  <body>\n\
                    <h1>Calibre \xe6\xa0\xb9\xe7\x9b\xae\xe5\xbd\x95\xe5\xb0\x81\xe9\x9d\xa2</h1>\n\
                    <hr>\n\
                    <img src=\"cover.jpg\" alt=\"Book Cover\">\n\
                    <br>\n\
                    <p>\xe5\xb0\x81\xe9\x9d\xa2\xe5\x86\x85\xe5\xae\xb9\xe8\xaf\xb4\xe6\x98\x8e\xe6\x96\x87\xe5\xad\x97</p>\n\
                  </body>\n\
                  </html>\n"
                    .to_vec(),
            ));
        }

        if requested.contains(&2) {
            // Alternate lowercase folder + .htm extension + ../images, ../Fonts, ../css references.
            manifest.push(json!({
                "chapterUid": 2,
                "files": ["xhtml/part0001.htm"]
            }));
            owned.push(("images/fig1.png".to_string(), FIG1_PNG.to_vec()));
            owned.push((
                "xhtml/part0001.htm".to_string(),
                b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                  <html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
                  <head>\n\
                    <title>Part 0001</title>\n\
                    <link rel=\"stylesheet\" type=\"text/css\" href=\"../css/book.css\"/>\n\
                  </head>\n\
                  <body>\n\
                    <h2>\xe7\xac\xac\xe4\xba\x8c\xe7\xab\xa0\xef\xbc\x9a\xe5\xb0\x8f\xe5\x86\x99\xe7\x9b\xae\xe5\xbd\x95\xe4\xb8\x8e\xe8\x87\xaa\xe5\xae\x9a\xe4\xb9\x89\xe5\xad\x97\xe4\xbd\x93</h2>\n\
                    <p>\xe5\xbc\x95\xe7\x94\xa8\xe7\x9b\xb8\xe5\xaf\xb9\xe8\xb7\xaf\xe5\xbe\x84\xe5\x9b\xbe\xe7\x89\x87\xe4\xb8\x8e\xe5\xad\x97\xe4\xbd\x93\xe3\x80\x82</p>\n\
                    <img src=\"../images/fig1.png\" alt=\"Figure 1\"/>\n\
                  </body>\n\
                  </html>\n"
                    .to_vec(),
            ));
        }

        if requested.contains(&3) {
            // Nested OEBPS/Text/ch3.xhtml with <!DOCTYPE html>, &nbsp;, unclosed <br>, and bare A & B.
            manifest.push(json!({
                "chapterUid": 3,
                "files": ["OEBPS/Text/ch3.xhtml"]
            }));
            owned.push((
                "OEBPS/Text/ch3.xhtml".to_string(),
                b"<!DOCTYPE html>\n\
                  <html xmlns=\"http://www.w3.org/1999/xhtml\">\n\
                  <head><title>Chapter 3</title></head>\n\
                  <body>\n\
                    <h2>\xe7\xac\xac\xe4\xb8\x89\xe7\xab\xa0\xef\xbc\x9a\xe8\x84\x8f HTML \xe4\xb8\x8e\xe5\xb5\x8c\xe5\xa5\x97 OEBPS</h2>\n\
                    <p>\xe7\xac\xac\xe4\xb8\x80\xe8\xa1\x8c&nbsp;\xe4\xb8\x8d\xe6\x8d\xa2\xe8\xa1\x8c\xe7\xa9\xba\xe6\xa0\xbc<br>\xe7\xac\xac\xe4\xba\x8c\xe8\xa1\x8c A & B \xe8\xa3\xb8\xe7\xac\xa6\xe5\x8f\xb7\xe3\x80\x82</p>\n\
                  </body>\n\
                  </html>\n"
                    .to_vec(),
            ));
        }

        owned.push((
            "info.txt".to_string(),
            serde_json::to_vec(&json!({ "chapters": manifest })).unwrap(),
        ));
        let refs: Vec<(&str, &[u8])> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        build_zipcrypto_zip(&refs, TEST_ZIP_PASSWORD)
    }));

    let server = MockWeReadServer::start(state);
    let creds = Credentials::new(
        TEST_VID,
        "access-initial-1",
        "rt-keep-original-1",
        TEST_DEVICE_ID,
    );
    let mut client = WeReadClient::new(server.transport(), creds).with_base_url(server.base_url());

    let toc = ChapterInfos {
        book_id: "700001".to_string(),
        synckey: Some(7001),
        book_version: Some(7),
        chapters: vec![
            ChapterInfo {
                chapter_uid: Some(0),
                chapter_idx: Some(0),
                title: Some("Public".to_string()),
                price: Some(0.0),
                paid: Some(0),
                ..Default::default()
            },
            ChapterInfo {
                chapter_uid: Some(1),
                chapter_idx: Some(1),
                title: Some("封面".to_string()),
                word_count: Some(20),
                price: Some(0.0),
                paid: Some(0),
                ..Default::default()
            },
            ChapterInfo {
                chapter_uid: Some(2),
                chapter_idx: Some(2),
                title: Some("第二章".to_string()),
                word_count: Some(40),
                price: Some(0.0),
                paid: Some(0),
                ..Default::default()
            },
            ChapterInfo {
                chapter_uid: Some(3),
                chapter_idx: Some(3),
                title: Some("第三章".to_string()),
                word_count: Some(40),
                price: Some(0.0),
                paid: Some(0),
                ..Default::default()
            },
        ],
    };

    let dir = ScratchDir::new("ko2-calibre-root-coverpage");
    let request = DownloadRequest {
        book_id: "700001".to_string(),
        title: "KO2 实测 Calibre 根路径封面".to_string(),
        format: SourceFormat::Epub,
        book_version: "7".to_string(),
        chapters: vec![1, 2, 3],
        epub_path: dir.path().join("700001.epub"),
        mapping_path: dir.path().join("books.json"),
        work_dir: dir.path().join("cache"),
    };

    let mut progress = Vec::new();
    let outcome = download_book_with_progress(&mut client, &toc, &request, |done, total| {
        progress.push((done, total));
    })
    .expect("Calibre root coverpage.html + alternate folders + dirty HTML must download cleanly");

    assert_eq!(progress, vec![(1, 3), (2, 3), (3, 3)]);
    assert_eq!(outcome.downloaded, vec![1, 2, 3]);
    assert_eq!(outcome.spine.len(), 3);
    assert_eq!(outcome.spine[0].href, "coverpage.html");
    assert_eq!(outcome.spine[1].href, "xhtml/part0001.htm");
    assert_eq!(outcome.spine[2].href, "OEBPS/Text/ch3.xhtml");

    let entries = read_epub_zip(&outcome.epub_path);
    assert_eq!(entries[0].name, "mimetype");
    assert_eq!(entries[0].data, EPUB_MIMETYPE.as_bytes());

    // Assert ALL root & alternate-folder assets are packed cleanly into the rebuilt EPUB.
    assert_eq!(find_epub_entry(&entries, "OEBPS/stylesheet.css"), ROOT_CSS);
    assert_eq!(find_epub_entry(&entries, "OEBPS/cover.jpg"), ROOT_COVER_JPG);
    assert_eq!(find_epub_entry(&entries, "OEBPS/images/fig1.png"), FIG1_PNG);
    assert_eq!(
        find_epub_entry(&entries, "OEBPS/Fonts/custom.ttf"),
        CUSTOM_TTF
    );
    assert_eq!(
        find_epub_entry(&entries, "OEBPS/css/book.css"),
        ALT_BOOK_CSS
    );

    // Assert EVERY chapter XHTML is packed at its exact OEBPS/<href> path and parses
    // cleanly via roxmltree with allow_dtd: true.
    let expected_xhtml_paths = [
        "OEBPS/coverpage.html",
        "OEBPS/xhtml/part0001.htm",
        "OEBPS/OEBPS/Text/ch3.xhtml",
    ];
    for path in expected_xhtml_paths {
        let bytes = find_epub_entry(&entries, path);
        let text = std::str::from_utf8(bytes)
            .unwrap_or_else(|e| panic!("{path} must be valid UTF-8: {e}"));
        let doc = Document::parse_with_options(
            text,
            ParsingOptions {
                allow_dtd: true,
                ..ParsingOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{path} failed roxmltree parse: {e}\n{text}"));
        if path == "OEBPS/coverpage.html" {
            assert!(
                doc.descendants()
                    .any(|n| n.has_tag_name("img") && n.attribute("src") == Some("cover.jpg")),
                "repaired coverpage.html must preserve <img src=\"cover.jpg\"/>"
            );
            assert!(
                doc.descendants().any(
                    |n| n.has_tag_name("link") && n.attribute("href") == Some("stylesheet.css")
                ),
                "repaired coverpage.html must preserve root stylesheet.css link"
            );
        }
        if path == "OEBPS/OEBPS/Text/ch3.xhtml" {
            let full_text: String = doc
                .descendants()
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join("");
            assert!(
                full_text.contains("A & B") && full_text.contains('\u{00a0}'),
                "ch3.xhtml must preserve decoded &nbsp; and A & B text; got: {full_text:?}"
            );
        }
    }

    // Assert OPF manifest lists all root & alternate-folder files and cover metadata.
    let opf_text = std::str::from_utf8(find_epub_entry(&entries, "OEBPS/content.opf"))
        .expect("content.opf UTF-8");
    let opf_doc = Document::parse_with_options(
        opf_text,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )
    .expect("content.opf must parse cleanly");
    let opf_hrefs: BTreeSet<&str> = opf_doc
        .descendants()
        .filter(|n| n.has_tag_name("item"))
        .filter_map(|n| n.attribute("href"))
        .collect();
    for required_href in [
        "coverpage.html",
        "stylesheet.css",
        "cover.jpg",
        "xhtml/part0001.htm",
        "images/fig1.png",
        "Fonts/custom.ttf",
        "css/book.css",
        "OEBPS/Text/ch3.xhtml",
    ] {
        assert!(
            opf_hrefs.contains(required_href),
            "OEBPS/content.opf manifest missing href {required_href}; got {opf_hrefs:?}"
        );
    }

    // Verify PositionMap::from_spine + AnchorMap::from_epub succeed with non-empty anchors
    // for all 3 chapters!
    let mut pos_map = PositionMap::from_spine("700001", "7", &outcome.spine, &toc.chapters);
    let anchors = AnchorMap::from_epub(&outcome.epub_path, &pos_map)
        .expect("AnchorMap::from_epub must succeed for root/alternate/nested XHTML");
    pos_map.apply_anchor_lengths(&anchors);

    assert_eq!(anchors.chapters.len(), 3);
    for uid in [1i64, 2, 3] {
        let ch = anchors
            .chapter(uid)
            .unwrap_or_else(|| panic!("missing anchors for chapterUid {uid}"));
        assert!(
            !ch.anchors.is_empty(),
            "chapterUid {uid} must have non-empty anchors"
        );
        assert!(
            ch.total_chars > 0,
            "chapterUid {uid} must have non-zero visible character length"
        );
        let mapped = pos_map
            .xpointer_for(
                WeReadCoordinate {
                    chapter_uid: uid,
                    chapter_idx: uid,
                    chapter_offset: 1,
                },
                Some(&anchors),
            )
            .unwrap_or_else(|e| panic!("xpointer_for chapterUid {uid} failed: {e}"));
        assert_eq!(mapped.precision, Precision::Exact);
        assert!(
            mapped
                .xpointer
                .starts_with(&format!("/body/DocFragment[{uid}]/")),
            "unexpected xpointer for chapterUid {uid}: {}",
            mapped.xpointer
        );
    }
}
