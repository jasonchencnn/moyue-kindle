//! Display backends and refresh requests.

use crate::buffer::GrayBuffer;
use crate::error::{Error, Result};
use crate::geom::Rect;

/// How the panel is refreshed for one frame.
///
/// E-ink panels keep their old image until a waveform is applied, so the mode
/// trades speed against ghosting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefreshMode {
    /// Update only the requested rectangle, without changing the waveform.
    /// Fastest, but previous content may stay visible.
    Partial,
    /// Update the whole panel with a clearing waveform. Slowest, no ghosting.
    Full,
    /// Update the requested rectangle with the fastest waveform the panel
    /// offers. May leave visible artefacts.
    Fast,
}

/// A request to push pixels to the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RefreshRequest {
    /// Region of the display that changed.
    pub rect: Rect,
    /// Waveform used to apply the update.
    pub mode: RefreshMode,
}

impl RefreshRequest {
    /// Create a refresh request.
    pub const fn new(rect: Rect, mode: RefreshMode) -> Self {
        Self { rect, mode }
    }

    /// A request that refreshes the whole panel.
    pub fn full(width: u32, height: u32) -> Self {
        Self::new(
            Rect::from_size(
                width.min(i32::MAX as u32) as i32,
                height.min(i32::MAX as u32) as i32,
            ),
            RefreshMode::Full,
        )
    }
}

/// A display backend, e.g. FBInk on the device or a window in the simulator.
///
/// Implementations receive the whole frame buffer and the region that changed;
/// they decide how much of it to push to the panel.
pub trait Display {
    /// Panel size in pixels, as `(width, height)`.
    fn size(&self) -> (u32, u32);

    /// Present a frame buffer.
    ///
    /// The frame is expected to have the size reported by [`Display::size`]
    /// and the request must stay inside the panel; use [`Display::check`] to
    /// validate both before touching the hardware.
    fn present(&mut self, frame: &GrayBuffer, request: &RefreshRequest) -> Result<()>;

    /// Validate that `frame` and `request` fit this display.
    fn check(&self, frame: &GrayBuffer, request: &RefreshRequest) -> Result<()> {
        check_present(self.size(), frame, request)
    }
}

/// Check that `frame` and `request` fit a display of the given size.
///
/// A frame whose size differs from the display, or a request rectangle that
/// reaches outside the panel, is rejected. An empty request rectangle is
/// rejected too: it covers no pixel, so a backend would receive a position
/// that is not a valid refresh region.
pub fn check_present(
    display: (u32, u32),
    frame: &GrayBuffer,
    request: &RefreshRequest,
) -> Result<()> {
    let actual = (frame.width(), frame.height());
    if actual != display {
        return Err(Error::SizeMismatch {
            expected: display,
            actual,
        });
    }
    let (w, h) = (
        display.0.min(i32::MAX as u32) as i32,
        display.1.min(i32::MAX as u32) as i32,
    );
    let rect = request.rect;
    if rect.is_empty() || rect.x < 0 || rect.y < 0 || rect.right() > w || rect.bottom() > h {
        return Err(Error::OutOfBounds { rect, display });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::WHITE;

    #[test]
    fn full_request_covers_the_panel() {
        let req = RefreshRequest::full(1264, 1680);
        assert_eq!(req.mode, RefreshMode::Full);
        assert_eq!(req.rect, Rect::new(0, 0, 1264, 1680));
    }

    #[test]
    fn check_accepts_a_matching_frame_and_rect() {
        let frame = GrayBuffer::new(8, 6);
        let req = RefreshRequest::new(Rect::new(0, 0, 8, 6), RefreshMode::Partial);
        assert!(check_present((8, 6), &frame, &req).is_ok());
    }

    #[test]
    fn check_rejects_a_frame_of_the_wrong_size() {
        let frame = GrayBuffer::new(8, 6);
        let req = RefreshRequest::new(Rect::new(0, 0, 8, 6), RefreshMode::Partial);
        let err = check_present((10, 10), &frame, &req).expect_err("size mismatch");
        assert_eq!(
            err,
            Error::SizeMismatch {
                expected: (10, 10),
                actual: (8, 6)
            }
        );
    }

    #[test]
    fn check_rejects_a_rect_outside_the_panel() {
        let frame = GrayBuffer::new(8, 6);
        let req = RefreshRequest::new(Rect::new(4, 4, 8, 4), RefreshMode::Partial);
        let err = check_present((8, 6), &frame, &req).expect_err("out of bounds");
        assert_eq!(
            err,
            Error::OutOfBounds {
                rect: Rect::new(4, 4, 8, 4),
                display: (8, 6)
            }
        );
    }

    #[test]
    fn check_rejects_a_negative_rect() {
        let frame = GrayBuffer::new(8, 6);
        let req = RefreshRequest::new(Rect::new(-1, 0, 4, 4), RefreshMode::Partial);
        assert!(check_present((8, 6), &frame, &req).is_err());
    }

    #[test]
    fn a_display_can_use_the_default_check() {
        struct Fake;
        impl Display for Fake {
            fn size(&self) -> (u32, u32) {
                (8, 6)
            }
            fn present(&mut self, frame: &GrayBuffer, request: &RefreshRequest) -> Result<()> {
                self.check(frame, request)?;
                assert_eq!(frame.pixels()[0], WHITE);
                Ok(())
            }
        }
        let mut fake = Fake;
        let frame = GrayBuffer::new(8, 6);
        let req = RefreshRequest::new(Rect::new(0, 0, 2, 2), RefreshMode::Fast);
        assert!(fake.present(&frame, &req).is_ok());
    }
}
