// SPDX-License-Identifier: AGPL-3.0-or-later
//! Full-screen 8-bit page buffers, and the header and footer drawn on them.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use plato_core::anyhow::{format_err, Error};
use plato_core::color::Color;
use plato_core::font::{Font, FontOpener};
use plato_core::framebuffer::{Framebuffer, Pixmap, UpdateMode};
use plato_core::geom::{Point, Rectangle};

use crate::book::Bounds;
use crate::style::Style;

/// Gray level below which a pixel becomes black in the black and white
/// frame of a page turn. Same value as the page-turn experiment
/// (`aristotle-observe bench pageturn`), where it was legible on the panel.
pub const BW_THRESHOLD: u8 = 160;

/// A full-screen page: `height` rows of `stride` bytes, the first `width`
/// of each visible. 0 is black, 255 white, like the PW4 framebuffer.
#[derive(Clone)]
pub struct Page {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: Vec<u8>,
    /// Where the book's images are, so night mode can leave them as they
    /// are (see [`Page::invert_images`]).
    pub images: Vec<Bounds>,
}

impl Page {
    /// A white page with the style's geometry.
    pub fn white(style: &Style) -> Page {
        Page {
            width: style.width,
            height: style.height,
            stride: style.stride,
            data: vec![0xff; style.stride * style.height as usize],
            images: Vec::new(),
        }
    }

    /// Invert the pixels of the images. Night mode inverts the whole page
    /// on its way to the panel; a page with its images inverted first shows
    /// them as they are, rather than as photographic negatives. Inverting
    /// twice gives the page back.
    pub fn invert_images(&mut self) {
        let (w, h) = (self.width as i32, self.height as i32);
        for r in &self.images {
            let (x0, x1) = (r.x0.clamp(0, w) as usize, r.x1.clamp(0, w) as usize);
            for y in r.y0.clamp(0, h)..r.y1.clamp(0, h) {
                let row = y as usize * self.stride;
                for p in &mut self.data[row + x0..row + x1] {
                    *p = 255 - *p;
                }
            }
        }
    }

    /// Copy a grayscale pixmap (1 sample per pixel) with its top-left
    /// corner at (`x`, `y`), clipped to the page. One memcpy per row.
    pub fn blit_pixmap(&mut self, pixmap: &Pixmap, x: i32, y: i32) {
        debug_assert_eq!(pixmap.samples, 1);
        let src_w = pixmap.width as i32;
        let x0 = x.max(0);
        let x1 = (x + src_w).min(self.width as i32);
        if x0 >= x1 {
            return;
        }
        for row in 0..pixmap.height as i32 {
            let dy = y + row;
            if dy < 0 || dy >= self.height as i32 {
                continue;
            }
            let src = (row * src_w + (x0 - x)) as usize;
            let dst = dy as usize * self.stride + x0 as usize;
            let len = (x1 - x0) as usize;
            self.data[dst..dst + len].copy_from_slice(&pixmap.data[src..src + len]);
        }
    }

    /// Copy a grayscale buffer (one byte per pixel, no padding) with its
    /// top-left corner at (`x`, `y`), clipped to the page. One memcpy per
    /// row. This is what crengine-ng's pages come in as.
    #[cfg(feature = "crengine")]
    pub fn blit_gray_buffer(&mut self, buffer: &inkread_platform::GrayBuffer, x: i32, y: i32) {
        let src_w = buffer.width() as i32;
        let src_h = buffer.height() as i32;
        let x0 = x.max(0);
        let x1 = (x + src_w).min(self.width as i32);
        if x0 >= x1 {
            return;
        }
        let pixels = buffer.pixels();
        for row in 0..src_h {
            let dy = y + row;
            if dy < 0 || dy >= self.height as i32 {
                continue;
            }
            let src = (row * src_w + (x0 - x)) as usize;
            let dst = dy as usize * self.stride + x0 as usize;
            let len = (x1 - x0) as usize;
            self.data[dst..dst + len].copy_from_slice(&pixels[src..src + len]);
        }
    }

    /// The black and white version shown first on a page turn: A2 can only
    /// drive pure black and white, and anything in between would be
    /// rounded unpredictably by the waveform.
    pub fn threshold(&self) -> Page {
        Page {
            data: self
                .data
                .iter()
                .map(|&p| if p < BW_THRESHOLD { 0x00 } else { 0xff })
                .collect(),
            images: self.images.clone(),
            ..*self
        }
    }

    /// Write the raw buffer (stride included), readable by
    /// `tools/host/fb2png.py` like a framebuffer dump.
    pub fn write_raw(&self, path: &Path) -> io::Result<()> {
        File::create(path)?.write_all(&self.data)
    }
}

// Plato's fonts draw into anything that implements its `Framebuffer` trait.
// Only the pixel setters matter here; the display methods are never called
// on an off-screen page.
impl Framebuffer for Page {
    fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.width && y < self.height {
            self.data[y as usize * self.stride + x as usize] = color.gray();
        }
    }

    fn set_blended_pixel(&mut self, x: u32, y: u32, color: Color, alpha: f32) {
        if x < self.width && y < self.height {
            let p = &mut self.data[y as usize * self.stride + x as usize];
            let (a, b) = (*p as f32, color.gray() as f32);
            *p = (a + (b - a) * alpha.clamp(0.0, 1.0)) as u8;
        }
    }

    fn invert_region(&mut self, _rect: &Rectangle) {}
    fn shift_region(&mut self, _rect: &Rectangle, _drift: u8) {}
    fn update(&mut self, _rect: &Rectangle, _mode: UpdateMode) -> Result<u32, Error> {
        Err(format_err!("an off-screen page has no display"))
    }
    fn wait(&self, _token: u32) -> Result<i32, Error> {
        Err(format_err!("an off-screen page has no display"))
    }
    fn save(&self, path: &str) -> Result<(), Error> {
        Ok(self.write_raw(Path::new(path))?)
    }
    fn set_rotation(&mut self, _n: i8) -> Result<(u32, u32), Error> {
        Err(format_err!("an off-screen page has no rotation"))
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

/// What the header and footer say about the current page.
#[derive(Clone, Debug, Default)]
pub struct Folio {
    /// Running head: the book's author and title.
    pub header: String,
    /// Chapter title, at the left of the footer's first row.
    pub chapter: String,
    /// Time left in the chapter, at the right of the first row. `None`
    /// when the reader cannot estimate it; the item is then left out.
    pub remaining: Option<String>,
    /// 1-based page number, when known (a reader that opened a book in the
    /// middle knows it only once the book is paginated).
    pub page: Option<usize>,
    /// Total pages, when the book has been paginated.
    pub pages: Option<usize>,
    /// Position in the book, 0.0 to 1.0.
    pub progress: f32,
    /// Whether the book is in degraded mode (suppresses inaccurate footer).
    pub degraded: bool,
}

/// The slim header and footer ("chrome") drawn by Aristotle itself rather
/// than by the book: chapter title on top, page number and progress below.
/// They are drawn in the same frame as the text, so a page turn needs no
/// separate footer update (the stock reader sends one, see
/// `docs/benchmarks.md`).
pub struct Chrome {
    regular: Font,
    size: u32,
    dpi: u16,
    gray: u8,
    margin: i32,
    header_baseline: i32,
    footer_top: i32,
    footer_h: i32,
    x_height: i32,
}

impl Chrome {
    /// Load the chrome font from `serif/Literata-Regular.ttf` (relative to
    /// the data directory; [`crate::style::CHROME_FAMILY`]), whatever the
    /// body font.
    pub fn load(style: &Style) -> Result<Chrome, Error> {
        use crate::style::{CHROME_DIR, CHROME_FAMILY};
        let opener = FontOpener::new()?;
        let path = |face: &str| format!("{CHROME_DIR}/{CHROME_FAMILY}-{face}.ttf");
        let mut regular = opener.open(path("Regular"))?;
        let size = style.chrome_size_26_6();
        regular.set_size(size, style.dpi);
        // Centre the running head in the band above the text block, with
        // its x-height optically centred.
        let x_height = regular.x_heights.0 as i32;
        let header_baseline = (style.text_top as i32 + x_height) / 2;
        let footer_top = style.height as i32 - style.text_bottom as i32;
        let footer_h = style.text_bottom as i32;
        Ok(Chrome {
            regular,
            size,
            dpi: style.dpi,
            gray: style.chrome_gray,
            margin: style.margin_px(),
            header_baseline,
            footer_top,
            footer_h,
            x_height,
        })
    }

    /// Draw the header and footer for `folio` onto `page`.
    pub fn draw(&mut self, page: &mut Page, folio: &Folio) {
        use aristotle_screen::Density;
        let color = Color::Gray(self.gray);
        let width = page.width as i32;
        let max_width = width - 2 * self.margin;

        self.regular.set_size(self.size, self.dpi);
        let mut plan = self.regular.plan(&folio.header, None, None);
        self.regular.crop_right(&mut plan, max_width);
        self.regular.render(
            page,
            color,
            &plan,
            Point::new(self.margin, self.header_baseline),
        );

        if folio.degraded {
            return;
        }

        let density = Density::new(self.dpi).unwrap_or_default();
        let gap = density.px(24);
        let thickness = density.px(6).max(2);
        let radius = thickness / 2;
        let row1 = self.footer_top + self.footer_h * 38 / 100 + self.x_height / 2;
        let row2 = self.footer_top + self.footer_h - density.px(12);

        // First row: chapter on the left, time left on the right.
        let mut plan = self.regular.plan(&folio.chapter, None, None);
        self.regular.crop_right(&mut plan, max_width / 2 - gap);
        self.regular
            .render(page, color, &plan, Point::new(self.margin, row1));
        if let Some(remaining) = folio.remaining.as_deref().filter(|s| !s.is_empty()) {
            let plan = self.regular.plan(remaining, None, None);
            let x = width - self.margin - plan.width;
            self.regular.render(page, color, &plan, Point::new(x, row1));
        }

        // Second row: fine progress bar, percentage, page numbers.
        let pct = format!(
            "{}%",
            (folio.progress.clamp(0.0, 1.0) * 100.0).floor() as u32
        );
        let number = match (folio.page, folio.pages) {
            (Some(page), Some(total)) => format!("{page} / {total}"),
            (Some(page), None) => page.to_string(),
            (None, _) => String::new(),
        };
        let pct_plan = self.regular.plan(&pct, None, None);
        let number_plan = self.regular.plan(&number, None, None);
        let pct_x = self.margin + (max_width - pct_plan.width - number_plan.width - 2 * gap).max(0);
        let number_x = width - self.margin - number_plan.width;
        let bar_w = (pct_x - gap - self.margin).max(density.px(24));
        let bar_y = row2 - self.x_height / 2 - thickness / 2;
        fill_round_rect(page, self.margin, bar_y, bar_w, thickness, radius, 0xf2);
        let filled = (bar_w as f32 * folio.progress.clamp(0.0, 1.0)).round() as i32;
        if filled > 0 {
            fill_round_rect(page, self.margin, bar_y, filled, thickness, radius, 0);
        }
        self.regular
            .render(page, color, &pct_plan, Point::new(pct_x, row2));
        if !number.is_empty() {
            self.regular
                .render(page, color, &number_plan, Point::new(number_x, row2));
        }
    }
}

/// Fill a rectangle with rounded left and right ends, one row at a time.
fn fill_round_rect(page: &mut Page, x: i32, y: i32, w: i32, h: i32, radius: i32, gray: u8) {
    if w <= 0 || h <= 0 || page.width == 0 {
        return;
    }
    let radius = radius.clamp(0, h / 2).min(w / 2);
    let (x0, x1) = (x.max(0), (x + w).min(page.width as i32));
    for row in 0..h {
        let dy = if row < radius {
            radius - row - 1
        } else if row >= h - radius {
            row - (h - radius)
        } else {
            0
        };
        let inset = if dy > 0 {
            let r = radius as f32;
            (r - (r * r - (dy as f32) * (dy as f32)).max(0.0).sqrt()).round() as i32
        } else {
            0
        };
        let (a, b) = ((x + inset).max(x0), (x + w - inset).min(x1));
        if a >= b {
            continue;
        }
        let yy = y + row;
        if yy < 0 || yy >= page.height as i32 {
            continue;
        }
        let start = yy as usize * page.stride + a as usize;
        page.data[start..start + (b - a) as usize].fill(gray);
    }
}
