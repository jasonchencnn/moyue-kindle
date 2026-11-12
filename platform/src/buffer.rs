//! 8-bit grayscale frame buffer.

use crate::geom::Rect;
use std::fmt;

/// Pixel value of the darkest pixel (black).
pub const BLACK: u8 = 0;

/// Pixel value of the brightest pixel (white).
pub const WHITE: u8 = 255;

/// A linear 8-bit grayscale frame buffer: one byte per pixel, row-major.
///
/// `0` is black and `255` is white. A freshly created buffer is filled with
/// [`WHITE`], the blank-page colour of an e-ink panel.
///
/// Every write clips to the buffer bounds instead of panicking, so a renderer
/// may draw straight into the buffer without pre-validating its coordinates.
#[derive(Clone, PartialEq, Eq)]
pub struct GrayBuffer {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl GrayBuffer {
    /// Create a white buffer of `width` x `height` pixels.
    pub fn new(width: u32, height: u32) -> Self {
        Self::filled(width, height, WHITE)
    }

    /// Create a buffer of `width` x `height` pixels filled with `value`.
    pub fn filled(width: u32, height: u32, value: u8) -> Self {
        let len = (width as usize).saturating_mul(height as usize);
        Self {
            width,
            height,
            pixels: vec![value; len],
        }
    }

    /// Buffer width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Buffer height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Whether the buffer holds no pixel.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The raw pixels, row-major, `width * height` bytes long.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The raw pixels, row-major, for direct access by a renderer.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Read the pixel at `(x, y)`, or `None` when it lies outside the buffer.
    pub fn get_pixel(&self, x: i32, y: i32) -> Option<u8> {
        if !self.contains(x, y) {
            return None;
        }
        Some(self.pixels[self.index(x, y)])
    }

    /// Write `value` at `(x, y)`.
    ///
    /// Returns whether the pixel was written; a coordinate outside the buffer
    /// is clipped away and leaves the buffer untouched.
    pub fn set_pixel(&mut self, x: i32, y: i32, value: u8) -> bool {
        if !self.contains(x, y) {
            return false;
        }
        let index = self.index(x, y);
        self.pixels[index] = value;
        true
    }

    /// Fill `rect` with `value`, clipped to the buffer bounds.
    ///
    /// A rectangle that lies completely outside the buffer draws nothing.
    pub fn fill_rect(&mut self, rect: Rect, value: u8) {
        let Some(area) = self.clip_to_bounds(rect) else {
            return;
        };
        let row_len = self.width as usize;
        for y in area.y..area.bottom() {
            let start = y as usize * row_len + area.x as usize;
            self.pixels[start..start + area.width as usize].fill(value);
        }
    }

    /// Fill the whole buffer with `value`.
    pub fn fill(&mut self, value: u8) {
        self.pixels.fill(value);
    }

    /// Copy a rectangle of `src` into this buffer at `(dst_x, dst_y)`.
    ///
    /// The copy is 1:1, no scaling. Both the source rectangle and the
    /// destination area are clipped to their own bounds, so pixels that would
    /// come from or land outside a buffer are skipped instead of panicking.
    pub fn blit(&mut self, src: &GrayBuffer, src_rect: Rect, dst_x: i32, dst_y: i32) {
        let Some(src_area) = src.clip_to_bounds(src_rect) else {
            return;
        };
        // Where the (clipped) source pixels land in this buffer.
        let dest = Rect::new(
            dst_x.saturating_add(src_area.x - src_rect.x),
            dst_y.saturating_add(src_area.y - src_rect.y),
            src_area.width,
            src_area.height,
        );
        let Some(dest_area) = self.clip_to_bounds(dest) else {
            return;
        };
        let src_w = src.width as usize;
        let dst_w = self.width as usize;
        let src_x = src_area.x + (dest_area.x - dest.x);
        let src_y = src_area.y + (dest_area.y - dest.y);
        let len = dest_area.width as usize;
        for row in 0..dest_area.height as usize {
            let src_start = (src_y + row as i32) as usize * src_w + src_x as usize;
            let dst_start = (dest_area.y + row as i32) as usize * dst_w + dest_area.x as usize;
            self.pixels[dst_start..dst_start + len]
                .copy_from_slice(&src.pixels[src_start..src_start + len]);
        }
    }

    /// Whether `(x, y)` addresses a pixel of this buffer.
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u64) < self.width as u64 && (y as u64) < self.height as u64
    }

    /// Index of the pixel at `(x, y)`; the caller must have checked bounds.
    fn index(&self, x: i32, y: i32) -> usize {
        y as usize * self.width as usize + x as usize
    }

    /// The part of `rect` that lies inside the buffer, or `None` if there is none.
    fn clip_to_bounds(&self, rect: Rect) -> Option<Rect> {
        if rect.is_empty() {
            return None;
        }
        let x0 = (rect.x as i64).max(0);
        let y0 = (rect.y as i64).max(0);
        let x1 = (rect.x as i64 + rect.width as i64).min(self.width.min(i32::MAX as u32) as i64);
        let y1 = (rect.y as i64 + rect.height as i64).min(self.height.min(i32::MAX as u32) as i64);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Rect::new(
            x0 as i32,
            y0 as i32,
            (x1 - x0) as i32,
            (y1 - y0) as i32,
        ))
    }
}

impl fmt::Debug for GrayBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrayBuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(width: u32, height: u32) -> GrayBuffer {
        GrayBuffer::filled(width, height, BLACK)
    }

    #[test]
    fn new_buffer_is_white() {
        let buf = GrayBuffer::new(4, 3);
        assert_eq!(buf.width(), 4);
        assert_eq!(buf.height(), 3);
        assert_eq!(buf.pixels().len(), 12);
        assert!(buf.pixels().iter().all(|p| *p == WHITE));
    }

    #[test]
    fn set_and_get_pixel() {
        let mut buf = GrayBuffer::new(4, 4);
        assert!(buf.set_pixel(1, 2, 128));
        assert_eq!(buf.get_pixel(1, 2), Some(128));
        assert_eq!(buf.get_pixel(0, 2), Some(WHITE));
    }

    #[test]
    fn get_pixel_out_of_bounds_is_none() {
        let buf = GrayBuffer::new(4, 4);
        assert_eq!(buf.get_pixel(-1, 0), None);
        assert_eq!(buf.get_pixel(0, -1), None);
        assert_eq!(buf.get_pixel(4, 0), None);
        assert_eq!(buf.get_pixel(0, 4), None);
    }

    #[test]
    fn set_pixel_out_of_bounds_is_ignored() {
        let mut buf = GrayBuffer::new(4, 4);
        assert!(!buf.set_pixel(-1, 0, BLACK));
        assert!(!buf.set_pixel(0, -1, BLACK));
        assert!(!buf.set_pixel(4, 0, BLACK));
        assert!(!buf.set_pixel(0, 4, BLACK));
        assert!(buf.pixels().iter().all(|p| *p == WHITE));
    }

    #[test]
    fn fill_rect_clips_at_the_left_edge() {
        let mut buf = filled(8, 8);
        buf.fill_rect(Rect::new(-3, 1, 6, 2), 200);
        // Only the columns 0..3 of the rectangle were inside the buffer.
        assert_eq!(buf.get_pixel(0, 1), Some(200));
        assert_eq!(buf.get_pixel(2, 1), Some(200));
        assert_eq!(buf.get_pixel(3, 1), Some(BLACK));
        assert_eq!(buf.get_pixel(0, 3), Some(BLACK));
    }

    #[test]
    fn fill_rect_clips_at_the_top_edge() {
        let mut buf = filled(8, 8);
        buf.fill_rect(Rect::new(1, -2, 2, 5), 200);
        assert_eq!(buf.get_pixel(1, 0), Some(200));
        assert_eq!(buf.get_pixel(1, 2), Some(200));
        assert_eq!(buf.get_pixel(1, 3), Some(BLACK));
    }

    #[test]
    fn fill_rect_clips_at_the_right_edge() {
        let mut buf = filled(8, 8);
        buf.fill_rect(Rect::new(6, 1, 5, 2), 200);
        assert_eq!(buf.get_pixel(6, 1), Some(200));
        assert_eq!(buf.get_pixel(7, 1), Some(200));
        assert_eq!(buf.get_pixel(5, 1), Some(BLACK));
        assert_eq!(buf.get_pixel(6, 3), Some(BLACK));
    }

    #[test]
    fn fill_rect_clips_at_the_bottom_edge() {
        let mut buf = filled(8, 8);
        buf.fill_rect(Rect::new(1, 6, 2, 5), 200);
        assert_eq!(buf.get_pixel(1, 6), Some(200));
        assert_eq!(buf.get_pixel(1, 7), Some(200));
        assert_eq!(buf.get_pixel(1, 5), Some(BLACK));
    }

    #[test]
    fn fill_rect_fully_outside_the_buffer_draws_nothing() {
        let mut buf = filled(8, 8);
        buf.fill_rect(Rect::new(100, 100, 10, 10), 200);
        buf.fill_rect(Rect::new(-100, -100, 10, 10), 200);
        // Spans the whole width but sits below the buffer.
        buf.fill_rect(Rect::new(0, 100, 8, 4), 200);
        assert!(buf.pixels().iter().all(|p| *p == BLACK));
    }

    #[test]
    fn fill_rect_covers_the_whole_area() {
        let mut buf = filled(4, 4);
        buf.fill_rect(Rect::new(1, 1, 2, 2), 77);
        assert_eq!(buf.get_pixel(0, 0), Some(BLACK));
        assert_eq!(buf.get_pixel(1, 1), Some(77));
        assert_eq!(buf.get_pixel(2, 2), Some(77));
        assert_eq!(buf.get_pixel(3, 3), Some(BLACK));
    }

    #[test]
    fn blit_copies_pixels() {
        let mut src = GrayBuffer::filled(2, 2, 10);
        src.set_pixel(1, 1, 20);
        let mut dst = filled(4, 4);
        dst.blit(&src, Rect::new(0, 0, 2, 2), 1, 1);
        assert_eq!(dst.get_pixel(1, 1), Some(10));
        assert_eq!(dst.get_pixel(2, 2), Some(20));
        assert_eq!(dst.get_pixel(0, 0), Some(BLACK));
    }

    #[test]
    fn blit_clips_at_the_destination_edges() {
        let src = GrayBuffer::filled(2, 2, 10);
        let mut dst = filled(4, 4);
        dst.blit(&src, Rect::new(0, 0, 2, 2), 3, 3);
        assert_eq!(dst.get_pixel(3, 3), Some(10));
        assert_eq!(dst.get_pixel(2, 3), Some(BLACK));
        assert_eq!(dst.get_pixel(3, 2), Some(BLACK));
    }

    #[test]
    fn blit_clips_a_source_rect_outside_the_source_buffer() {
        let mut src = GrayBuffer::filled(2, 2, 10);
        src.set_pixel(1, 1, 20);
        let mut dst = filled(4, 4);
        // The source rectangle starts one pixel to the left of the source, so
        // the missing column shifts the visible content one pixel to the right.
        dst.blit(&src, Rect::new(-1, -1, 4, 4), 0, 0);
        assert_eq!(dst.get_pixel(1, 1), Some(10));
        assert_eq!(dst.get_pixel(2, 2), Some(20));
        assert_eq!(dst.get_pixel(0, 0), Some(BLACK));
    }

    #[test]
    fn blit_fully_outside_the_destination_draws_nothing() {
        let src = GrayBuffer::filled(2, 2, 10);
        let mut dst = filled(4, 4);
        dst.blit(&src, Rect::new(0, 0, 2, 2), 100, 100);
        assert!(dst.pixels().iter().all(|p| *p == BLACK));
    }
}
