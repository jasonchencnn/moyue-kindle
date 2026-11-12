//! Platform abstraction shared by the desktop simulator and the Kindle client.
//!
//! The crate holds the pieces both sides need: the 8-bit grayscale frame
//! buffer Slint renders into (ADR-004), the refresh requests a display backend
//! consumes, and the `Display`, `Input` and `Power` traits every backend
//! implements.
//!
//! It contains no device code. The FBInk and evdev backends live in `eink`,
//! the windowed backend in `simulator`.

pub mod buffer;
pub mod display;
pub mod error;
pub mod geom;
pub mod input;
pub mod power;

pub use buffer::{GrayBuffer, BLACK, WHITE};
pub use display::{check_present, Display, RefreshMode, RefreshRequest};
pub use error::{Error, Result};
pub use geom::{merge_rects, Rect};
pub use input::{Input, InputEvent, KeyCode, TouchPhase};
pub use power::Power;
