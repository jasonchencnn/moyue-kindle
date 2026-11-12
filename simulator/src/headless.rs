//! Headless PNG output for the desktop simulator.
//!
//! The simulator works on the same 8-bit [`GrayBuffer`] as the Kindle client.
//! A real e-ink panel shows 16 gray levels, so the headless backend quantizes
//! the buffer before writing an 8-bit grayscale PNG.

use inkread_platform::GrayBuffer;
use std::fs::File;
use std::io::{BufWriter, Result};
use std::path::Path;

/// Number of gray levels an e-ink panel can show.
const LEVELS: u32 = 16;

/// Distance between two adjacent gray levels, `255 / (16 - 1)`.
const STEP: u32 = 255 / (LEVELS - 1);

/// Quantize an 8-bit gray value to the nearest of the 16 e-ink levels.
///
/// The levels are `0, 17, 34, …, 255`. Values equally close to two levels are
/// rounded up, which is what integer division by [`STEP`] does after adding
/// half a step.
pub fn quantize16(v: u8) -> u8 {
    (((v as u32 + STEP / 2) / STEP) * STEP) as u8
}

/// Write `buf` as an 8-bit grayscale PNG at `path`.
///
/// Every pixel is passed through [`quantize16`] first, so the file contains
/// only values the target panel can display. The PNG dimensions match the
/// buffer dimensions.
pub fn write_png(buf: &GrayBuffer, path: &Path) -> Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, buf.width(), buf.height());
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder.write_header()?;

    let pixels: Vec<u8> = buf.pixels().iter().map(|v| quantize16(*v)).collect();
    png_writer.write_image_data(&pixels)?;
    png_writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkread_platform::Rect;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn quantize16_maps_endpoints_exactly() {
        assert_eq!(quantize16(0), 0);
        assert_eq!(quantize16(255), 255);
    }

    #[test]
    fn quantize16_maps_the_exact_levels_to_themselves() {
        for level in 0..LEVELS {
            let value = (level * STEP) as u8;
            assert_eq!(quantize16(value), value);
        }
    }

    #[test]
    fn quantize16_rounds_to_the_nearest_level() {
        assert_eq!(quantize16(1), 0);
        assert_eq!(quantize16(8), 0);
        assert_eq!(quantize16(9), 17);
        assert_eq!(quantize16(16), 17);
        assert_eq!(quantize16(128), 136);
        assert_eq!(quantize16(127), 119);
        assert_eq!(quantize16(250), 255);
    }

    #[test]
    fn write_png_round_trips_through_the_png_crate() {
        let mut buf = GrayBuffer::new(4, 3);
        assert!(buf.set_pixel(0, 0, 0));
        assert!(buf.set_pixel(1, 0, 9));
        assert!(buf.set_pixel(2, 0, 128));
        assert!(buf.set_pixel(3, 0, 255));
        buf.fill_rect(Rect::new(0, 1, 4, 2), 200);

        let path = temp_png_path();
        write_png(&buf, &path).expect("PNG is written");

        let decoder = png::Decoder::new(File::open(&path).expect("PNG exists"));
        let mut reader = decoder.read_info().expect("PNG header is valid");
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels).expect("PNG data is valid");
        assert_eq!(info.width, 4);
        assert_eq!(info.height, 3);
        assert_eq!(info.color_type, png::ColorType::Grayscale);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);

        let expected_row = [0, 17, 136, 255];
        assert_eq!(&pixels[..4], &expected_row);
        assert!(pixels[4..].iter().all(|p| *p == 204));

        fs::remove_file(path).expect("temporary PNG is removed");
    }

    /// A unique path in the system temporary directory.
    fn temp_png_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "inkread-simulator-{}-{nanos}.png",
            std::process::id()
        ))
    }
}
