// SPDX-License-Identifier: AGPL-3.0-or-later
//! E-ink framebuffer on the i.MX EPDC, using Lab126's update ABI.
//!
//! # Model
//!
//! Drawing and displaying are separate steps on e-ink:
//!
//! 1. Write pixels into the memory-mapped framebuffer ([`Framebuffer::fill`],
//!    [`Framebuffer::line`]). Nothing changes on the panel yet.
//! 2. Ask the EPDC to drive a rectangle to the new content with a chosen
//!    [`Waveform`] ([`Framebuffer::update`]). The call returns in about 1.4 ms;
//!    the panel then takes 144 ms (A2) to 544 ms (GC16) to settle.
//! 3. Optionally block until that update is on the panel ([`Framebuffer::wait`]).
//!
//! Step 2 is less cheap than it looks: the driver runs one update at a time,
//! and submitting while an earlier update is still on the panel blocks until
//! it is done. Interactive code should therefore submit from the display
//! thread in [`crate::display`] and never from its input path. See
//! `docs/ui-framework.md` ("E-ink rendering").
//!
//! Coordinates are framebuffer pixels in portrait orientation: (0, 0) is the
//! top-left corner. The visible area is the driver's own: 1072 × 1448 on the
//! PW4, 1264 × 1680 on the Oasis 2 and 1860 × 2480 on the Scribe. Drivers that
//! report their native landscape `xres`/`yres` are normalized to portrait at
//! open with `aristotle_screen::model::Geometry::normalize`. Gray levels are
//! 8-bit, with 0 black and 255 white.
//!
//! # ABI
//!
//! The update struct layouts come from FBInk's `eink/mxcfb-kindle.h`:
//! `mxcfb_update_data` (72 bytes) on the older Kindles,
//! `mxcfb_update_data_rex` (80 bytes) on the PW4,
//! `mxcfb_update_data_zelda` (88 bytes) on the Oasis 2/3 and
//! `mxcfb_update_data_mtk` (96 bytes) on the PW5 and Scribe. All are checked
//! at compile time below. Plato's Kobo struct has a different field order and
//! size and must not be reused.
//!
//! Which layout a device speaks is read from the device itself, from the
//! serial number in `/proc/usid`; see [`crate::platform`]. The driver answers
//! a request in another layout with EINVAL, so guessing would leave the panel
//! blank.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aristotle_screen::model::KoboModel;

use crate::platform::Platform;

pub(crate) const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOPUT_VSCREENINFO: libc::c_ulong = 0x4601;
pub(crate) const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;
/// `_IOW('F', 0x2E, struct mxcfb_update_data)`, 72-byte payload.
const MXCFB_SEND_UPDATE_K51: libc::c_ulong = 0x4048_462E;
/// `_IOW('F', 0x2E, struct mxcfb_update_data_rex)`, 80-byte payload.
const MXCFB_SEND_UPDATE_REX: libc::c_ulong = 0x4050_462E;
/// `_IOW('F', 0x2E, struct mxcfb_update_data_zelda)`, 88-byte payload.
const MXCFB_SEND_UPDATE_ZELDA: libc::c_ulong = 0x4058_462E;
/// `_IOW('F', 0x2E, struct mxcfb_update_data_mtk)`, 96-byte payload.
const MXCFB_SEND_UPDATE_MTK: libc::c_ulong = 0x4060_462E;
/// `_IOWR('F', 0x2F, struct mxcfb_update_marker_data)`.
const MXCFB_WAIT_FOR_UPDATE_COMPLETE: libc::c_ulong = 0xC008_462F;

/// Which panel backend this framebuffer drives. Kindle models use their EPDC
/// layout; Kobo models use [`crate::kobo`]'s i.MX EPDC layout. The two are
/// dispatched here so the rest of the HAL keeps one `Framebuffer` API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Kindle(Platform),
    Kobo(KoboModel),
}

/// The size an `_IOW` request carries: bits 16 to 29 of its number.
const fn iow_size(request: libc::c_ulong) -> usize {
    ((request >> 16) & 0x3FFF) as usize
}

const UPDATE_MODE_PARTIAL: u32 = 0;
const UPDATE_MODE_FULL: u32 = 1;
const TEMP_USE_AMBIENT: i32 = 0x1000;
/// `TEMP_USE_AUTO` (the same value as `TEMP_USE_PAPYRUS`): the temperature
/// the older K51 driver expects from user space.
const TEMP_USE_AUTO: i32 = 0x1001;
/// `EPDC_FLAG_FORCE_MONOCHROME`: an any-to-2-bit update, used for DU on the
/// K51 and MTK drivers when the pixels are not dithered.
const EPDC_FLAG_FORCE_MONOCHROME: u32 = 0x02;
/// `EPDC_FLAG_ENABLE_INVERSION` from FBInk's `mxcfb-kindle.h`: the PXP
/// inverts the update region on its way from the framebuffer to the panel.
/// FBInk and KOReader report it as working on mxcfb Kindles (`canHWInvert`).
const EPDC_FLAG_ENABLE_INVERSION: u32 = 0x01;

/// The update flags for the current inversion state.
fn update_flags(invert: &AtomicBool) -> u32 {
    if invert.load(Ordering::Relaxed) {
        EPDC_FLAG_ENABLE_INVERSION
    } else {
        0
    }
}

/// EPDC waveform mode: how the panel moves each pixel to its new value.
///
/// The PW4 (Rex) driver uses the Zelda numbering from FBInk's
/// `mxcfb-kindle.h`, not the older PW2 one. Using the PW2 value 4 for A2 on
/// this device silently selects REAGL instead, almost four times slower.
/// Measured times (120 × 120 region): A2 144 ms, DU 309 ms, DU4 344 ms,
/// everything else about 544 ms. See `docs/pw4-reference.md`.
///
/// It is a plain number rather than an enum so experiments can try any mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waveform(pub u32);

impl Waveform {
    /// Clears to white.
    pub const INIT: Self = Self(0x0);
    /// Fast black/white, no flash.
    pub const DU: Self = Self(0x1);
    /// Full 16-level gray with a flash.
    pub const GC16: Self = Self(0x2);
    /// 16-level gray from white, no flash.
    pub const GL16: Self = Self(0x3);
    /// Ghost-compensated text (REAGL).
    pub const GLR16: Self = Self(0x4);
    /// REAGL with dithering.
    pub const GLD16: Self = Self(0x5);
    /// Fastest black/white, for animation and pen strokes.
    pub const A2: Self = Self(0x6);
    /// The stock UI's flashing full refresh on the PW4. FBInk labels it a
    /// night-mode waveform, but the driver log shows stock using it in day
    /// mode for every full clean refresh.
    pub const GCK16: Self = Self(0x8);
    /// The stock reader's page-turn waveform on the PW4, always sent as a
    /// full update of the whole screen. FBInk labels it night mode too.
    /// Clean pages without a flash; about 544 ms.
    pub const GLKW16: Self = Self(0x9);
    /// Let the driver pick from the content.
    pub const AUTO: Self = Self(257);
}

/// A screen rectangle in framebuffer pixels. `#[repr(C)]` because it is also
/// the `mxcfb_rect` passed to the driver, field order included.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub top: u32,
    pub left: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Smallest rectangle covering both.
    pub fn union(self, other: Rect) -> Rect {
        let top = self.top.min(other.top);
        let left = self.left.min(other.left);
        let bottom = (self.top + self.height).max(other.top + other.height);
        let right = (self.left + self.width).max(other.left + other.width);
        Rect {
            top,
            left,
            width: right - left,
            height: bottom - top,
        }
    }
}

/// Submits updates without waiting for each to finish, up to `max_in_flight`
/// outstanding; beyond that the oldest is awaited first.
///
/// This is the measured *baseline*, not the recommended path. The Lab126
/// driver handles one update at a time, and the submit ioctl itself blocks
/// while an earlier update is still on the panel, so a `Presenter` used from
/// an input loop still stalls that loop. Interactive code should use
/// [`crate::display::Display`]. Measurements: `docs/ui-framework.md` ("E-ink rendering").
pub struct Presenter {
    in_flight: std::collections::VecDeque<u32>,
    max_in_flight: usize,
}

impl Presenter {
    pub fn new(max_in_flight: usize) -> Self {
        Self {
            in_flight: Default::default(),
            max_in_flight: max_in_flight.max(1),
        }
    }

    /// Submit `rect`; returns its update marker (`None` if fully off-screen).
    pub fn present(
        &mut self,
        fb: &mut Framebuffer,
        rect: Rect,
        waveform: Waveform,
    ) -> io::Result<Option<u32>> {
        while self.in_flight.len() >= self.max_in_flight {
            let oldest = self.in_flight.pop_front();
            fb.wait(oldest)?;
        }
        let marker = fb.update(rect, waveform, false)?;
        if let Some(m) = marker {
            self.in_flight.push_back(m);
        }
        Ok(marker)
    }

    /// Wait for every submitted update to reach the panel.
    pub fn flush(&mut self, fb: &Framebuffer) -> io::Result<()> {
        while let Some(marker) = self.in_flight.pop_front() {
            fb.wait(Some(marker))?;
        }
        Ok(())
    }
}

/// Accumulates damage between frames so each frame sends one EPDC update.
///
/// Sending one small update per input event made the first touch test lag by
/// tens of seconds: overlapping updates collide in the driver and each waits
/// for the previous one to finish (309 ms per DU update).
#[derive(Default)]
pub struct Damage {
    rect: Option<Rect>,
}

impl Damage {
    pub fn add(&mut self, rect: Rect) {
        self.rect = Some(match self.rect {
            Some(r) => r.union(rect),
            None => rect,
        });
    }

    pub fn take(&mut self) -> Option<Rect> {
        self.rect.take()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AltBufferData {
    phys_addr: u32,
    width: u32,
    height: u32,
    alt_update_region: Rect,
}

/// `mxcfb_update_data_rex`: the update request of the PW4's driver.
#[repr(C)]
#[derive(Default)]
struct UpdateDataRex {
    update_region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    temp: i32,
    flags: u32,
    dither_mode: i32,
    quant_bit: i32,
    alt_buffer_data: AltBufferData,
    hist_bw_waveform_mode: u32,
    hist_gray_waveform_mode: u32,
}

/// `mxcfb_update_data`: the update request of the older Kindle driver, used
/// by the K51 models (Voyage, PW3, Oasis 1 and the KT3). The history modes
/// come before `temp`, unlike in the Rex/Zelda layout.
#[repr(C)]
#[derive(Default)]
struct UpdateDataK51 {
    update_region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    hist_bw_waveform_mode: u32,
    hist_gray_waveform_mode: u32,
    temp: i32,
    flags: u32,
    alt_buffer_data: AltBufferData,
}

/// The swipe-animation fields of the MTK update request. They are ignored
/// unless `MTK_EPDC_FLAG_ENABLE_SWIPE` is set, which this code never does.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SwipeData {
    direction: u32,
    steps: u32,
}

/// `mxcfb_update_data_zelda`: the update request of the Oasis 2's driver.
/// The same fields as the Rex one, plus the two timestamps the driver fills
/// in on its way to the panel.
#[repr(C)]
#[derive(Default)]
struct UpdateDataZelda {
    update_region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    temp: i32,
    flags: u32,
    dither_mode: i32,
    quant_bit: i32,
    alt_buffer_data: AltBufferData,
    hist_bw_waveform_mode: u32,
    hist_gray_waveform_mode: u32,
    ts_pxp: u32,
    ts_epdc: u32,
}

impl UpdateDataZelda {
    /// The same request as `rex`, in the Zelda layout. The timestamps are
    /// the driver's own and stay zero.
    fn from_rex(rex: &UpdateDataRex) -> UpdateDataZelda {
        UpdateDataZelda {
            update_region: rex.update_region,
            waveform_mode: rex.waveform_mode,
            update_mode: rex.update_mode,
            update_marker: rex.update_marker,
            temp: rex.temp,
            flags: rex.flags,
            dither_mode: rex.dither_mode,
            quant_bit: rex.quant_bit,
            alt_buffer_data: rex.alt_buffer_data,
            hist_bw_waveform_mode: rex.hist_bw_waveform_mode,
            hist_gray_waveform_mode: rex.hist_gray_waveform_mode,
            ts_pxp: 0,
            ts_epdc: 0,
        }
    }
}

/// `mxcfb_update_data_mtk`: the update request of the PW5/Scribe driver.
/// The same fields as the Zelda one, with the swipe fields before the
/// history modes.
#[repr(C)]
#[derive(Default)]
struct UpdateDataMtk {
    update_region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    temp: i32,
    flags: u32,
    dither_mode: i32,
    quant_bit: i32,
    alt_buffer_data: AltBufferData,
    swipe_data: SwipeData,
    hist_bw_waveform_mode: u32,
    hist_gray_waveform_mode: u32,
    ts_pxp: u32,
    ts_epdc: u32,
}

// The struct layouts are the driver's ABI: their sizes are fixed, and an
// `_IOW` request number carries the size of its payload, so the two must
// agree.
const _: () = assert!(std::mem::size_of::<UpdateDataK51>() == 72);
const _: () = assert!(std::mem::size_of::<UpdateDataRex>() == 80);
const _: () = assert!(std::mem::size_of::<UpdateDataZelda>() == 88);
const _: () = assert!(std::mem::size_of::<UpdateDataMtk>() == 96);
const _: () = assert!(iow_size(MXCFB_SEND_UPDATE_K51) == std::mem::size_of::<UpdateDataK51>());
const _: () = assert!(iow_size(MXCFB_SEND_UPDATE_REX) == std::mem::size_of::<UpdateDataRex>());
const _: () = assert!(iow_size(MXCFB_SEND_UPDATE_ZELDA) == std::mem::size_of::<UpdateDataZelda>());
const _: () = assert!(iow_size(MXCFB_SEND_UPDATE_MTK) == std::mem::size_of::<UpdateDataMtk>());
const _: () = assert!(MXCFB_SEND_UPDATE_K51 == 0x4048_462E);
const _: () = assert!(MXCFB_SEND_UPDATE_REX == 0x4050_462E);
const _: () = assert!(MXCFB_SEND_UPDATE_ZELDA == 0x4058_462E);
const _: () = assert!(MXCFB_SEND_UPDATE_MTK == 0x4060_462E);

#[repr(C)]
struct UpdateMarkerData {
    update_marker: u32,
    collision_test: u32,
}

#[repr(C)]
#[derive(Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct VarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
pub(crate) struct FixScreenInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

/// Submits EPDC updates from another thread through its own file descriptor.
/// The pixels are whatever is in the shared framebuffer mapping when the
/// driver processes the update, so drawing stays with whoever holds the
/// [`Framebuffer`] while this handle only issues the (blocking) ioctl.
pub struct Updater {
    file: File,
    width: u32,
    height: u32,
    next_marker: u32,
    invert: Arc<AtomicBool>,
    backend: Backend,
}

impl Updater {
    /// Count this handle's update markers from `base`, so two updaters on
    /// two threads never hand out the same marker (a waiter would then wake
    /// for the wrong update). The default base is `0x8000_0000`.
    pub fn with_marker_base(mut self, base: u32) -> Self {
        self.next_marker = base;
        self
    }

    /// Same contract as [`Framebuffer::update`]. Blocks while the driver is
    /// still busy with an earlier update (about one waveform duration,
    /// measured: 120 ms for A2).
    ///
    /// `full` drives every pixel in `rect`, not only the changed ones. That is
    /// what clears ghosting: a partial update leaves pixels whose value did
    /// not change alone, including the faint remains of earlier content.
    pub fn update(
        &mut self,
        rect: Rect,
        waveform: Waveform,
        full: bool,
    ) -> io::Result<Option<u32>> {
        let right = rect.left.saturating_add(rect.width).min(self.width);
        let bottom = rect.top.saturating_add(rect.height).min(self.height);
        if rect.left >= right || rect.top >= bottom {
            return Ok(None);
        }
        let rect = Rect {
            top: rect.top,
            left: rect.left,
            width: right - rect.left,
            height: bottom - rect.top,
        };
        // Keep markers disjoint from the `Framebuffer`'s, which count up from 1.
        let marker = self.next_marker;
        self.next_marker = self.next_marker.wrapping_add(1).max(0x8000_0000);
        let flags = update_flags(&self.invert);
        send_update(
            &self.file,
            self.backend,
            rect,
            waveform,
            full,
            marker,
            flags,
        )?;
        Ok(Some(marker))
    }
}

/// Waits for update completion on its own file descriptor; `Send`, so it can
/// live on a measurement thread.
pub struct UpdateWaiter {
    file: File,
    backend: Backend,
}

impl UpdateWaiter {
    pub fn wait(&self, marker: u32) -> io::Result<()> {
        match self.backend {
            Backend::Kobo(_) => crate::kobo::wait_for_update(&self.file, marker),
            Backend::Kindle(_) => {
                let mut data = UpdateMarkerData {
                    update_marker: marker,
                    collision_test: 0,
                };
                ioctl(&self.file, MXCFB_WAIT_FOR_UPDATE_COMPLETE, &mut data)
            }
        }
    }
}

/// Geometry as reported by the driver. Never assume `stride == width`.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bits_per_pixel: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub rotate: u32,
}

/// The memory-mapped `/dev/fb0` plus the EPDC update interface.
///
/// Not `Send`: it owns a raw mapping. Use [`Framebuffer::waiter`] to wait on
/// updates from another thread.
pub struct Framebuffer {
    file: File,
    map: NonNull<u8>,
    map_len: usize,
    geometry: Geometry,
    id: String,
    next_marker: u32,
    /// Hardware inversion for every update, shared with [`Updater`]s.
    invert: Arc<AtomicBool>,
    /// The update ABI this device speaks.
    backend: Backend,
    restore_grayscale: Option<u32>,
}

/// Preserve valid driver coordinates, including a real landscape layout.
fn runtime_geometry(
    platform: Platform,
    xres: u32,
    yres: u32,
    stride: u32,
) -> io::Result<(u32, u32)> {
    let (width, height) = if platform == Platform::Mtk && xres > stride && yres <= stride {
        (yres, xres)
    } else {
        (xres, yres)
    };
    if width == 0 || height == 0 || width > stride {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "framebuffer dimensions exceed its row stride",
        ));
    }
    Ok((width, height))
}

/// The waveform to send on `platform` for a requested one.
///
/// The Oasis 2's driver dims the frontlight to almost off for a `GCK16`
/// update and ramps it back over about 0.8 s; `GC16` flashes the same way
/// without touching the light. Measured on the Oasis 2 (5.16.2) with
/// FBInk full flashing refreshes, the backlight sampled every 20 ms: GCK16
/// 1114 → 4 → 1114, GC16 and GLKW16 a steady 1114 (H-010). The PW4 keeps
/// `GCK16`, its stock UI's flash.
///
/// The MTK driver has the same split as KOReader's `refresh_mtk`: `GCK16`
/// and `GLKW16` are its night waveforms, while day mode uses `GC16`/`GL16`.
/// Sending the night values in day mode can leave a full-screen update
/// undisplayed, which looks like a blank layer with only small repaints
/// working. The K51 driver uses the older numbering: A2 is 4, GL16 is 5,
/// GLR16 is 8 and GLD16 is 9.
fn device_waveform(platform: Platform, waveform: Waveform) -> Waveform {
    match (platform, waveform) {
        (Platform::Zelda, Waveform::GCK16) => Waveform::GC16,
        (Platform::Mtk, Waveform::GCK16) => Waveform::GC16,
        (Platform::Mtk, Waveform::GLKW16) => Waveform::GL16,
        (Platform::K51, Waveform::A2) => Waveform(4),
        (Platform::K51, Waveform::GL16) => Waveform(5),
        (Platform::K51, Waveform::GLR16) => Waveform(8),
        (Platform::K51, Waveform::GLD16) => Waveform(9),
        (Platform::K51, Waveform::GCK16) => Waveform::GC16,
        (Platform::K51, Waveform::GLKW16) => Waveform(5),
        _ => waveform,
    }
}

/// The history modes the older driver expects alongside `waveform`; REAGL
/// updates ask for the same mode in both, as KOReader's `refresh_k51` does.
fn k51_history(waveform: Waveform) -> (u32, u32) {
    match waveform.0 {
        8 | 9 => (waveform.0, waveform.0),
        2 => (Waveform::DU.0, Waveform::GC16.0),
        // `WAVEFORM_MODE_GC16_FAST` in the older numbering.
        _ => (Waveform::DU.0, 3),
    }
}

/// The history modes the MTK driver expects. As in KOReader's
/// `refresh_mtk`, only GLR16/GLD16 request their own mode in both slots.
fn mtk_history(waveform: Waveform) -> (u32, u32) {
    match waveform.0 {
        4 | 5 => (waveform.0, waveform.0),
        _ => (Waveform::DU.0, Waveform::GC16.0),
    }
}

/// Whether the request must drive every pixel for this platform/waveform.
fn full_update(platform: Platform, waveform: Waveform, full: bool) -> bool {
    // REAGL updates drive every pixel on the K51/MTK drivers.
    full || (platform == Platform::K51 && matches!(waveform.0, 8 | 9))
        || (platform == Platform::Mtk && matches!(waveform.0, 4 | 5))
}

/// The flags for this platform/waveform, on top of the caller's.
fn platform_flags(platform: Platform, waveform: Waveform, flags: u32) -> u32 {
    // An any-to-2-bit DU update needs the monochrome flag when it is not
    // dithered. A2 is left alone: forcing it would crush antialiased glyphs.
    if matches!(platform, Platform::K51 | Platform::Mtk) && waveform == Waveform::DU {
        flags | EPDC_FLAG_FORCE_MONOCHROME
    } else {
        flags
    }
}

/// Ask the EPDC to drive `rect`, in the layout this backend expects.
fn send_update(
    file: &File,
    backend: Backend,
    rect: Rect,
    waveform: Waveform,
    full: bool,
    marker: u32,
    flags: u32,
) -> io::Result<()> {
    match backend {
        Backend::Kobo(model) => crate::kobo::send_update(
            file,
            model,
            rect,
            waveform,
            full,
            marker,
            flags & EPDC_FLAG_ENABLE_INVERSION != 0,
        ),
        Backend::Kindle(platform) => {
            send_update_kindle(file, platform, rect, waveform, full, marker, flags)
        }
    }
}

/// The Kindle EPDC layout.
fn send_update_kindle(
    file: &File,
    platform: Platform,
    rect: Rect,
    waveform: Waveform,
    full: bool,
    marker: u32,
    flags: u32,
) -> io::Result<()> {
    let waveform = device_waveform(platform, waveform);
    let full = full_update(platform, waveform, full);
    let flags = platform_flags(platform, waveform, flags);
    let mut rex = UpdateDataRex {
        update_region: rect,
        waveform_mode: waveform.0,
        update_mode: if full {
            UPDATE_MODE_FULL
        } else {
            UPDATE_MODE_PARTIAL
        },
        update_marker: marker,
        temp: TEMP_USE_AMBIENT,
        flags,
        ..Default::default()
    };
    match platform {
        Platform::K51 => {
            let (hist_bw, hist_gray) = k51_history(waveform);
            let mut k51 = UpdateDataK51 {
                update_region: rect,
                waveform_mode: waveform.0,
                update_mode: rex.update_mode,
                update_marker: marker,
                hist_bw_waveform_mode: hist_bw,
                hist_gray_waveform_mode: hist_gray,
                temp: TEMP_USE_AUTO,
                flags,
                alt_buffer_data: AltBufferData::default(),
            };
            ioctl(file, MXCFB_SEND_UPDATE_K51, &mut k51)
        }
        Platform::Rex => ioctl(file, MXCFB_SEND_UPDATE_REX, &mut rex),
        Platform::Zelda => {
            let mut zelda = UpdateDataZelda::from_rex(&rex);
            ioctl(file, MXCFB_SEND_UPDATE_ZELDA, &mut zelda)
        }
        Platform::Mtk => {
            let (hist_bw, hist_gray) = mtk_history(waveform);
            let mut mtk = UpdateDataMtk {
                update_region: rect,
                waveform_mode: waveform.0,
                update_mode: rex.update_mode,
                update_marker: marker,
                temp: TEMP_USE_AMBIENT,
                flags,
                hist_bw_waveform_mode: hist_bw,
                hist_gray_waveform_mode: hist_gray,
                swipe_data: SwipeData::default(),
                ..Default::default()
            };
            ioctl(file, MXCFB_SEND_UPDATE_MTK, &mut mtk)
        }
        Platform::KoboImx => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Platform::KoboImx is a label; Kobo updates go through crate::kobo",
        )),
    }
}

pub(crate) fn ioctl<T>(file: &File, request: libc::c_ulong, arg: *mut T) -> io::Result<()> {
    // SAFETY: `arg` points to a properly sized, initialised #[repr(C)] value
    // matching `request`, and the fd stays open for the call.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), request as _, arg) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Framebuffer {
    /// Open and map the framebuffer (normally `/dev/fb0`).
    ///
    /// A Kobo is detected from its firmware tag and product name
    /// ([`crate::platform::detect_kobo`]); everything else keeps the Kindle
    /// serial-number detection. Fails unless the driver reports 8 bits per
    /// pixel, the only format this code draws.
    pub fn open(path: &str) -> io::Result<Self> {
        match crate::platform::detect_kobo()? {
            Some(kobo) => Self::open_kobo(path, kobo.model),
            None => Self::open_with(path, Platform::detect()?),
        }
    }

    /// Open the framebuffer for a known Kobo model.
    pub fn open_kobo(path: &str, model: KoboModel) -> io::Result<Self> {
        Self::open_backend(path, Backend::Kobo(model))
    }

    /// Open the framebuffer for a known Kindle platform.
    ///
    /// `Platform::KoboImx` is only a label; Kobo requests are built by
    /// [`crate::kobo`], so it is refused here.
    pub fn open_with(path: &str, platform: Platform) -> io::Result<Self> {
        if platform == Platform::KoboImx {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Platform::KoboImx is a label; use Framebuffer::open_kobo",
            ));
        }
        Self::open_backend(path, Backend::Kindle(platform))
    }

    fn open_backend(path: &str, backend: Backend) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut var = VarScreenInfo::default();
        ioctl(&file, FBIOGET_VSCREENINFO, &mut var)?;
        // SAFETY: plain-old-data struct; all-zero is a valid bit pattern.
        let mut fix: FixScreenInfo = unsafe { std::mem::zeroed() };
        ioctl(&file, FBIOGET_FSCREENINFO, &mut fix)?;
        if var.bits_per_pixel != 8 {
            let detail = match backend {
                Backend::Kobo(model) => format!(
                    "Kobo {} framebuffer reports {} bpp; this build draws 8 bpp only",
                    model.name(),
                    var.bits_per_pixel
                ),
                Backend::Kindle(_) => {
                    format!("expected 8 bpp, driver reports {}", var.bits_per_pixel)
                }
            };
            return Err(io::Error::new(io::ErrorKind::Unsupported, detail));
        }
        let (width, height) = match backend {
            Backend::Kindle(platform) => {
                runtime_geometry(platform, var.xres, var.yres, fix.line_length)?
            }
            Backend::Kobo(model) => {
                let g = crate::kobo::KoboGeometry::validate(
                    model,
                    var.xres,
                    var.yres,
                    fix.line_length,
                    var.bits_per_pixel,
                    var.rotate,
                )?;
                (g.width, g.height)
            }
        };
        eprintln!(
            "aristotle: framebuffer {:?} {}x{} stride={} bpp={} rotate={} grayscale={}",
            backend,
            var.xres,
            var.yres,
            fix.line_length,
            var.bits_per_pixel,
            var.rotate,
            var.grayscale
        );
        let map_len = fix.smem_len as usize;
        let visible_len = u64::from(fix.line_length) * u64::from(height);
        if u64::from(fix.smem_len) < visible_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "framebuffer maps {} bytes, needs {visible_len} for {width}x{height}",
                    fix.smem_len
                ),
            ));
        }
        // SAFETY: mapping the device's own framebuffer memory with its reported length.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let id_len = fix.id.iter().position(|&b| b == 0).unwrap_or(fix.id.len());
        let mut framebuffer = Self {
            file,
            map: NonNull::new(ptr.cast()).expect("mmap returned null"),
            map_len,
            geometry: Geometry {
                width,
                height,
                stride: fix.line_length,
                bits_per_pixel: var.bits_per_pixel,
                x_offset: var.xoffset,
                y_offset: var.yoffset,
                rotate: var.rotate,
            },
            id: String::from_utf8_lossy(&fix.id[..id_len]).into_owned(),
            next_marker: 1,
            invert: Arc::new(AtomicBool::new(false)),
            backend,
            restore_grayscale: None,
        };
        if backend == Backend::Kindle(Platform::Mtk) {
            if var.grayscale > 2 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "unsupported MTK framebuffer grayscale mode {}",
                        var.grayscale
                    ),
                ));
            }
            if var.grayscale == 2 {
                // The app renders night mode itself; MTK's driver night mode omits GL16/DU.
                framebuffer.restore_grayscale = Some(var.grayscale);
                var.grayscale = 1;
                ioctl(&framebuffer.file, FBIOPUT_VSCREENINFO, &mut var)?;
                let mut current = VarScreenInfo::default();
                ioctl(&framebuffer.file, FBIOGET_VSCREENINFO, &mut current)?;
                if current.grayscale != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "MTK driver did not enable grayscale day waveforms",
                    ));
                }
                eprintln!("aristotle: MTK driver grayscale 2 -> 1; restoring on exit");
            }
        }
        Ok(framebuffer)
    }

    /// A handle that can wait for update markers on another thread, to measure
    /// when pixels actually reach the panel without blocking the render loop.
    pub fn waiter(&self) -> io::Result<UpdateWaiter> {
        Ok(UpdateWaiter {
            file: self.file.try_clone()?,
            backend: self.backend,
        })
    }

    /// A handle that submits updates from another thread; see [`Updater`].
    pub fn updater(&self) -> io::Result<Updater> {
        Ok(Updater {
            file: self.file.try_clone()?,
            width: self.geometry.width,
            height: self.geometry.height,
            next_marker: 0x8000_0000,
            invert: self.invert.clone(),
            backend: self.backend,
        })
    }

    /// The update ABI this device speaks. Kobo panels report
    /// [`Platform::KoboImx`], which is a label: their request layout is built
    /// by [`crate::kobo`].
    pub fn platform(&self) -> Platform {
        match self.backend {
            Backend::Kindle(platform) => platform,
            Backend::Kobo(_) => Platform::KoboImx,
        }
    }

    /// Hardware night mode: from now on every update, from this handle and
    /// from every [`Updater`] made from it, asks the PXP to invert the region
    /// on its way to the panel (`EPDC_FLAG_ENABLE_INVERSION`). The
    /// framebuffer keeps day pixels, so screenshots of `/dev/fb0` show the
    /// day picture. The inversion only reaches the panel with the next
    /// update of each region, so follow a switch with a full update of the
    /// whole screen.
    pub fn set_hw_invert(&self, on: bool) {
        self.invert.store(on, Ordering::Relaxed);
    }

    pub fn hw_invert(&self) -> bool {
        self.invert.load(Ordering::Relaxed)
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Driver identifier, `mxc_epdc_fb` on the PW4.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `rows` rows of the visible framebuffer from `top`, each
    /// [`Geometry::stride`] bytes: what the panel will show for that band.
    /// Empty when the range is outside the screen.
    ///
    /// The mapping is shared with the driver, so this is what is on the
    /// panel, not a copy frozen at open time.
    pub fn rows(&self, top: u32, rows: u32) -> &[u8] {
        let g = self.geometry;
        if top >= g.height || rows == 0 {
            return &[];
        }
        let rows = rows.min(g.height - top) as usize;
        let start = (top + g.y_offset) as usize * g.stride as usize + g.x_offset as usize;
        let len = rows * g.stride as usize;
        if start + len > self.map_len {
            return &[];
        }
        // SAFETY: `start` and `len` are inside the read-only view of the
        // mapping checked above; no Rust reference aliases it.
        unsafe { std::slice::from_raw_parts(self.map.as_ptr().add(start), len) }
    }

    /// Fill a rectangle with one gray level (0 = black, 255 = white).
    pub fn fill(&mut self, rect: Rect, gray: u8) {
        let g = self.geometry;
        let bottom = (rect.top + rect.height).min(g.height);
        let right = (rect.left + rect.width).min(g.width);
        if rect.left >= right {
            return;
        }
        for y in rect.top..bottom {
            let row = ((y + g.y_offset) * g.stride + rect.left + g.x_offset) as usize;
            let len = (right - rect.left) as usize;
            if row + len > self.map_len {
                break;
            }
            // SAFETY: bounds checked against the mapping length above.
            unsafe { std::ptr::write_bytes(self.map.as_ptr().add(row), gray, len) };
        }
    }

    /// Copy an 8-bit image into the framebuffer at `rect`. `src` holds
    /// `rect.height` rows of `src_stride` bytes; the first `rect.width` bytes
    /// of each row are used. Clipped to the screen. One memcpy per row, so a
    /// full page takes well under a millisecond.
    pub fn blit(&mut self, src: &[u8], src_stride: usize, rect: Rect) {
        self.blit_rows(src, src_stride, rect, false);
    }

    /// [`Framebuffer::blit`] with every pixel inverted (`255 - v`), for
    /// software night mode. The inversion is fused into the copy, which is
    /// bound by memory bandwidth, so it costs about the same as a plain blit.
    pub fn blit_inverted(&mut self, src: &[u8], src_stride: usize, rect: Rect) {
        self.blit_rows(src, src_stride, rect, true);
    }

    fn blit_rows(&mut self, src: &[u8], src_stride: usize, rect: Rect, invert: bool) {
        let Some(clipped) = self.clip(rect) else {
            return;
        };
        let g = self.geometry;
        let width = clipped.width as usize;
        for row in 0..clipped.height as usize {
            let src_off = (clipped.top - rect.top) as usize * src_stride
                + row * src_stride
                + (clipped.left - rect.left) as usize;
            let dst_off = ((clipped.top as usize + row + g.y_offset as usize) * g.stride as usize)
                + clipped.left as usize
                + g.x_offset as usize;
            if src_off + width > src.len() || dst_off + width > self.map_len {
                break;
            }
            let from = &src[src_off..src_off + width];
            // SAFETY: the destination range was bounds-checked just above,
            // and the mapping is not aliased by any Rust reference.
            let to =
                unsafe { std::slice::from_raw_parts_mut(self.map.as_ptr().add(dst_off), width) };
            if invert {
                invert_into(to, from);
            } else {
                to.copy_from_slice(from);
            }
        }
    }

    /// Clip `rect` to the visible screen. The driver rejects regions that
    /// extend past the panel with EINVAL.
    pub fn clip(&self, rect: Rect) -> Option<Rect> {
        let g = self.geometry;
        let right = (rect.left.saturating_add(rect.width)).min(g.width);
        let bottom = (rect.top.saturating_add(rect.height)).min(g.height);
        (rect.left < right && rect.top < bottom).then(|| Rect {
            top: rect.top,
            left: rect.left,
            width: right - rect.left,
            height: bottom - rect.top,
        })
    }

    /// Ask the EPDC to refresh `rect` (clipped to the screen). Returns the
    /// update marker, or `None` when nothing of `rect` is visible. Takes about
    /// 1 ms when the panel is idle, but blocks for the rest of the current
    /// update when it is not.
    ///
    /// `full` selects a full update, where every pixel in `rect` is driven
    /// even if unchanged (with GC16 this is the flashing "clean" refresh).
    /// Partial updates only drive pixels whose value changed. Temperature
    /// compensation uses the panel's ambient sensor.
    pub fn update(
        &mut self,
        rect: Rect,
        waveform: Waveform,
        full: bool,
    ) -> io::Result<Option<u32>> {
        let Some(rect) = self.clip(rect) else {
            return Ok(None);
        };
        let marker = self.next_marker;
        self.next_marker = self.next_marker.wrapping_add(1).max(1);
        let flags = update_flags(&self.invert);
        send_update(
            &self.file,
            self.backend,
            rect,
            waveform,
            full,
            marker,
            flags,
        )?;
        Ok(Some(marker))
    }

    /// Block until the update with `marker` has been displayed.
    pub fn wait(&self, marker: Option<u32>) -> io::Result<()> {
        let Some(marker) = marker else { return Ok(()) };
        match self.backend {
            Backend::Kobo(_) => crate::kobo::wait_for_update(&self.file, marker),
            Backend::Kindle(_) => {
                let mut data = UpdateMarkerData {
                    update_marker: marker,
                    collision_test: 0,
                };
                ioctl(&self.file, MXCFB_WAIT_FOR_UPDATE_COMPLETE, &mut data)
            }
        }
    }

    /// Draw a thick line by stamping squares of half-width `r` along it, spaced
    /// `r / 2` apart so the stroke has no gaps. Returns the damaged rectangle
    /// for the caller to pass to [`Framebuffer::update`] or a [`Presenter`].
    pub fn line(&mut self, from: (u32, u32), to: (u32, u32), r: u32, gray: u8) -> Rect {
        let (x0, y0) = (from.0 as i64, from.1 as i64);
        let (x1, y1) = (to.0 as i64, to.1 as i64);
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        let stride = (r as i64 / 2).max(1);
        let mut damage: Option<Rect> = None;
        let mut i = 0;
        loop {
            let t = i.min(steps);
            let x = (x0 + (x1 - x0) * t / steps) as u32;
            let y = (y0 + (y1 - y0) * t / steps) as u32;
            let dot = Rect {
                top: y.saturating_sub(r),
                left: x.saturating_sub(r),
                width: 2 * r,
                height: 2 * r,
            };
            self.fill(dot, gray);
            damage = Some(damage.map_or(dot, |d| d.union(dot)));
            if t == steps {
                break;
            }
            i += stride;
        }
        damage.expect("at least one dot")
    }

    /// The whole visible screen.
    pub fn full_screen(&self) -> Rect {
        Rect {
            top: 0,
            left: 0,
            width: self.geometry.width,
            height: self.geometry.height,
        }
    }

    /// Verify the kernel framebuffer's `var.rotate` still matches `expected`
    /// (the startup rotation bound to the touchscreen digitizer). If the
    /// native Kindle accelerometer/powerd changed `var.rotate` (for example on
    /// sleep wakeup), force it back via `FBIOPUT_VSCREENINFO` and return
    /// `Ok(true)` so the caller can repaint cleanly.
    pub fn ensure_rotate(&mut self, expected: u32) -> io::Result<bool> {
        let mut var = VarScreenInfo::default();
        ioctl(&self.file, FBIOGET_VSCREENINFO, &mut var)?;
        if var.rotate == expected {
            return Ok(false);
        }
        eprintln!(
            "aristotle: kernel fb0 rotate drifted {} -> {}; forcing back to {}",
            expected, var.rotate, expected
        );
        var.rotate = expected;
        ioctl(&self.file, FBIOPUT_VSCREENINFO, &mut var)?;
        Ok(true)
    }
}

/// `dst[i] = 255 - src[i]`; a plain loop the compiler vectorizes (NEON on
/// the PW4).
pub fn invert_into(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = !*s;
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the region mapped in `open`.
        unsafe { libc::munmap(self.map.as_ptr().cast(), self.map_len) };
        if let Some(grayscale) = self.restore_grayscale {
            let mut current = VarScreenInfo::default();
            let restored = ioctl(&self.file, FBIOGET_VSCREENINFO, &mut current).and_then(|()| {
                current.grayscale = grayscale;
                ioctl(&self.file, FBIOPUT_VSCREENINFO, &mut current)
            });
            if let Err(error) = restored {
                eprintln!("aristotle: could not restore MTK grayscale mode: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_kobo_label_is_not_a_kindle_opener() {
        let result = Framebuffer::open_with("/dev/null", Platform::KoboImx);
        assert!(matches!(
            result,
            Err(ref error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn runtime_geometry_preserves_landscape_and_limits_the_mtk_workaround() {
        use super::*;
        assert_eq!(
            runtime_geometry(Platform::Mtk, 1860, 2480, 1872).unwrap(),
            (1860, 2480)
        );
        assert_eq!(
            runtime_geometry(Platform::Mtk, 2480, 1860, 2496).unwrap(),
            (2480, 1860)
        );
        assert_eq!(
            runtime_geometry(Platform::Mtk, 2480, 1860, 1872).unwrap(),
            (1860, 2480)
        );
        assert!(runtime_geometry(Platform::Rex, 2480, 1860, 1872).is_err());
        assert!(runtime_geometry(Platform::Mtk, 0, 1860, 1872).is_err());
    }

    #[test]
    fn the_oasis_flashes_with_gc16_to_leave_the_light_alone() {
        use super::*;
        assert_eq!(
            device_waveform(Platform::Zelda, Waveform::GCK16),
            Waveform::GC16
        );
        assert_eq!(
            device_waveform(Platform::Rex, Waveform::GCK16),
            Waveform::GCK16
        );
        assert_eq!(
            device_waveform(Platform::Zelda, Waveform::GLKW16),
            Waveform::GLKW16
        );
    }

    use super::*;

    #[test]
    fn inversion_flag_follows_the_switch() {
        let invert = AtomicBool::new(false);
        assert_eq!(update_flags(&invert), 0);
        invert.store(true, Ordering::Relaxed);
        assert_eq!(update_flags(&invert), EPDC_FLAG_ENABLE_INVERSION);
    }

    #[test]
    fn invert_into_maps_every_level() {
        let src: Vec<u8> = (0..=255).collect();
        let mut dst = vec![0; 256];
        invert_into(&mut dst, &src);
        assert!(dst.iter().enumerate().all(|(i, &v)| v as usize == 255 - i));
    }

    #[test]
    fn the_mtk_day_waveforms_do_not_use_the_night_modes() {
        assert_eq!(
            device_waveform(Platform::Mtk, Waveform::GCK16),
            Waveform::GC16
        );
        assert_eq!(
            device_waveform(Platform::Mtk, Waveform::GLKW16),
            Waveform::GL16
        );
        assert_eq!(
            device_waveform(Platform::Mtk, Waveform::GL16),
            Waveform::GL16
        );
        assert_eq!(device_waveform(Platform::Mtk, Waveform::DU), Waveform::DU);
        assert_eq!(device_waveform(Platform::Mtk, Waveform::A2), Waveform::A2);
    }

    #[test]
    fn the_k51_waveforms_use_the_older_numbering() {
        assert_eq!(device_waveform(Platform::K51, Waveform::A2), Waveform(4));
        assert_eq!(device_waveform(Platform::K51, Waveform::GL16), Waveform(5));
        assert_eq!(device_waveform(Platform::K51, Waveform::GLR16), Waveform(8));
        assert_eq!(device_waveform(Platform::K51, Waveform::GLD16), Waveform(9));
        assert_eq!(
            device_waveform(Platform::K51, Waveform::GCK16),
            Waveform::GC16
        );
        assert_eq!(
            device_waveform(Platform::K51, Waveform::GLKW16),
            Waveform(5)
        );
    }

    #[test]
    fn the_reagl_updates_are_full_and_du_is_monochrome() {
        assert!(full_update(Platform::Mtk, Waveform(4), false));
        assert!(full_update(Platform::Mtk, Waveform(5), false));
        assert!(full_update(Platform::K51, Waveform(8), false));
        assert!(full_update(Platform::K51, Waveform(9), false));
        assert!(!full_update(Platform::Mtk, Waveform::GL16, false));
        assert!(full_update(Platform::Rex, Waveform::GC16, true));

        assert_eq!(
            platform_flags(Platform::Mtk, Waveform::DU, 0),
            EPDC_FLAG_FORCE_MONOCHROME
        );
        assert_eq!(
            platform_flags(Platform::K51, Waveform::DU, 0),
            EPDC_FLAG_FORCE_MONOCHROME
        );
        assert_eq!(platform_flags(Platform::Rex, Waveform::DU, 0), 0);
        assert_eq!(platform_flags(Platform::Mtk, Waveform::A2, 0), 0);
    }

    #[test]
    fn the_k51_and_mtk_history_modes_match_koreader() {
        assert_eq!(k51_history(Waveform(8)), (8, 8));
        assert_eq!(k51_history(Waveform(9)), (9, 9));
        assert_eq!(k51_history(Waveform::GC16), (Waveform::DU.0, 2));
        assert_eq!(k51_history(Waveform(5)), (Waveform::DU.0, 3));

        assert_eq!(mtk_history(Waveform(4)), (4, 4));
        assert_eq!(mtk_history(Waveform(5)), (5, 5));
        assert_eq!(mtk_history(Waveform(3)), (Waveform::DU.0, 2));
    }

    #[test]
    fn the_update_layouts_have_the_driver_sizes() {
        assert_eq!(std::mem::size_of::<UpdateDataK51>(), 72);
        assert_eq!(std::mem::size_of::<UpdateDataRex>(), 80);
        assert_eq!(std::mem::size_of::<UpdateDataZelda>(), 88);
        assert_eq!(std::mem::size_of::<UpdateDataMtk>(), 96);
        assert_eq!(iow_size(MXCFB_SEND_UPDATE_K51), 72);
        assert_eq!(iow_size(MXCFB_SEND_UPDATE_REX), 80);
        assert_eq!(iow_size(MXCFB_SEND_UPDATE_ZELDA), 88);
        assert_eq!(iow_size(MXCFB_SEND_UPDATE_MTK), 96);
        // The Kindle Carta wait, used by the K51/Rex/Zelda/MTK branches.
        assert_eq!(MXCFB_WAIT_FOR_UPDATE_COMPLETE, 0xC008_462F);
        assert_eq!(std::mem::size_of::<UpdateMarkerData>(), 8);
    }
}
