// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kobo i.MX EPDC backend: the "kobo1" update ABI of the first-batch models.
//!
//! The Clara HD (nova, mark 7) and the Libra 2 (io, mark 9) both drive the
//! panel through the i.MX EPDC driver, which Plato talks to with its
//! `KoboFramebuffer1`. They do not use the sunxi "kobo2" backend of the
//! mark-8 devices (Sage, Elipsa), so that family is rejected here instead of
//! being guessed at.
//!
//! # ABI
//!
//! Mark 7 and up send `mxcfb_update_data_v2`, 72 bytes:
//!
//! ```text
//! mxcfb_rect(16) + waveform_mode(4) + update_mode(4) + update_marker(4)
//! + temp(4) + flags(4) + dither_mode(4) + quant_bit(4)
//! + alt_buffer_data_v2(28)
//! ```
//!
//! The layout is **not** the Kindle K51 layout, although both are 72 bytes
//! and both answer `_IOW('F', 0x2E, 72)`: the field order differs, so reusing
//! the Kindle struct would send the driver nonsense. Completion is awaited
//! with an 8-byte `mxcfb_update_marker_data`.
//!
//! # Waveforms
//!
//! Kobo numbers them differently from Kindle: A2 is 4, GL16 is 5, GLR16 is
//! 6, GLD16 is 7; mark 9 adds GCK16 9 and GLKW16 10. The app's [`Waveform`]
//! values are Kindle numbers, so [`waveform_mode`] maps them explicitly.
//!
//! # Bits per pixel
//!
//! Plato can draw 8, 16 and 32 bpp panels; this build draws 8-bit gray only.
//! Both first-batch models are expected to report 8 bpp (Plato only forces
//! the depth down on older models), and any other depth is refused with an
//! `Unsupported` error rather than written as if it were 8-bit.

use std::fs::File;
use std::io;

use aristotle_screen::model::{Geometry as ModelGeometry, KoboModel};

use crate::framebuffer::{ioctl, Rect, Waveform};

/// `_IOW('F', 0x2E, struct mxcfb_update_data_v2)`, 72-byte payload.
pub(crate) const MXCFB_SEND_UPDATE_V2: libc::c_ulong = 0x4048_462E;
/// `_IOWR('F', 0x2F, struct mxcfb_update_marker_data)`, 8-byte payload.
pub(crate) const MXCFB_WAIT_FOR_UPDATE_COMPLETE: libc::c_ulong = 0xC008_462F;

/// `EPDC_FLAG_ENABLE_INVERSION`, the hardware night-mode flag below mark 11.
pub(crate) const EPDC_FLAG_ENABLE_INVERSION: u32 = 0x01;
/// `EPDC_FLAG_FORCE_MONOCHROME`, used for undithered DU updates.
const EPDC_FLAG_FORCE_MONOCHROME: u32 = 0x02;

const UPDATE_MODE_PARTIAL: u32 = 0;
const UPDATE_MODE_FULL: u32 = 1;
const TEMP_USE_AMBIENT: i32 = 0x1000;

/// The alternate-buffer tail of `mxcfb_update_data_v2`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AltBufferDataV2 {
    phys_addr: u32,
    width: u32,
    height: u32,
    alt_update_region: Rect,
}

/// `mxcfb_update_data_v2`: the update request of the Kobo i.MX EPDC driver
/// from model mark 7 on.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct KoboUpdateDataV2 {
    update_region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    temp: i32,
    flags: u32,
    dither_mode: i32,
    quant_bit: i32,
    alt_buffer_data: AltBufferDataV2,
}

/// `mxcfb_update_marker_data`, the completion request of mark 7 and up.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct KoboMarkerData {
    pub update_marker: u32,
    pub collision_test: u32,
}

// The driver's ABI, locked at compile time: an `_IOW` request carries the
// payload size, so the struct and the request number must agree.
const _: () = assert!(std::mem::size_of::<KoboUpdateDataV2>() == 72);
const _: () = assert!(std::mem::size_of::<KoboMarkerData>() == 8);
const _: () = assert!(
    ((MXCFB_SEND_UPDATE_V2 >> 16) & 0x3FFF) as usize == std::mem::size_of::<KoboUpdateDataV2>()
);
const _: () = assert!(
    ((MXCFB_WAIT_FOR_UPDATE_COMPLETE >> 16) & 0x3FFF) as usize
        == std::mem::size_of::<KoboMarkerData>()
);

/// The geometry of a Kobo framebuffer, checked against the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KoboGeometry {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bits_per_pixel: u32,
    pub rotate: u32,
}

impl KoboGeometry {
    /// Check `FBIOGET_VSCREENINFO`/`FBIOGET_FSCREENINFO` against `model`.
    ///
    /// Only 8 bpp is accepted: the rest of this build writes one gray byte
    /// per pixel. A 16 or 32 bpp panel is an explicit `Unsupported` error,
    /// not a silently corrupted frame.
    pub fn validate(
        model: KoboModel,
        xres: u32,
        yres: u32,
        stride: u32,
        bits_per_pixel: u32,
        rotate: u32,
    ) -> io::Result<Self> {
        if bits_per_pixel != 8 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "Kobo {} framebuffer reports {bits_per_pixel} bpp; this build draws 8 bpp only",
                    model.name()
                ),
            ));
        }
        let expected: ModelGeometry = model.geometry();
        let portrait = (xres, yres) == (expected.width, expected.height);
        let landscape = (xres, yres) == (expected.height, expected.width);
        if !portrait && !landscape {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Kobo {} framebuffer is {xres}x{yres}, expected {}x{}",
                    model.name(),
                    expected.width,
                    expected.height
                ),
            ));
        }
        if xres == 0 || yres == 0 || stride < xres {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Kobo framebuffer {xres}x{yres} has row stride {stride}"),
            ));
        }
        Ok(Self {
            width: xres,
            height: yres,
            stride,
            bits_per_pixel,
            rotate,
        })
    }
}

/// The Kobo waveform mode for an app [`Waveform`], or `None` for a value
/// this backend does not map.
///
/// The app's numbers are Kindle's (A2 6, GL16 3, GLR16 4, GLD16 5, GCK16 8,
/// GLKW16 9, AUTO 257). Kobo's are NTX (A2 4, GL16 5, GLR16 6, GLD16 7,
/// GCK16 9, GLKW16 10). Values outside the table are refused instead of
/// being passed through to the driver as a different waveform.
pub(crate) fn waveform_mode(model: KoboModel, waveform: Waveform, inverted: bool) -> Option<u32> {
    let mark = model.mark();
    let mode = match waveform.0 {
        0 => 0,              // INIT
        1 => 1,              // DU
        2 => 2,              // GC16
        3 => 5,              // GL16
        4 => 6,              // GLR16
        5 => 7,              // GLD16
        6 => 4,              // A2
        7 if mark >= 9 => 8, // DU4
        8 => {
            if mark >= 9 {
                9 // GCK16, mark 9+
            } else {
                2 // Clara HD has no GCK16: clean with GC16
            }
        }
        9 => {
            if mark >= 9 {
                10 // GLKW16, mark 9+
            } else {
                6 // Clara HD has no GLKW16: clean with GLR16
            }
        }
        257 => 0x101, // AUTO
        _ => return None,
    };
    // Plato's night mapping below mark 11: GC16 -> GCK16 and GLR16 -> GLKW16
    // where the mark defines them. The Clara HD keeps the day waveform.
    let mode = if inverted && mark >= 9 {
        match mode {
            2 => 9,
            5 | 6 => 10,
            other => other,
        }
    } else {
        mode
    };
    Some(mode)
}

/// Build the request `send_update` submits. Pure, so the ABI fields can be
/// asserted in a host test.
pub(crate) fn update_data(
    model: KoboModel,
    rect: Rect,
    waveform: Waveform,
    full: bool,
    inverted: bool,
    marker: u32,
) -> Option<KoboUpdateDataV2> {
    let mode = waveform_mode(model, waveform, inverted)?;
    let mut flags = 0;
    if inverted {
        flags |= EPDC_FLAG_ENABLE_INVERSION;
    }
    if mode == 1 {
        flags |= EPDC_FLAG_FORCE_MONOCHROME;
    }
    Some(KoboUpdateDataV2 {
        update_region: rect,
        waveform_mode: mode,
        update_mode: if full {
            UPDATE_MODE_FULL
        } else {
            UPDATE_MODE_PARTIAL
        },
        update_marker: marker,
        temp: TEMP_USE_AMBIENT,
        flags,
        dither_mode: 0,
        quant_bit: 0,
        alt_buffer_data: AltBufferDataV2::default(),
    })
}

/// Submit one update in the Kobo mark-7+ layout.
pub(crate) fn send_update(
    file: &File,
    model: KoboModel,
    rect: Rect,
    waveform: Waveform,
    full: bool,
    marker: u32,
    inverted: bool,
) -> io::Result<()> {
    let Some(mut data) = update_data(model, rect, waveform, full, inverted, marker) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Kobo {} has no waveform for {:?}", model.name(), waveform),
        ));
    };
    ioctl(file, MXCFB_SEND_UPDATE_V2, &mut data)
}

/// Wait for an update marker in the Kobo mark-7+ layout.
pub(crate) fn wait_for_update(file: &File, marker: u32) -> io::Result<()> {
    let mut data = KoboMarkerData {
        update_marker: marker,
        collision_test: 0,
    };
    ioctl(file, MXCFB_WAIT_FOR_UPDATE_COMPLETE, &mut data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kobo_update_layout_is_the_drivers_72_byte_v2() {
        use std::mem::offset_of;
        assert_eq!(std::mem::size_of::<KoboUpdateDataV2>(), 72);
        assert_eq!(std::mem::size_of::<KoboMarkerData>(), 8);
        assert_eq!(offset_of!(KoboUpdateDataV2, update_region), 0);
        assert_eq!(offset_of!(KoboUpdateDataV2, waveform_mode), 16);
        assert_eq!(offset_of!(KoboUpdateDataV2, update_mode), 20);
        assert_eq!(offset_of!(KoboUpdateDataV2, update_marker), 24);
        assert_eq!(offset_of!(KoboUpdateDataV2, temp), 28);
        assert_eq!(offset_of!(KoboUpdateDataV2, flags), 32);
        assert_eq!(offset_of!(KoboUpdateDataV2, dither_mode), 36);
        assert_eq!(offset_of!(KoboUpdateDataV2, quant_bit), 40);
        assert_eq!(offset_of!(KoboUpdateDataV2, alt_buffer_data), 44);
        assert_eq!(MXCFB_SEND_UPDATE_V2, 0x4048_462E);
        assert_eq!(MXCFB_WAIT_FOR_UPDATE_COMPLETE, 0xC008_462F);
        // The Kindle K51 request has the same number, but the layouts differ:
        // the Kobo one puts dither_mode/quant_bit where Kindle has history.
        assert_eq!(MXCFB_SEND_UPDATE_V2, 0x4048_462E);
    }

    #[test]
    fn the_kobo_waveforms_use_the_ntx_numbering() {
        for model in KoboModel::ALL {
            assert_eq!(waveform_mode(*model, Waveform::A2, false), Some(4));
            assert_eq!(waveform_mode(*model, Waveform::GL16, false), Some(5));
            assert_eq!(waveform_mode(*model, Waveform::GLR16, false), Some(6));
            assert_eq!(waveform_mode(*model, Waveform::GLD16, false), Some(7));
            assert_eq!(waveform_mode(*model, Waveform::GC16, false), Some(2));
            assert_eq!(waveform_mode(*model, Waveform::DU, false), Some(1));
        }
        // The Clara HD (mark 7) has no GCK16/GLKW16.
        assert_eq!(
            waveform_mode(KoboModel::ClaraHd, Waveform::GCK16, false),
            Some(2)
        );
        assert_eq!(
            waveform_mode(KoboModel::ClaraHd, Waveform::GLKW16, false),
            Some(6)
        );
        // The Libra 2 (mark 9) does.
        assert_eq!(
            waveform_mode(KoboModel::Libra2, Waveform::GCK16, false),
            Some(9)
        );
        assert_eq!(
            waveform_mode(KoboModel::Libra2, Waveform::GLKW16, false),
            Some(10)
        );
        // Night mode uses the mark-9 night waveforms only where they exist.
        assert_eq!(
            waveform_mode(KoboModel::Libra2, Waveform::GC16, true),
            Some(9)
        );
        assert_eq!(
            waveform_mode(KoboModel::ClaraHd, Waveform::GC16, true),
            Some(2)
        );
        // An app waveform that does not exist on Kobo is refused, not passed
        // through as a different mode.
        assert_eq!(
            waveform_mode(KoboModel::ClaraHd, Waveform::GLKW16, false),
            Some(6)
        );
        assert_eq!(
            waveform_mode(KoboModel::ClaraHd, Waveform::AUTO, false),
            Some(257)
        );
        assert_eq!(
            waveform_mode(KoboModel::Libra2, Waveform(7), false),
            Some(8)
        );
        assert_eq!(waveform_mode(KoboModel::ClaraHd, Waveform(7), false), None);
    }

    #[test]
    fn the_kobo_update_carries_mode_flags_and_full_mode() {
        let rect = Rect {
            top: 1,
            left: 2,
            width: 3,
            height: 4,
        };
        let data = update_data(
            KoboModel::Libra2,
            rect,
            Waveform::GLR16,
            true,
            false,
            0x1234,
        )
        .unwrap();
        assert_eq!(data.update_region.top, 1);
        assert_eq!(data.update_region.height, 4);
        assert_eq!(data.waveform_mode, 6);
        assert_eq!(data.update_mode, UPDATE_MODE_FULL);
        assert_eq!(data.update_marker, 0x1234);
        assert_eq!(data.flags, 0);
        assert_eq!(data.flags & EPDC_FLAG_ENABLE_INVERSION, 0);

        let night = update_data(KoboModel::Libra2, rect, Waveform::GC16, true, true, 1).unwrap();
        assert_eq!(night.waveform_mode, 9);
        assert_eq!(night.flags & EPDC_FLAG_ENABLE_INVERSION, 1);
        let monochrome =
            update_data(KoboModel::ClaraHd, rect, Waveform::DU, false, false, 2).unwrap();
        assert_eq!(monochrome.flags, EPDC_FLAG_FORCE_MONOCHROME);
    }

    #[test]
    fn the_kobo_geometry_accepts_both_models_at_8bpp_only() {
        assert_eq!(
            KoboGeometry::validate(KoboModel::ClaraHd, 1072, 1448, 1088, 8, 0)
                .unwrap()
                .width,
            1072
        );
        assert_eq!(
            KoboGeometry::validate(KoboModel::Libra2, 1264, 1680, 1280, 8, 0)
                .unwrap()
                .height,
            1680
        );
        // A real landscape report is accepted, as the driver decides.
        assert!(KoboGeometry::validate(KoboModel::ClaraHd, 1448, 1072, 1448, 8, 0).is_ok());

        let sixteen =
            KoboGeometry::validate(KoboModel::ClaraHd, 1072, 1448, 2176, 16, 0).unwrap_err();
        assert_eq!(sixteen.kind(), io::ErrorKind::Unsupported);
        assert!(sixteen.to_string().contains("16 bpp"));
        assert!(KoboGeometry::validate(KoboModel::Libra2, 1072, 1448, 1088, 8, 0).is_err());
        assert!(KoboGeometry::validate(KoboModel::ClaraHd, 1072, 1448, 1000, 8, 0).is_err());
        assert!(KoboGeometry::validate(KoboModel::ClaraHd, 0, 1448, 1088, 8, 0).is_err());
    }
}
