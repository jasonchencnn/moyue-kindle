//! Desktop simulator entry point.
//!
//! PLAT-004 only ships the headless backend: it renders a test pattern into a
//! [`GrayBuffer`] and writes it as a 16-level grayscale PNG. The windowed
//! backend arrives with Slint in K1 (ADR-004).

mod headless;

use inkread_platform::{GrayBuffer, Rect, BLACK};
use std::process::ExitCode;

/// Screen size of the target device, the Kindle Oasis 2 (1264 x 1680).
const WIDTH: u32 = 1264;
/// Screen height of the target device in pixels.
const HEIGHT: u32 = 1680;
/// Width of the black frame drawn around the test pattern.
const BORDER: u32 = 4;
/// Horizontal margin between the frame and the gray bar.
const BAR_MARGIN: u32 = 80;
/// Height of the gray bar.
const BAR_HEIGHT: u32 = 256;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    let path = args.next();
    let trailing = args.next();

    match (command.as_deref(), path, trailing) {
        (Some("--headless"), Some(path), None) => match write_headless(&path) {
            Ok(()) => {
                println!("wrote {path}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("failed to write {path}: {err}");
                ExitCode::FAILURE
            }
        },
        _ => {
            usage();
            ExitCode::from(2)
        }
    }
}

/// Render the test pattern and write it to `path`.
fn write_headless(path: &str) -> std::io::Result<()> {
    let buffer = test_pattern();
    headless::write_png(&buffer, std::path::Path::new(path))
}

/// A white page with a black frame and a 16-step grayscale bar.
fn test_pattern() -> GrayBuffer {
    let mut buffer = GrayBuffer::new(WIDTH, HEIGHT);
    let width = WIDTH as i32;
    let height = HEIGHT as i32;
    let border = BORDER as i32;

    buffer.fill_rect(Rect::new(0, 0, width, border), BLACK);
    buffer.fill_rect(Rect::new(0, height - border, width, border), BLACK);
    buffer.fill_rect(Rect::new(0, 0, border, height), BLACK);
    buffer.fill_rect(Rect::new(width - border, 0, border, height), BLACK);

    let bar_width = (WIDTH - 2 * BAR_MARGIN) / 16;
    let bar_x = BAR_MARGIN as i32;
    let bar_y = ((HEIGHT - BAR_HEIGHT) / 2) as i32;
    for level in 0..16u32 {
        let value = (level * 17) as u8;
        buffer.fill_rect(
            Rect::new(
                bar_x + (level * bar_width) as i32,
                bar_y,
                bar_width as i32,
                BAR_HEIGHT as i32,
            ),
            value,
        );
    }

    buffer
}

/// Print the command line accepted by the simulator.
fn usage() {
    eprintln!("usage: inkread-simulator --headless <out.png>");
}
