// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cover thumbnails for the e-ink library view.
//!
//! Pipeline: raw cover bytes → decode to 8-bit luminance → area-average
//! downscale to the thumbnail width → Floyd–Steinberg dither to the 16 gray
//! levels the PW4 panel shows → binary PGM in the cache, named after the MD5
//! of the source image.
//!
//! Why these choices:
//!
//! - **Grayscale early.** The panel has no colour. JPEG decoding goes
//!   straight to luminance and uses the DCT scaling of `jpeg-decoder` (1/2,
//!   1/4 or 1/8 size during the IDCT), so a 1600 × 2560 cover is decoded at
//!   200 × 320 at a fraction of the cost. That matters on the 1 GHz A9.
//! - **16 levels, dithered.** GL16 and GC16 waveforms render 16 levels
//!   (0x00, 0x11, ... 0xFF). Values in between are rounded by the EPDC, which
//!   bands smooth gradients. Dithering ourselves keeps gradients smooth.
//!   Floyd–Steinberg beats ordered dithering for still images; the usual
//!   argument for ordered dither (stable patterns under partial refresh)
//!   does not apply to a thumbnail that is drawn once.
//! - **PGM.** Trivial to write and read, viewable on any desktop, and the
//!   pixel data is the exact byte layout of the 8 bpp framebuffer, so the UI
//!   can blit it without converting.
//! - **Content-keyed cache.** The key is the MD5 of the cover image; the
//!   file name adds the thumbnail size, so re-scans, renames and duplicate
//!   books never redo the work, a changed cover gets a new file, and one
//!   source image can be cached at several sizes (the shelf's and the
//!   screensaver's) without a second decode on the way.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Thumbnail geometry. Width is fixed; height follows the aspect ratio but is
/// clamped so that a panorama or a strip does not produce an absurd tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbSpec {
    pub width: u32,
    pub max_height: u32,
}

impl Default for ThumbSpec {
    /// 200 px is about 1.7 cm on the 300 ppi panel: five covers per row in
    /// portrait with margins, readable titles on most covers.
    fn default() -> Self {
        ThumbSpec {
            width: 200,
            max_height: 320,
        }
    }
}

impl ThumbSpec {
    /// The screensaver's cover: the 560 × 747 slot of the sleep screen, with
    /// the height left free so a 2:3 cover keeps its shape instead of being
    /// squeezed. Made on demand, not during a scan: only the book being read
    /// ever needs it.
    pub fn large() -> Self {
        ThumbSpec {
            width: 560,
            max_height: 840,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
    Gif,
    Webp,
}

pub fn sniff_image(b: &[u8]) -> Option<ImageKind> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageKind::Jpeg)
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if b.starts_with(b"GIF8") {
        Some(ImageKind::Gif)
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else {
        None
    }
}

pub fn is_image_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".gif", ".webp"]
        .iter()
        .any(|e| l.ends_with(e))
        && !l.starts_with("__macosx/")
}

/// An 8-bit grayscale image, row-major, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gray {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Refuse to fully decode images larger than this (PNG has no scaled
/// decode); 40 MP of luminance is already 40 MB on a 512 MB device.
pub const MAX_PIXELS: u64 = 40_000_000;

/// Decodes a JPEG or PNG to luminance. JPEGs are decoded at the smallest DCT
/// scale that is still at least `min_width` wide.
pub fn decode_gray(data: &[u8], min_width: u32) -> Result<Gray> {
    match sniff_image(data) {
        Some(ImageKind::Jpeg) => decode_jpeg(data, min_width),
        Some(ImageKind::Png) => decode_png(data),
        Some(k) => Err(Error::format(format!("cover: {k:?} is not supported"))),
        None => Err(Error::format("cover: not an image")),
    }
}

fn decode_jpeg(data: &[u8], min_width: u32) -> Result<Gray> {
    let mut dec = jpeg_decoder::Decoder::new(Cursor::new(data));
    dec.read_info()
        .map_err(|e| Error::format(format!("jpeg: {e}")))?;
    let info = dec.info().ok_or_else(|| Error::format("jpeg: no header"))?;
    let (w, h) = (info.width as u32, info.height as u32);
    if w == 0 || h == 0 {
        return Err(Error::format("jpeg: empty image"));
    }
    let req_w = min_width.min(w);
    let req_h = ((req_w as u64 * h as u64).div_ceil(w as u64)) as u32;
    let (sw, sh) = dec
        .scale(
            req_w.min(u16::MAX as u32) as u16,
            req_h.clamp(1, u16::MAX as u32) as u16,
        )
        .map_err(|e| Error::format(format!("jpeg: {e}")))?;
    let px = dec
        .decode()
        .map_err(|e| Error::format(format!("jpeg: {e}")))?;
    let info = dec.info().ok_or_else(|| Error::format("jpeg: no header"))?;
    let (sw, sh) = (sw as u32, sh as u32);
    let n = (sw * sh) as usize;
    use jpeg_decoder::PixelFormat::*;
    let pixels: Vec<u8> = match info.pixel_format {
        L8 => px,
        L16 => px.as_chunks::<2>().0.iter().map(|c| c[0]).collect(),
        RGB24 => px
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| luma(c[0], c[1], c[2]))
            .collect(),
        // jpeg-decoder returns CMYK as stored; Adobe files store it inverted,
        // which this formula assumes, as most cover JPEGs in CMYK come from Adobe tools.
        CMYK32 => px
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let k = c[3] as u32;
                let l = luma(c[0], c[1], c[2]) as u32;
                (l * k / 255) as u8
            })
            .collect(),
    };
    if pixels.len() != n {
        return Err(Error::format("jpeg: unexpected pixel count"));
    }
    Ok(Gray {
        width: sw,
        height: sh,
        pixels,
    })
}

fn decode_png(data: &[u8]) -> Result<Gray> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec
        .read_info()
        .map_err(|e| Error::format(format!("png: {e}")))?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(Error::format("png: too large"));
    }
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or_else(|| Error::format("png: too large"))?
    ];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| Error::format(format!("png: {e}")))?;
    let buf = &buf[..frame.buffer_size()];
    use png::ColorType::*;
    // Transparent areas are composited over white, the colour of the page.
    let over_white = |v: u8, a: u8| ((v as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
    let pixels: Vec<u8> = match frame.color_type {
        Grayscale => buf.to_vec(),
        GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| over_white(c[0], c[1]))
            .collect(),
        Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| luma(c[0], c[1], c[2]))
            .collect(),
        Rgba => buf
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| over_white(luma(c[0], c[1], c[2]), c[3]))
            .collect(),
        Indexed => return Err(Error::format("png: palette not expanded")),
    };
    if pixels.len() != (frame.width * frame.height) as usize {
        return Err(Error::format("png: unexpected pixel count"));
    }
    Ok(Gray {
        width: frame.width,
        height: frame.height,
        pixels,
    })
}

/// Rec. 601 luma in integer arithmetic.
fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114 + 500) / 1000) as u8
}

/// Resizes with area averaging when shrinking (every source pixel
/// contributes in proportion to its overlap, so thin text on covers does not
/// alias away) and bilinear interpolation when enlarging.
pub fn resize(src: &Gray, w: u32, h: u32) -> Gray {
    let tmp = resize_axis(&src.pixels, src.width, src.height, w, true);
    let pixels = resize_axis(&tmp, w, src.height, h, false);
    Gray {
        width: w,
        height: h,
        pixels,
    }
}

/// How one axis is resampled: a box filter when shrinking, linear
/// interpolation when enlarging. The weights are worked out once per output
/// column, not per pixel: a page of covers is resampled on the way to the
/// screen, and the 1 GHz device feels every multiplication.
enum Plan {
    /// For each output index: the first source index, the weights of the
    /// source pixels that overlap it (adding up to 256), and where they
    /// start in the flat `weights` table.
    Shrink {
        start: Vec<u32>,
        offset: Vec<u32>,
        weights: Vec<u16>,
    },
    /// For each output index: the source index and the fraction (in 256ths)
    /// towards the next one.
    Grow(Vec<(u32, u16)>),
}

fn plan(s_len: u32, d: u32) -> Plan {
    let scale = s_len as f64 / d as f64;
    if scale > 1.0 {
        let mut start = Vec::with_capacity(d as usize);
        let mut offset = vec![0u32];
        let mut weights: Vec<u16> = Vec::new();
        for i in 0..d {
            let a = i as f64 * scale;
            let b = (i + 1) as f64 * scale;
            let j0 = a.floor() as u32;
            let j1 = (b.ceil() as u32).min(s_len);
            let mut w: Vec<u16> = (j0..j1)
                .map(|j| {
                    let lo = a.max(j as f64);
                    let hi = b.min(j as f64 + 1.0);
                    ((hi - lo).max(0.0) / scale * 256.0).round() as u16
                })
                .collect();
            // The weights must add up to exactly 256: the rounding is put on
            // the last one, which keeps the mean of the row exact.
            let sum: i32 = w.iter().map(|&x| x as i32).sum();
            if let Some(last) = w.last_mut() {
                *last = (*last as i32 + 256 - sum).max(0) as u16;
            }
            start.push(j0);
            weights.extend_from_slice(&w);
            offset.push(weights.len() as u32);
        }
        Plan::Shrink {
            start,
            offset,
            weights,
        }
    } else {
        let mut lerp = Vec::with_capacity(d as usize);
        for i in 0..d {
            let x = ((i as f64 + 0.5) * scale - 0.5).max(0.0);
            let j = (x.floor() as u32).min(s_len - 1);
            let t = ((x - j as f64) * 256.0).round().clamp(0.0, 256.0) as u16;
            lerp.push((j, t));
        }
        Plan::Grow(lerp)
    }
}

/// Resamples one axis of an `sw` × `sh` image to length `d`: the width when
/// `horizontal`, else the height. Integer arithmetic throughout: the
/// weights are 8-bit fixed point.
fn resize_axis(px: &[u8], sw: u32, sh: u32, d: u32, horizontal: bool) -> Vec<u8> {
    let (s_len, lines) = if horizontal { (sw, sh) } else { (sh, sw) };
    let (out_w, out_h) = if horizontal { (d, sh) } else { (sw, d) };
    let mut out = vec![0u8; (out_w * out_h) as usize];
    if d == 0 || s_len == 0 {
        return out;
    }
    // Distance between two samples along the resampled axis, and between two
    // lines, in the source's row-major buffer.
    let (step, line_step) = if horizontal {
        (1usize, sw as usize)
    } else {
        (sw as usize, 1usize)
    };
    let out_step = if horizontal { 1 } else { out_w as usize };
    let plan = plan(s_len, d);
    let last = s_len as usize - 1;
    for line in 0..lines as usize {
        let src_base = line * line_step;
        let out_base = if horizontal { line * d as usize } else { line };
        match &plan {
            Plan::Shrink {
                start,
                offset,
                weights,
            } => {
                for i in 0..d as usize {
                    let w = &weights[offset[i] as usize..offset[i + 1] as usize];
                    let s = src_base + start[i] as usize * step;
                    let mut acc = 0u32;
                    for (k, &wt) in w.iter().enumerate() {
                        acc += px[s + k * step] as u32 * wt as u32;
                    }
                    out[out_base + i * out_step] = ((acc + 128) >> 8) as u8;
                }
            }
            Plan::Grow(lerp) => {
                for i in 0..d as usize {
                    let (j, t) = lerp[i];
                    let a = px[src_base + (j as usize).min(last) * step] as u32;
                    let b = px[src_base + (j as usize + 1).min(last) * step] as u32;
                    let t = t as u32;
                    out[out_base + i * out_step] = ((a * (256 - t) + b * t + 128) >> 8) as u8;
                }
            }
        }
    }
    out
}

/// Serpentine Floyd–Steinberg error diffusion to 16 evenly spaced levels
/// (multiples of 17), the values the GL16/GC16 waveforms reproduce exactly.
pub fn dither16(img: &mut Gray) {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut cur = vec![0i32; w + 2];
    let mut next = vec![0i32; w + 2];
    for y in 0..h {
        let ltr = y % 2 == 0;
        for step in 0..w {
            let x = if ltr { step } else { w - 1 - step };
            let i = y * w + x;
            let old = (img.pixels[i] as i32 * 16 + cur[x + 1]).clamp(0, 255 * 16);
            let q = ((old + 8 * 17) / (17 * 16)).min(15) * 17;
            img.pixels[i] = q as u8;
            let err = old - q * 16;
            // Values and errors are kept in 1/16 gray steps, so the 7/3/5/1
            // sixteenths lose little to integer rounding.
            let (fwd, back) = if ltr { (x + 2, x) } else { (x, x + 2) };
            cur[fwd] += err * 7 / 16;
            next[back] += err * 3 / 16;
            next[x + 1] += err * 5 / 16;
            next[fwd] += err / 16;
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|e| *e = 0);
    }
}

/// Full pipeline from cover bytes to a dithered thumbnail.
pub fn make_thumbnail(data: &[u8], spec: ThumbSpec) -> Result<Gray> {
    let src = decode_gray(data, spec.width)?;
    let h = ((spec.width as u64 * src.height as u64 + src.width as u64 / 2) / src.width as u64)
        .clamp(1, spec.max_height as u64) as u32;
    let mut t = resize(&src, spec.width, h);
    dither16(&mut t);
    Ok(t)
}

pub fn write_pgm(path: &Path, img: &Gray) -> Result<()> {
    let mut data = format!("P5\n{} {}\n255\n", img.width, img.height).into_bytes();
    data.extend_from_slice(&img.pixels);
    crate::index::write_atomic(path, &data)
}

pub fn read_pgm(path: &Path) -> Result<Gray> {
    let data = fs::read(path)?;
    let mut fields = Vec::new();
    let mut p = 0;
    while fields.len() < 4 {
        while p < data.len() && data[p].is_ascii_whitespace() {
            p += 1;
        }
        let s = p;
        while p < data.len() && !data[p].is_ascii_whitespace() {
            p += 1;
        }
        if s == p {
            return Err(Error::format("pgm: truncated header"));
        }
        fields.push(String::from_utf8_lossy(&data[s..p]).into_owned());
    }
    p += 1;
    let num = |i: usize| {
        fields[i]
            .parse::<u32>()
            .map_err(|_| Error::format("pgm: bad header"))
    };
    if fields[0] != "P5" || num(3)? != 255 {
        return Err(Error::format("pgm: not an 8-bit P5 file"));
    }
    let (width, height) = (num(1)?, num(2)?);
    let pixels = data
        .get(p..p + (width * height) as usize)
        .ok_or_else(|| Error::format("pgm: truncated"))?
        .to_vec();
    Ok(Gray {
        width,
        height,
        pixels,
    })
}

/// The on-disk thumbnail cache.
pub struct CoverCache {
    dir: PathBuf,
    spec: ThumbSpec,
}

impl CoverCache {
    pub fn new(dir: PathBuf, spec: ThumbSpec) -> CoverCache {
        CoverCache { dir, spec }
    }

    pub fn spec(&self) -> ThumbSpec {
        self.spec
    }

    /// The content key of a cover image: the MD5 of its bytes. One source
    /// image has one key however many thumbnail sizes are cached for it.
    pub fn key(cover: &[u8]) -> String {
        crate::id::md5_hex(cover)
    }

    /// Where the thumbnail of `key` at `spec` is cached. The size is part of
    /// the file name, so changing the geometry never serves a stale file.
    pub fn path_for(&self, key: &str, spec: ThumbSpec) -> PathBuf {
        self.dir
            .join(format!("{key}-{}x{}.pgm", spec.width, spec.max_height))
    }

    /// The thumbnail of `key` at this cache's size.
    pub fn path(&self, key: &str) -> PathBuf {
        self.path_for(key, self.spec)
    }

    /// Returns the key, creating the thumbnail at this cache's size unless it
    /// is already cached.
    pub fn store(&self, cover: &[u8]) -> Result<String> {
        let key = Self::key(cover);
        self.store_keyed(&key, cover, self.spec)?;
        Ok(key)
    }

    /// Returns the key, creating the thumbnail at `spec` (which may differ
    /// from the cache's own size) unless it is already cached.
    pub fn store_with(&self, cover: &[u8], spec: ThumbSpec) -> Result<String> {
        let key = Self::key(cover);
        self.store_keyed(&key, cover, spec)?;
        Ok(key)
    }

    /// Writes the thumbnail of an already hashed cover at `spec`.
    pub fn store_keyed(&self, key: &str, cover: &[u8], spec: ThumbSpec) -> Result<()> {
        let path = self.path_for(key, spec);
        if !path.exists() {
            let thumb = make_thumbnail(cover, spec)?;
            fs::create_dir_all(&self.dir)?;
            write_pgm(&path, &thumb)?;
        }
        Ok(())
    }
}

/// The content key of a stored thumbnail, without its size. Covers written
/// by older versions kept the geometry in `BookRecord::cover`
/// ("<md5>-200x320"); anything else is returned as it is.
pub fn base_key(stored: &str) -> &str {
    let head = stored.get(..32);
    match head {
        Some(h) if h.bytes().all(|b| b.is_ascii_hexdigit()) && stored[32..].starts_with('-') => h,
        _ => stored,
    }
}

/// Writes `img` as PNG (8-bit gray) for viewing on the host.
pub fn write_png(path: &Path, img: &Gray) -> Result<()> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc
            .write_header()
            .map_err(|e| Error::format(format!("png: {e}")))?;
        w.write_image_data(&img.pixels)
            .map_err(|e| Error::format(format!("png: {e}")))?;
    }
    let mut f = fs::File::create(path)?;
    f.write_all(&out)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod testutil {
    /// A gradient PNG, RGB, `w` × `h`.
    pub fn gradient_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            let mut data = Vec::new();
            for _y in 0..h {
                for x in 0..w {
                    let v = (x * 255 / (w - 1).max(1)) as u8;
                    data.extend_from_slice(&[v, v, v]);
                }
            }
            wr.write_image_data(&data).unwrap();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_has_16_levels_and_keeps_tone() {
        let png = testutil::gradient_png(600, 900);
        let t = make_thumbnail(&png, ThumbSpec::default()).unwrap();
        assert_eq!((t.width, t.height), (200, 300));
        assert!(t.pixels.iter().all(|&v| v % 17 == 0));
        // Dithering preserves mean brightness of each column band.
        let mean = |x0: u32, x1: u32| {
            let mut s = 0u64;
            let mut n = 0u64;
            for y in 0..t.height {
                for x in x0..x1 {
                    s += t.pixels[(y * t.width + x) as usize] as u64;
                    n += 1;
                }
            }
            s as f64 / n as f64
        };
        let expected = |x0: u32, x1: u32| (x0 + x1) as f64 / 2.0 / 199.0 * 255.0;
        for (a, b) in [(20, 40), (90, 110), (160, 180)] {
            assert!(
                (mean(a, b) - expected(a, b)).abs() < 4.0,
                "band {a}-{b}: {} vs {}",
                mean(a, b),
                expected(a, b)
            );
        }
        // A gradient must use many levels, not posterise to a few.
        let mut levels: Vec<u8> = t.pixels.clone();
        levels.sort();
        levels.dedup();
        assert!(levels.len() >= 14);
    }

    #[test]
    fn pgm_roundtrip_and_cache() {
        let dir = crate::testdir("cover");
        let cache = CoverCache::new(
            dir.join("covers"),
            ThumbSpec {
                width: 50,
                max_height: 60,
            },
        );
        let png = testutil::gradient_png(100, 400);
        let key = cache.store(&png).unwrap();
        let g = read_pgm(&cache.path(&key)).unwrap();
        assert_eq!((g.width, g.height), (50, 60));
        assert_eq!(cache.store(&png).unwrap(), key);
    }

    #[test]
    fn upscales_small_covers() {
        let png = testutil::gradient_png(40, 60);
        let t = make_thumbnail(&png, ThumbSpec::default()).unwrap();
        assert_eq!((t.width, t.height), (200, 300));
    }

    #[test]
    fn one_image_is_cached_at_several_sizes() {
        let dir = crate::testdir("cover-sizes");
        let cache = CoverCache::new(dir.join("covers"), ThumbSpec::default());
        let png = testutil::gradient_png(600, 900);
        let key = cache.store(&png).unwrap();
        assert_eq!(
            CoverCache::key(&png),
            key,
            "the key is the image's MD5 alone"
        );
        let large = ThumbSpec::large();
        cache.store_with(&png, large).unwrap();
        let shelf = read_pgm(&cache.path(&key)).unwrap();
        let hero = read_pgm(&cache.path_for(&key, large)).unwrap();
        assert_eq!((shelf.width, shelf.height), (200, 300));
        assert_eq!((hero.width, hero.height), (560, 840));
        assert!(hero.pixels.iter().all(|&v| v % 17 == 0));
        // Storing again writes nothing: both sizes are there.
        assert_eq!(cache.store_with(&png, large).unwrap(), key);
    }

    #[test]
    fn a_damaged_image_has_no_thumbnail() {
        let dir = crate::testdir("cover-broken");
        let cache = CoverCache::new(dir.join("covers"), ThumbSpec::default());
        // A PNG header followed by rubbish, and plain text that only looks
        // like neither: both must fail cleanly, not panic.
        let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
        broken.extend_from_slice(&[0u8; 64]);
        assert!(cache.store(&broken).is_err());
        assert!(cache.store(b"not an image at all").is_err());
        assert!(
            dir.join("covers")
                .read_dir()
                .map(|d| d.count())
                .unwrap_or(0)
                == 0
        );
    }

    #[test]
    fn an_oversized_image_is_refused_before_it_is_decoded() {
        // 8000 × 5001 is just over MAX_PIXELS: the guard must refuse it
        // without allocating a 120 MB luminance buffer, so only a header
        // with those dimensions is needed.
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend_from_slice(&8000u32.to_be_bytes());
        ihdr.extend_from_slice(&5001u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
        png.extend_from_slice(&(13u32).to_be_bytes());
        png.extend_from_slice(&ihdr);
        png.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&crc32(b"IDAT").to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&crc32(b"IEND").to_be_bytes());
        let err = decode_gray(&png, 200).unwrap_err().to_string();
        assert!(err.contains("too large"), "{err}");
    }

    /// CRC-32 as PNG defines it, for the hand-made header above.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn legacy_cover_keys_lose_their_size() {
        assert_eq!(
            base_key("0123456789abcdef0123456789abcdef-200x320"),
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            base_key("0123456789abcdef0123456789abcdef"),
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(base_key("anything"), "anything");
    }
}
