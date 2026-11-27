// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fixed-layout / comic (CBZ, PDF) layout helpers: viewport geometry, scaling,
//! automatic white-margin cropping, and landscape two-page spread splitting.

use plato_core::framebuffer::Pixmap;

use crate::book::Bounds;
use crate::style::{FitMode, PageDirection, Style};

/// Minimum width/height aspect ratio for a fixed-layout page to be treated as
/// a two-page landscape spread when `split_spread` is enabled.
pub const SPREAD_ASPECT_THRESHOLD: f32 = 1.15;

/// Pixels darker than this gray level (0 = black, 255 = white) count as page
/// content when detecting white margins for auto-crop.
const CROP_INK_THRESHOLD: u8 = 245;

/// Maximum rasterised pixel count for a single MuPDF render call (16 MP),
/// guarding memory usage on 512 MB Kindle devices when scaling up a cropped
/// or split region.
const MAX_RASTER_PIXELS: f32 = 4096.0 * 4096.0;

/// Which horizontal half of a physical page a virtual slice covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpreadHalf {
    /// The entire physical page.
    Full,
    /// Left half (`0.0..0.5` of the physical width).
    Left,
    /// Right half (`0.5..1.0` of the physical width).
    Right,
}

/// One virtual page of a fixed-layout book when spread splitting is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedSlice {
    /// 0-based physical page index in the underlying MuPDF document.
    pub doc_page: usize,
    /// Which horizontal half of `doc_page` this slice displays.
    pub half: SpreadHalf,
}

/// Build the virtual slice table for a fixed-layout document given each page's
/// `(width, height)` dimensions.
///
/// Pages with `width > height * SPREAD_ASPECT_THRESHOLD` split into two halves
/// ordered by `direction` (`Rtl`: right then left; `Ltr`: left then right).
pub fn build_slices<I>(dims: I, direction: PageDirection) -> Vec<FixedSlice>
where
    I: IntoIterator<Item = (usize, Option<(f32, f32)>)>,
{
    let mut slices = Vec::new();
    for (doc_page, dim) in dims {
        let is_spread = dim.is_some_and(|(w, h)| h > 0.0 && w > h * SPREAD_ASPECT_THRESHOLD);
        if is_spread {
            match direction {
                PageDirection::Rtl => {
                    slices.push(FixedSlice {
                        doc_page,
                        half: SpreadHalf::Right,
                    });
                    slices.push(FixedSlice {
                        doc_page,
                        half: SpreadHalf::Left,
                    });
                }
                PageDirection::Ltr => {
                    slices.push(FixedSlice {
                        doc_page,
                        half: SpreadHalf::Left,
                    });
                    slices.push(FixedSlice {
                        doc_page,
                        half: SpreadHalf::Right,
                    });
                }
            }
        } else {
            slices.push(FixedSlice {
                doc_page,
                half: SpreadHalf::Full,
            });
        }
    }
    slices
}

/// Viewport `(x, y, width, height)` in screen pixels for a fixed-layout page.
///
/// Fullscreen mode uses the entire panel without header, footer or margins.
/// Otherwise the page sits inside the body band below the header and above the
/// footer, preserving the historical vertical margin inset.
pub fn viewport_rect(style: &Style) -> (i32, i32, u32, u32) {
    if style.fixed_layout.fullscreen {
        (0, 0, style.width, style.height)
    } else {
        let (top, body_h) = style.body();
        let m = style.margin_px().max(0);
        let h = (body_h as i32 - 2 * m).max(1) as u32;
        (0, top + m, style.width, h)
    }
}

/// Compute the proportional scale factor to place a content box of
/// `(content_w, content_h)` into a viewport of `(vp_w, vp_h)` under `fit`,
/// capped so the full physical page `(doc_w, doc_h)` does not exceed
/// [`MAX_RASTER_PIXELS`].
pub fn compute_scale(
    content_w: f32,
    content_h: f32,
    doc_w: f32,
    doc_h: f32,
    vp_w: f32,
    vp_h: f32,
    fit: FitMode,
) -> f32 {
    if content_w <= 0.0 || content_h <= 0.0 || vp_w <= 0.0 || vp_h <= 0.0 {
        return 1.0;
    }
    let sx = vp_w / content_w;
    let sy = vp_h / content_h;
    let raw = match fit {
        FitMode::FitPage => sx.min(sy),
        FitMode::FitWidth => sx,
        FitMode::FitHeight => sy,
    };
    let doc_area = (doc_w * doc_h).max(1.0);
    let max_scale = (MAX_RASTER_PIXELS / doc_area).sqrt();
    raw.min(max_scale).max(0.05)
}

/// Compute the top-left `(x, y)` screen coordinates to place a rasterised body
/// of `(body_w, body_h)` inside viewport `(vp_x, vp_y, vp_w, vp_h)`.
///
/// - `FitPage`: centred horizontally and vertically.
/// - `FitWidth`: centred horizontally; top-aligned when taller than the
///   viewport so the top of the comic page is shown first, centred otherwise.
/// - `FitHeight`: centred vertically and horizontally.
pub fn place_in_viewport(
    body_w: i32,
    body_h: i32,
    vp_x: i32,
    vp_y: i32,
    vp_w: i32,
    vp_h: i32,
    fit: FitMode,
) -> (i32, i32) {
    let x = vp_x + (vp_w - body_w) / 2;
    let y = match fit {
        FitMode::FitWidth if body_h > vp_h => vp_y,
        _ => vp_y + (vp_h - body_h) / 2,
    };
    (x, y)
}

/// Horizontal sub-slice bounds `(x0..x1, 0..h)` of `pixmap` for `half`.
pub fn half_bounds(w: u32, h: u32, half: SpreadHalf) -> Bounds {
    let w = w as i32;
    let h = h as i32;
    match half {
        SpreadHalf::Full => Bounds {
            x0: 0,
            y0: 0,
            x1: w,
            y1: h,
        },
        SpreadHalf::Left => Bounds {
            x0: 0,
            y0: 0,
            x1: (w / 2).max(1).min(w),
            y1: h,
        },
        SpreadHalf::Right => Bounds {
            x0: (w / 2).min(w.saturating_sub(1)),
            y0: 0,
            x1: w,
            y1: h,
        },
    }
}

/// Detect the bounding box of non-white content inside an 8-bit grayscale
/// `Pixmap` (`samples == 1`), returning `None` if the page is blank.
///
/// Requires at least 2 dark pixels per row/column (falling back to 1 on very
/// sparse pages) to resist single-pixel scanner specks, then expands by a
/// small 2% safety margin so panel borders are never clipped.
pub fn detect_content_bounds(pixmap: &Pixmap) -> Option<Bounds> {
    if pixmap.samples != 1 || pixmap.width == 0 || pixmap.height == 0 {
        return None;
    }
    let w = pixmap.width as usize;
    let h = pixmap.height as usize;
    if pixmap.data.len() < w * h {
        return None;
    }
    let mut row_counts = vec![0u32; h];
    let mut col_counts = vec![0u32; w];
    let mut total_dark = 0u64;

    for (y, row_count) in row_counts.iter_mut().enumerate() {
        let row = &pixmap.data[y * w..(y + 1) * w];
        let mut rc = 0u32;
        for (x, &px) in row.iter().enumerate() {
            if px < CROP_INK_THRESHOLD {
                rc += 1;
                col_counts[x] += 1;
            }
        }
        *row_count = rc;
        total_dark += rc as u64;
    }

    if total_dark == 0 {
        return None;
    }

    // Require >= 2 dark pixels per line on normal pages to ignore isolated specks.
    let min_line_hits: u32 = if total_dark >= 16 { 2 } else { 1 };
    let find_range = |counts: &[u32], thresh: u32| -> Option<(usize, usize)> {
        let first = counts.iter().position(|&c| c >= thresh)?;
        let last = counts.iter().rposition(|&c| c >= thresh)?;
        Some((first, last))
    };

    let (y0, y1) = find_range(&row_counts, min_line_hits).or_else(|| find_range(&row_counts, 1))?;
    let (x0, x1) = find_range(&col_counts, min_line_hits).or_else(|| find_range(&col_counts, 1))?;

    let pad_x = ((w as f32 * 0.02).round() as usize).max(2);
    let pad_y = ((h as f32 * 0.02).round() as usize).max(2);

    let bx0 = x0.saturating_sub(pad_x) as i32;
    let by0 = y0.saturating_sub(pad_y) as i32;
    let bx1 = (x1 + 1 + pad_x).min(w) as i32;
    let by1 = (y1 + 1 + pad_y).min(h) as i32;

    (bx0 < bx1 && by0 < by1).then_some(Bounds {
        x0: bx0,
        y0: by0,
        x1: bx1,
        y1: by1,
    })
}

/// Extract the sub-rectangle `bounds` from an 8-bit grayscale `Pixmap`.
pub fn crop_pixmap(pixmap: &Pixmap, bounds: Bounds) -> Pixmap {
    debug_assert_eq!(pixmap.samples, 1);
    let w = pixmap.width as i32;
    let h = pixmap.height as i32;
    let x0 = bounds.x0.clamp(0, w) as usize;
    let y0 = bounds.y0.clamp(0, h) as usize;
    let x1 = bounds.x1.clamp(x0 as i32, w) as usize;
    let y1 = bounds.y1.clamp(y0 as i32, h) as usize;
    let cw = x1 - x0;
    let ch = y1 - y0;
    let mut data = Vec::with_capacity(cw * ch);
    let src_w = pixmap.width as usize;
    for y in y0..y1 {
        let start = y * src_w + x0;
        data.extend_from_slice(&pixmap.data[start..start + cw]);
    }
    Pixmap {
        width: cw as u32,
        height: ch as u32,
        samples: 1,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_pixmap(w: u32, h: u32, fill: u8) -> Pixmap {
        Pixmap {
            width: w,
            height: h,
            samples: 1,
            data: vec![fill; (w * h) as usize],
        }
    }

    fn paint_rect(pm: &mut Pixmap, x0: u32, y0: u32, x1: u32, y1: u32, gray: u8) {
        for y in y0..y1.min(pm.height) {
            for x in x0..x1.min(pm.width) {
                pm.data[(y * pm.width + x) as usize] = gray;
            }
        }
    }

    #[test]
    fn build_slices_splits_landscape_pages_by_direction() {
        let dims = [
            (0, Some((600.0, 900.0))),  // portrait
            (1, Some((1400.0, 900.0))), // landscape spread (1.55)
            (2, Some((700.0, 700.0))),  // square (1.0 < 1.15)
        ];
        let rtl = build_slices(dims, PageDirection::Rtl);
        assert_eq!(
            rtl,
            vec![
                FixedSlice {
                    doc_page: 0,
                    half: SpreadHalf::Full
                },
                FixedSlice {
                    doc_page: 1,
                    half: SpreadHalf::Right
                },
                FixedSlice {
                    doc_page: 1,
                    half: SpreadHalf::Left
                },
                FixedSlice {
                    doc_page: 2,
                    half: SpreadHalf::Full
                },
            ]
        );
        let ltr = build_slices(dims, PageDirection::Ltr);
        assert_eq!(ltr[1].half, SpreadHalf::Left);
        assert_eq!(ltr[2].half, SpreadHalf::Right);
    }

    #[test]
    fn detect_content_bounds_trims_white_margins_and_ignores_speck() {
        let mut pm = gray_pixmap(200, 300, 255);
        // Main artwork box: x in 40..160, y in 50..250.
        paint_rect(&mut pm, 40, 50, 160, 250, 30);
        // Single-pixel scanner speck near top-left corner should be ignored.
        pm.data[2 * 200 + 3] = 0;

        let b = detect_content_bounds(&pm).expect("content found");
        // 2% of 200 = 4 px, 2% of 300 = 6 px.
        assert_eq!(b.x0, 36);
        assert_eq!(b.x1, 164);
        assert_eq!(b.y0, 44);
        assert_eq!(b.y1, 256);
    }

    #[test]
    fn detect_content_bounds_returns_none_for_blank_page() {
        let pm = gray_pixmap(100, 100, 255);
        assert_eq!(detect_content_bounds(&pm), None);
    }

    #[test]
    fn crop_pixmap_extracts_exact_subregion() {
        let mut pm = gray_pixmap(10, 10, 255);
        paint_rect(&mut pm, 2, 3, 6, 8, 42);
        let cropped = crop_pixmap(
            &pm,
            Bounds {
                x0: 2,
                y0: 3,
                x1: 6,
                y1: 8,
            },
        );
        assert_eq!((cropped.width, cropped.height), (4, 5));
        assert!(cropped.data.iter().all(|&p| p == 42));
    }

    #[test]
    fn viewport_and_fit_modes_behave_as_expected() {
        let mut style = Style::pw4();
        let (_, y_norm, w_norm, h_norm) = viewport_rect(&style);
        assert_eq!(w_norm, style.width);
        assert!(y_norm > 0);
        assert!(h_norm < style.height);

        style.fixed_layout.fullscreen = true;
        assert_eq!(viewport_rect(&style), (0, 0, style.width, style.height));

        let s_page = compute_scale(
            800.0,
            1200.0,
            800.0,
            1200.0,
            1000.0,
            1000.0,
            FitMode::FitPage,
        );
        let s_w = compute_scale(
            800.0,
            1200.0,
            800.0,
            1200.0,
            1000.0,
            1000.0,
            FitMode::FitWidth,
        );
        let s_h = compute_scale(
            800.0,
            1200.0,
            800.0,
            1200.0,
            1000.0,
            1000.0,
            FitMode::FitHeight,
        );
        assert!((s_page - 1000.0 / 1200.0).abs() < 1e-4);
        assert!((s_w - 1000.0 / 800.0).abs() < 1e-4);
        assert!((s_h - 1000.0 / 1200.0).abs() < 1e-4);

        // FitWidth taller than viewport aligns to top.
        assert_eq!(
            place_in_viewport(1000, 1500, 0, 100, 1000, 1000, FitMode::FitWidth),
            (0, 100)
        );
        // FitPage centres both axes.
        assert_eq!(
            place_in_viewport(600, 800, 0, 100, 1000, 1000, FitMode::FitPage),
            (200, 200)
        );
    }
}
