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

/// Live device status displayed in configurable reader header/footer slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceBarStatus {
    /// Formatted wall-clock time, e.g. `"21:38"` or `"9:38 PM"`.
    pub clock: String,
    /// Battery charge percentage `0..=100`, when known.
    pub battery_pct: Option<u8>,
    /// When `true`, hide the numeric percentage text and draw only the battery
    /// icon (respecting the "Show battery percentage" device setting).
    pub hide_battery_pct: bool,
    /// Whether the battery is currently charging or on a charger.
    pub charging: bool,
    /// Current Wi-Fi / airplane radio badge.
    pub wifi: aristotle_screen::WifiBadge,
    /// Current frontlight `(step, max_steps)`, e.g. `(12, 24)`.
    pub frontlight: Option<(u32, u32)>,
    /// Optional warm-light `(step, max_steps)` on dual-tone devices.
    pub warmth: Option<(u32, u32)>,
    /// Localized label for frontlight off (e.g. `"关"` / `"Off"`).
    pub light_off_label: String,
    /// Localized label for Wi-Fi off / disconnected.
    pub wifi_off_label: String,
    /// Localized label for airplane mode.
    pub wifi_airplane_label: String,
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
    /// Time left in the whole book, when estimated.
    pub book_remaining: Option<String>,
    /// 1-based page number, when known (a reader that opened a book in the
    /// middle knows it only once the book is paginated).
    pub page: Option<usize>,
    /// Total pages, when the book has been paginated.
    pub pages: Option<usize>,
    /// 1-based `(page_in_chapter, pages_in_chapter)`, when known.
    pub chapter_page: Option<(usize, usize)>,
    /// Position in the book, 0.0 to 1.0.
    pub progress: f32,
    /// Whether the book is in degraded mode (suppresses inaccurate footer).
    pub degraded: bool,
}

/// The slim header and footer ("chrome") drawn by Aristotle itself rather
/// than by the book: configurable top/bottom slots, progress bar and live
/// device status. They are drawn in the same frame as the text, so a page
/// turn needs no separate footer update (the stock reader sends one, see
/// `docs/benchmarks.md`).
pub struct Chrome {
    regular: Font,
    size: u32,
    dpi: u16,
    gray: u8,
    margin: i32,
    text_top: i32,
    header_baseline: i32,
    footer_top: i32,
    footer_h: i32,
    x_height: i32,
    config: aristotle_screen::ReaderBarConfig,
    status: DeviceBarStatus,
    extra_book_remaining: Option<String>,
    extra_chapter_page: Option<(usize, usize)>,
    has_footnotes: bool,
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
        let text_top = style.text_top as i32;
        let header_baseline = (text_top + x_height) / 2;
        let footer_top = style.height as i32 - style.text_bottom as i32;
        let footer_h = style.text_bottom as i32;
        Ok(Chrome {
            regular,
            size,
            dpi: style.dpi,
            gray: style.chrome_gray,
            margin: style.margin_px(),
            text_top,
            header_baseline,
            footer_top,
            footer_h,
            x_height,
            config: style.bar_config,
            status: DeviceBarStatus::default(),
            extra_book_remaining: None,
            extra_chapter_page: None,
            has_footnotes: false,
        })
    }

    /// Update the bar configuration used by subsequent draws.
    pub fn set_config(&mut self, config: aristotle_screen::ReaderBarConfig) {
        self.config = config;
    }

    /// Current bar configuration.
    pub fn config(&self) -> aristotle_screen::ReaderBarConfig {
        self.config
    }

    /// Update the live device status (clock, battery, Wi-Fi, frontlight).
    pub fn set_status(&mut self, status: DeviceBarStatus) {
        self.status = status;
    }

    /// Update per-page extra estimates (whole-book remaining time and chapter
    /// page numbers) without changing `Book::render_location_with_degraded`.
    pub fn set_page_extras(
        &mut self,
        book_remaining: Option<String>,
        chapter_page: Option<(usize, usize)>,
    ) {
        self.extra_book_remaining = book_remaining;
        self.extra_chapter_page = chapter_page;
    }

    /// Whether the page being drawn has footnotes (reserves room in the
    /// top-right header band for the magnifying-glass footnote button).
    pub fn set_has_footnotes(&mut self, has_footnotes: bool) {
        self.has_footnotes = has_footnotes;
    }

    /// The DPI this chrome was built for.
    pub fn dpi(&self) -> u16 {
        self.dpi
    }

    /// Clear only the header and footer bands on `page` to white and redraw
    /// the bars with the current configuration and live status. Leaves the
    /// body text untouched so status changes never trigger a reflow.
    pub fn redraw_bars(&mut self, page: &mut Page, folio: &Folio) {
        let top_h = (self.text_top.max(0) as usize).min(page.height as usize);
        for y in 0..top_h {
            let row = y * page.stride;
            let end = row + (page.width as usize).min(page.stride);
            page.data[row..end].fill(0xff);
        }
        let foot_y0 = (self.footer_top.max(0) as usize).min(page.height as usize);
        for y in foot_y0..page.height as usize {
            let row = y * page.stride;
            let end = row + (page.width as usize).min(page.stride);
            page.data[row..end].fill(0xff);
        }
        self.draw(page, folio);
    }

    /// Draw the header and footer for `folio` onto `page`.
    pub fn draw(&mut self, page: &mut Page, folio: &Folio) {
        use aristotle_screen::{BarItem, Density};
        let color = Color::Gray(self.gray);
        let width = page.width as i32;
        let max_width = (width - 2 * self.margin).max(0);
        let density = Density::new(self.dpi).unwrap_or_default();
        let gap = density.px(24);
        let ribbon_reserve = if self.has_footnotes {
            density.mm(10.0) + density.mm(9.0) + density.px(8)
        } else if self.config.show_bookmark_ribbon {
            density.px(56)
        } else {
            0
        };

        self.regular.set_size(self.size, self.dpi);

        // --- Top bar (Header) ---
        if self.config.show_header && self.text_top > 0 {
            let right_edge = width - self.margin.max(ribbon_reserve);
            let left_edge = self.margin;
            self.draw_slot_row(
                page,
                folio,
                self.config.header_slots,
                left_edge,
                right_edge,
                self.header_baseline,
                color,
                gap,
                density,
            );
            if self.config.show_dividers {
                let rule_y = (self.text_top - density.px(8)).max(0);
                fill_round_rect(
                    page,
                    self.margin,
                    rule_y,
                    max_width,
                    density.px(2).max(1),
                    0,
                    0xc0,
                );
            }
        }

        if folio.degraded || !self.config.show_footer || self.footer_h <= 0 {
            return;
        }

        // --- Bottom bar (Footer) ---
        if self.config.show_dividers {
            let rule_y = self.footer_top + density.px(6);
            fill_round_rect(
                page,
                self.margin,
                rule_y,
                max_width,
                density.px(2).max(1),
                0,
                0xc0,
            );
        }

        let has_progress = self.config.show_progress_bar;
        let row1 = if has_progress {
            self.footer_top + self.footer_h * 38 / 100 + self.x_height / 2
        } else {
            self.footer_top + (self.footer_h + self.x_height) / 2
        };

        let footer_slots = self.config.footer_slots;
        let has_any_footer_slot = footer_slots.iter().any(|&item| item != BarItem::None);
        if has_any_footer_slot {
            self.draw_slot_row(
                page,
                folio,
                footer_slots,
                self.margin,
                width - self.margin,
                row1,
                color,
                gap,
                density,
            );
        }

        if has_progress {
            let thickness = density.px(6).max(2);
            let radius = thickness / 2;
            let row2 = self.footer_top + self.footer_h - density.px(12);
            let pct = format!(
                "{}%",
                (folio.progress.clamp(0.0, 1.0) * 100.0).floor() as u32
            );
            let number = match (folio.page, folio.pages) {
                (Some(p), Some(total)) => format!("{p} / {total}"),
                (Some(p), None) => p.to_string(),
                (None, _) => String::new(),
            };
            let pct_plan = self.regular.plan(&pct, None, None);
            let number_plan = self.regular.plan(&number, None, None);
            let pct_x =
                self.margin + (max_width - pct_plan.width - number_plan.width - 2 * gap).max(0);
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

    #[allow(clippy::too_many_arguments)]
    fn draw_slot_row(
        &mut self,
        page: &mut Page,
        folio: &Folio,
        slots: [aristotle_screen::BarItem; 3],
        left_x: i32,
        right_x: i32,
        baseline: i32,
        color: Color,
        gap: i32,
        density: aristotle_screen::Density,
    ) {
        let total_w = (right_x - left_x).max(0);
        if total_w == 0 {
            return;
        }
        let left_parts = self.slot_parts(slots[0], folio);
        let center_parts = self.slot_parts(slots[1], folio);
        let right_parts = self.slot_parts(slots[2], folio);

        let has_center = !center_parts.is_empty();
        let has_right = !right_parts.is_empty();

        // Render right slot first so we know its exact width and can budget left/center.
        let mut right_w = 0;
        if has_right {
            let right_max = if has_center {
                total_w * 35 / 100
            } else {
                total_w * 55 / 100
            };
            let planned = self.plan_slot(&right_parts, right_max, density);
            right_w = planned.width;
            let x = (right_x - right_w).max(left_x);
            self.render_slot(page, color, &planned, x, baseline, density);
        }

        // Render center slot if present.
        let mut center_left = right_x - right_w;
        if has_center {
            let center_max = (total_w - right_w - gap * 2).clamp(0, total_w * 45 / 100);
            let planned = self.plan_slot(&center_parts, center_max, density);
            let cx = left_x + (total_w - planned.width) / 2;
            let cx = cx.min(right_x - right_w - gap - planned.width).max(left_x);
            self.render_slot(page, color, &planned, cx, baseline, density);
            center_left = cx;
        }

        // Render left slot into remaining space.
        if !left_parts.is_empty() {
            let left_max = if has_center || has_right {
                (center_left - left_x - gap).max(0)
            } else {
                total_w
            };
            if left_max > 0 {
                let planned = self.plan_slot(&left_parts, left_max, density);
                self.render_slot(page, color, &planned, left_x, baseline, density);
            }
        }
    }

    fn plan_slot(
        &mut self,
        parts: &SlotParts,
        max_w: i32,
        density: aristotle_screen::Density,
    ) -> PlannedSlot {
        let Some((pct, charging)) = parts.battery else {
            if parts.after.is_empty() || max_w <= 0 {
                return PlannedSlot::default();
            }
            let mut after = self.regular.plan(&parts.after, None, None);
            self.regular.crop_right(&mut after, max_w);
            let width = after.width;
            return PlannedSlot {
                before: None,
                battery: None,
                after_pad: 0,
                after: Some(after),
                width,
            };
        };

        let icon_w = density.px(24);
        if max_w < icon_w {
            return PlannedSlot::default();
        }
        let after_pad = if parts.after.is_empty() || parts.after.starts_with(' ') {
            0
        } else {
            density.px(6)
        };
        let mut after = if parts.after.is_empty() {
            None
        } else {
            Some(self.regular.plan(&parts.after, None, None))
        };
        let mut before = if parts.before.is_empty() {
            None
        } else {
            Some(self.regular.plan(&parts.before, None, None))
        };

        // Fit within max_w: prefer keeping battery icon + percentage intact,
        // cropping the prefix first and only cropping the suffix if needed.
        let after_w = after.as_ref().map_or(0, |p| p.width);
        let before_w = before.as_ref().map_or(0, |p| p.width);
        if before_w + icon_w + after_pad + after_w > max_w {
            let before_budget = (max_w - icon_w - after_pad - after_w).max(0);
            if before_budget == 0 {
                before = None;
            } else if let Some(ref mut bp) = before {
                self.regular.crop_right(bp, before_budget);
            }
            let after_budget = (max_w - icon_w - after_pad).max(0);
            if let Some(ref mut ap) = after {
                if ap.width > after_budget {
                    self.regular.crop_right(ap, after_budget);
                }
            }
        }

        let width = before.as_ref().map_or(0, |p| p.width)
            + icon_w
            + after_pad
            + after.as_ref().map_or(0, |p| p.width);
        PlannedSlot {
            before,
            battery: Some((pct, charging)),
            after_pad,
            after,
            width,
        }
    }

    fn render_slot(
        &mut self,
        page: &mut Page,
        color: Color,
        planned: &PlannedSlot,
        x: i32,
        baseline: i32,
        density: aristotle_screen::Density,
    ) {
        let mut cur_x = x;
        if let Some(ref before) = planned.before {
            if before.width > 0 {
                self.regular
                    .render(page, color, before, Point::new(cur_x, baseline));
                cur_x += before.width;
            }
        }
        if let Some((pct, charging)) = planned.battery {
            let icon_w = density.px(24);
            let icon_h = density.px(12).max(6);
            let icon_y = baseline - self.x_height / 2 - icon_h / 2;
            draw_mini_battery(
                page,
                icon_x_clamped(cur_x),
                icon_y,
                icon_w,
                icon_h,
                pct,
                charging,
                self.gray,
            );
            cur_x += icon_w + planned.after_pad;
        }
        if let Some(ref after) = planned.after {
            if after.width > 0 {
                self.regular
                    .render(page, color, after, Point::new(cur_x, baseline));
            }
        }
    }

    fn slot_parts(&self, item: aristotle_screen::BarItem, folio: &Folio) -> SlotParts {
        use aristotle_screen::{BarItem, WifiBadge};
        match item {
            BarItem::Battery => match self.status.battery_pct {
                Some(pct) => {
                    let after = if self.status.hide_battery_pct {
                        if self.status.charging {
                            "⚡".to_string()
                        } else {
                            String::new()
                        }
                    } else if self.status.charging {
                        format!("⚡{pct}%")
                    } else {
                        format!("{pct}%")
                    };
                    SlotParts {
                        before: String::new(),
                        battery: Some((pct, self.status.charging)),
                        after,
                    }
                }
                None => SlotParts::default(),
            },
            BarItem::DeviceStatusGroup => {
                let mut prefix = Vec::new();
                match self.status.wifi {
                    WifiBadge::Connected { .. } => prefix.push("Wi-Fi".to_string()),
                    WifiBadge::Airplane => {
                        let lbl = if self.status.wifi_airplane_label.is_empty() {
                            "✈"
                        } else {
                            &self.status.wifi_airplane_label
                        };
                        prefix.push(lbl.to_string());
                    }
                    WifiBadge::Disconnected | WifiBadge::Off => {}
                }
                if let Some((step, _)) = self.status.frontlight {
                    if step > 0 {
                        prefix.push(format!("☀{step}"));
                    }
                }
                let clock = if self.status.clock.is_empty() {
                    None
                } else {
                    Some(self.status.clock.as_str())
                };
                if let Some(pct) = self.status.battery_pct {
                    let before = if prefix.is_empty() {
                        String::new()
                    } else {
                        format!("{} · ", prefix.join(" · "))
                    };
                    let bat_txt = if !self.status.hide_battery_pct {
                        if self.status.charging {
                            format!("⚡{pct}%")
                        } else {
                            format!("{pct}%")
                        }
                    } else if self.status.charging {
                        "⚡".to_string()
                    } else {
                        String::new()
                    };
                    let after = match (bat_txt.is_empty(), clock) {
                        (false, Some(c)) => format!("{bat_txt} · {c}"),
                        (false, None) => bat_txt,
                        (true, Some(c)) => format!(" · {c}"),
                        (true, None) => String::new(),
                    };
                    SlotParts {
                        before,
                        battery: Some((pct, self.status.charging)),
                        after,
                    }
                } else {
                    if let Some(c) = clock {
                        prefix.push(c.to_string());
                    }
                    SlotParts {
                        before: String::new(),
                        battery: None,
                        after: prefix.join(" · "),
                    }
                }
            }
            _ => SlotParts {
                before: String::new(),
                battery: None,
                after: self.format_item(item, folio),
            },
        }
    }

    fn format_item(&self, item: aristotle_screen::BarItem, folio: &Folio) -> String {
        use aristotle_screen::{BarItem, WifiBadge};
        match item {
            BarItem::None | BarItem::Battery | BarItem::DeviceStatusGroup => String::new(),
            BarItem::BookTitle => folio.header.clone(),
            BarItem::ChapterTitle => folio.chapter.clone(),
            BarItem::Clock => self.status.clock.clone(),
            BarItem::Wifi => match self.status.wifi {
                WifiBadge::Connected { signal } => {
                    let bars = match signal {
                        0 => "·",
                        1 => "▂",
                        2 => "▄",
                        3 => "▆",
                        _ => "█",
                    };
                    format!("Wi-Fi {bars}")
                }
                WifiBadge::Airplane => {
                    if self.status.wifi_airplane_label.is_empty() {
                        "Airplane".to_string()
                    } else {
                        self.status.wifi_airplane_label.clone()
                    }
                }
                WifiBadge::Disconnected | WifiBadge::Off => {
                    if self.status.wifi_off_label.is_empty() {
                        String::new()
                    } else {
                        self.status.wifi_off_label.clone()
                    }
                }
            },
            BarItem::Frontlight => match self.status.frontlight {
                Some((0, _)) => {
                    let off = if self.status.light_off_label.is_empty() {
                        "Off"
                    } else {
                        &self.status.light_off_label
                    };
                    format!("☀ {off}")
                }
                Some((step, max)) => match self.status.warmth {
                    Some((w, _)) if w > 0 => format!("☀ {step}/{max} · W{w}"),
                    _ => format!("☀ {step}/{max}"),
                },
                None => String::new(),
            },
            BarItem::ProgressPercent => format!(
                "{}%",
                (folio.progress.clamp(0.0, 1.0) * 100.0).floor() as u32
            ),
            BarItem::BookPages => match (folio.page, folio.pages) {
                (Some(p), Some(total)) => format!("{p} / {total}"),
                (Some(p), None) => p.to_string(),
                (None, _) => String::new(),
            },
            BarItem::ChapterPages => match folio.chapter_page.or(self.extra_chapter_page) {
                Some((p, total)) if total > 0 => format!("{p} / {total}"),
                _ => String::new(),
            },
            BarItem::ChapterTimeLeft => folio.remaining.clone().unwrap_or_default(),
            BarItem::BookTimeLeft => folio
                .book_remaining
                .clone()
                .or_else(|| self.extra_book_remaining.clone())
                .unwrap_or_default(),
        }
    }
}

/// Renderable pieces of a single header/footer slot: optional text before an
/// inline battery icon, the battery icon itself, and the text after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SlotParts {
    before: String,
    battery: Option<(u8, bool)>,
    after: String,
}

impl SlotParts {
    fn is_empty(&self) -> bool {
        self.before.is_empty() && self.battery.is_none() && self.after.is_empty()
    }
}

#[derive(Default)]
struct PlannedSlot {
    before: Option<plato_core::font::RenderPlan>,
    battery: Option<(u8, bool)>,
    after_pad: i32,
    after: Option<plato_core::font::RenderPlan>,
    width: i32,
}

#[inline]
fn icon_x_clamped(x: i32) -> i32 {
    x.max(0)
}

/// Draw a crisp monochrome battery outline + level fill into `page`.
#[allow(clippy::too_many_arguments)]
fn draw_mini_battery(
    page: &mut Page,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    pct: u8,
    charging: bool,
    gray: u8,
) {
    if w < 8 || h < 6 {
        return;
    }
    let body_w = w - 2;
    // Outer border (1 px).
    fill_round_rect(page, x, y, body_w, 1, 0, gray);
    fill_round_rect(page, x, y + h - 1, body_w, 1, 0, gray);
    fill_round_rect(page, x, y, 1, h, 0, gray);
    fill_round_rect(page, x + body_w - 1, y, 1, h, 0, gray);
    // Positive terminal nub on the right.
    let nub_h = (h / 2).max(2);
    let nub_y = y + (h - nub_h) / 2;
    fill_round_rect(page, x + body_w, nub_y, 2, nub_h, 0, gray);
    // Inner charge fill.
    let inner_w = (body_w - 4).max(0);
    let fill_w = ((inner_w as f32 * pct.min(100) as f32 / 100.0).round() as i32).clamp(0, inner_w);
    if fill_w > 0 {
        let fill_gray = if charging { 0 } else { gray };
        fill_round_rect(page, x + 2, y + 2, fill_w, (h - 4).max(1), 0, fill_gray);
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

#[cfg(test)]
mod tests {
    use super::*;
    use aristotle_screen::{BarItem, WifiBadge};

    fn sample_chrome_with_status(status: DeviceBarStatus) -> Chrome {
        let style = Style::pw4();
        let opener = FontOpener::new().expect("font opener");
        let font_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aristotle-ui/assets/fonts/Literata-Regular.ttf"
        );
        let mut regular = opener.open(font_path).expect("embedded Literata font");
        let size = style.chrome_size_26_6();
        regular.set_size(size, style.dpi);
        let x_height = regular.x_heights.0 as i32;
        let text_top = style.text_top as i32;
        Chrome {
            regular,
            size,
            dpi: style.dpi,
            gray: style.chrome_gray,
            margin: style.margin_px(),
            text_top,
            header_baseline: (text_top + x_height) / 2,
            footer_top: style.height as i32 - style.text_bottom as i32,
            footer_h: style.text_bottom as i32,
            x_height,
            config: style.bar_config,
            status,
            extra_book_remaining: None,
            extra_chapter_page: None,
            has_footnotes: false,
        }
    }

    #[test]
    fn device_status_group_places_battery_icon_adjacent_to_percentage() {
        let mut chrome = sample_chrome_with_status(DeviceBarStatus {
            clock: "21:38".to_string(),
            battery_pct: Some(84),
            hide_battery_pct: false,
            charging: false,
            wifi: WifiBadge::Connected { signal: 3 },
            frontlight: Some((12, 24)),
            ..Default::default()
        });
        let folio = Folio::default();
        let parts = chrome.slot_parts(BarItem::DeviceStatusGroup, &folio);
        assert_eq!(parts.before, "Wi-Fi · ☀12 · ");
        assert_eq!(parts.battery, Some((84, false)));
        assert_eq!(parts.after, "84% · 21:38");

        // When percentage text is hidden, the icon stays between prefix and clock.
        chrome.status.hide_battery_pct = true;
        let hidden = chrome.slot_parts(BarItem::DeviceStatusGroup, &folio);
        assert_eq!(hidden.before, "Wi-Fi · ☀12 · ");
        assert_eq!(hidden.battery, Some((84, false)));
        assert_eq!(hidden.after, " · 21:38");

        // Standalone Battery item has no prefix and puts icon directly before percentage.
        chrome.status.hide_battery_pct = false;
        let standalone = chrome.slot_parts(BarItem::Battery, &folio);
        assert_eq!(standalone.before, "");
        assert_eq!(standalone.battery, Some((84, false)));
        assert_eq!(standalone.after, "84%");
    }

    #[test]
    fn device_status_group_planned_layout_orders_prefix_icon_and_percentage() {
        let mut chrome = sample_chrome_with_status(DeviceBarStatus {
            clock: "21:38".to_string(),
            battery_pct: Some(84),
            hide_battery_pct: false,
            charging: false,
            wifi: WifiBadge::Connected { signal: 3 },
            frontlight: Some((12, 24)),
            ..Default::default()
        });
        let density = aristotle_screen::Density::new(chrome.dpi()).unwrap_or_default();
        let parts = chrome.slot_parts(BarItem::DeviceStatusGroup, &Folio::default());
        let planned = chrome.plan_slot(&parts, 800, density);
        let before_w = planned.before.as_ref().map_or(0, |p| p.width);
        let after_w = planned.after.as_ref().map_or(0, |p| p.width);
        assert!(
            before_w > 0,
            "Wi-Fi and frontlight prefix must be planned before battery icon"
        );
        assert!(
            after_w > 0,
            "percentage and clock must be planned after battery icon"
        );
        assert_eq!(
            planned.width,
            before_w + density.px(24) + planned.after_pad + after_w
        );
    }
}
