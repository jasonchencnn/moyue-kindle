//! Error type shared by the platform traits.

use crate::geom::Rect;
use std::fmt;

/// Errors reported by platform backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The frame buffer does not have the size the display expects.
    SizeMismatch {
        /// Size the display expects, as `(width, height)` in pixels.
        expected: (u32, u32),
        /// Size of the buffer that was handed over.
        actual: (u32, u32),
    },
    /// The requested rectangle lies outside the display.
    OutOfBounds {
        /// The offending rectangle.
        rect: Rect,
        /// Size of the display the rectangle was checked against.
        display: (u32, u32),
    },
    /// The backend failed for a device-specific reason.
    Backend(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::SizeMismatch { expected, actual } => {
                write!(
                    f,
                    "frame size {actual:?} does not match display size {expected:?}"
                )
            }
            Error::OutOfBounds { rect, display } => {
                write!(f, "rectangle {rect:?} is outside display {display:?}")
            }
            Error::Backend(msg) => write!(f, "backend error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Result of a fallible platform operation.
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_failing_values() {
        let e = Error::SizeMismatch {
            expected: (1264, 1680),
            actual: (600, 800),
        };
        assert!(e.to_string().contains("600"));
        assert!(e.to_string().contains("1264"));

        let e = Error::OutOfBounds {
            rect: Rect::new(0, 0, 10, 10),
            display: (8, 8),
        };
        assert!(e.to_string().contains("outside"));
    }
}
