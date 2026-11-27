// SPDX-License-Identifier: AGPL-3.0-or-later
//! Touch input from the evdev touchscreen, and the page-turn buttons.
//!
//! The devices are found by what they can do, not by their `eventN` number:
//! the touchscreen by its multi-touch axes, the buttons by the key codes they
//! report. The names are only a first guess, because they differ between
//! the Kindles (the PW4's Goodix gt9xx and the Oasis 2's i2c controller are
//! called differently).

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use aristotle_screen::model::{self, KoboModel, KoboProtocol, Model, PageTurnKeys};

/// The transform of a touchscreen: the same type as the model table's.
pub use aristotle_screen::model::TouchTransform as Transform;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const ABS_PRESSURE: u16 = 0x18;
const ABS_MT_PRESSURE: u16 = 0x3a;

/// `_IOW('E', 0x90, int)`
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
/// `_IOW('E', 0xa0, int)`: select the clock used for event timestamps.
const EVIOCSCLOCKID: libc::c_ulong = 0x4004_45a0;

/// `EVIOCGMTSLOTS(len)`, `_IOC(_IOC_READ, 'E', 0x0a, len)`: the values of
/// one multi-touch axis in every slot.
const fn eviocgmtslots(len: usize) -> libc::c_ulong {
    0x8000_450a | ((len as libc::c_ulong) << 16)
}

/// `_IOR('E', 0x40 + abs, struct input_absinfo)`
const fn eviocgabs(abs: u16) -> libc::c_ulong {
    0x8018_4540 + abs as libc::c_ulong
}

/// `EVIOCGBIT(ev, len)`, `_IOC(_IOC_READ, 'E', 0x20 + ev, len)`: the bitmap of
/// the event types or codes a device can report.
const fn eviocgbit(ev: u16, len: usize) -> libc::c_ulong {
    0x8000_4500 | ((len as libc::c_ulong) << 16) | (0x20 + ev as libc::c_ulong)
}

/// Touch controllers we know how to drive, matched against evdev names.
/// The list is only a shortcut: any device with multi-touch position axes
/// is a touchscreen.
const TOUCH_NAMES: &[&str] = &["goodix-ts", "cyttsp5_mt", "cyttsp5"];
/// The device the page-turn buttons are on, matched against evdev names.
const PAGE_KEY_NAMES: &[&str] = &["gpio-keys"];
const MAX_SLOTS: usize = 10;
/// The size of the key-code bitmap: `KEY_MAX` is 0x2ff, so 97 bytes cover it.
const KEY_BITS: usize = 128;
/// The size of the event-code bitmap: `ABS_MAX` is 0x3f, so 8 bytes cover it.
const ABS_BITS: usize = 8;

/// Kernel 4.1 `struct input_event` on 32-bit ARM: a 32-bit `timeval`, so it is
/// 16 bytes. Don't use `libc::input_event`: musl's time64 layout is 24 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawEvent {
    sec: u32,
    usec: u32,
    kind: u16,
    code: u16,
    value: i32,
}

impl RawEvent {
    fn time_us(&self, kernel_monotonic: bool) -> u64 {
        if kernel_monotonic {
            self.sec as u64 * 1_000_000 + self.usec as u64
        } else {
            monotonic_us()
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
}

#[derive(Clone, Copy, Debug)]
pub struct TouchEvent {
    pub phase: Phase,
    /// Slot index of the contact.
    pub id: usize,
    pub x: u32,
    pub y: u32,
    pub raw_x: i32,
    pub raw_y: i32,
    /// CLOCK_MONOTONIC microseconds at the report, or at receipt on legacy evdev.
    pub time_us: u64,
}

/// CLOCK_MONOTONIC in microseconds, the clock touch timestamps use.
pub fn monotonic_us() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer for clock_gettime.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1_000
}

#[derive(Clone, Copy, Default)]
struct Slot {
    active: bool,
    was_active: bool,
    dirty: bool,
    x: i32,
    y: i32,
}

/// The single-contact Kobo digitizer state.
///
/// MultiB and MultiC share the position axes but release differently: a
/// MultiB contact that is absent from a report is up (pressure is not a
/// release signal), while MultiC releases when pressure reaches zero. Plato
/// makes the same distinction in `parse_device_events`.
#[derive(Clone, Copy, Debug)]
struct KoboState {
    protocol: KoboProtocol,
    x: i32,
    y: i32,
    active: bool,
    was_active: bool,
    moved: bool,
    frame_position: bool,
}

impl KoboState {
    fn new(protocol: KoboProtocol) -> Self {
        Self {
            protocol,
            x: 0,
            y: 0,
            active: false,
            was_active: false,
            moved: false,
            frame_position: false,
        }
    }

    /// Feed one raw evdev event; returns the touch event a `SYN_REPORT`
    /// completes, if any.
    fn feed(&mut self, kind: u16, code: u16, value: i32) -> Option<(Phase, i32, i32)> {
        match (kind, code) {
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                self.frame_position = true;
                if value >= 0 {
                    if !self.active {
                        self.moved = true;
                    }
                    self.active = true;
                } else {
                    self.active = false;
                }
            }
            (EV_ABS, ABS_MT_POSITION_X) | (EV_ABS, ABS_X) => {
                self.moved |= self.x != value;
                self.x = value;
                self.frame_position = true;
                self.active = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) | (EV_ABS, ABS_Y) => {
                self.moved |= self.y != value;
                self.y = value;
                self.frame_position = true;
                self.active = true;
            }
            (EV_ABS, ABS_MT_PRESSURE) | (EV_ABS, ABS_PRESSURE) => {
                if self.protocol == KoboProtocol::MultiC && value == 0 {
                    self.active = false;
                }
            }
            (EV_KEY, BTN_TOUCH) => {
                self.active = value != 0;
            }
            (EV_SYN, SYN_REPORT) => {
                if self.protocol == KoboProtocol::MultiB && self.active && !self.frame_position {
                    self.active = false;
                }
                let phase = match (self.was_active, self.active) {
                    (false, true) => Some(Phase::Down),
                    (true, true) if self.moved => Some(Phase::Move),
                    (true, false) => Some(Phase::Up),
                    _ => None,
                };
                self.was_active = self.active;
                self.moved = false;
                self.frame_position = false;
                return phase.map(|phase| (phase, self.x, self.y));
            }
            _ => {}
        }
        None
    }
}

pub struct TouchScreen {
    file: File,
    path: String,
    name: String,
    abs_x: AbsInfo,
    abs_y: AbsInfo,
    screen: (u32, u32),
    transform: Transform,
    slots: [Slot; MAX_SLOTS],
    slot: usize,
    grabbed: bool,
    kernel_monotonic: bool,
    raw_events: u64,
    /// Kobo's single-contact protocol; `None` on the Kindle slot protocol.
    kobo: Option<KoboState>,
}

/// The evdev devices of the system, with their names and event nodes.
fn input_devices() -> io::Result<Vec<(String, String)>> {
    let devices = fs::read_to_string("/proc/bus/input/devices")?;
    let mut out = Vec::new();
    for block in devices.split("\n\n") {
        let name = block
            .lines()
            .find_map(|l| l.strip_prefix("N: Name=\""))
            .map(|n| n.trim_end_matches('"').to_string());
        let handler = block
            .lines()
            .find_map(|l| l.strip_prefix("H: Handlers="))
            .and_then(|h| {
                h.split_whitespace()
                    .find(|t| t.starts_with("event"))
                    .map(str::to_string)
            });
        if let (Some(name), Some(handler)) = (name, handler) {
            out.push((name, format!("/dev/input/{handler}")));
        }
    }
    Ok(out)
}

/// Whether the device at `path` reports both multi-touch position axes: a
/// touchscreen, whatever its driver is called.
fn is_touchscreen(path: &str) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut bits = [0u8; ABS_BITS];
    // SAFETY: EVIOCGBIT fills at most `bits.len()` bytes of the bitmap.
    let rc = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            eviocgbit(EV_ABS, bits.len()) as _,
            bits.as_mut_ptr(),
        )
    };
    if rc < 0 {
        return false;
    }
    let has = |code: u16| bits[code as usize / 8] & (1 << (code % 8)) != 0;
    has(ABS_MT_POSITION_X) && has(ABS_MT_POSITION_Y)
}

/// Whether the device at `path` can report the page-turn key codes.
///
/// `EVIOCGBIT(EV_KEY, len)` is the bitmap of the key codes the device
/// supports. `EVIOCGKEY` is not: it is the keys held down right now, and it
/// drops the events waiting in the queue.
fn has_page_keys(path: &str, keys: PageTurnKeys) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut bits = [0u8; KEY_BITS];
    // SAFETY: EVIOCGBIT fills at most `bits.len()` bytes of the bitmap.
    let rc = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            eviocgbit(EV_KEY, bits.len()) as _,
            bits.as_mut_ptr(),
        )
    };
    if rc < 0 {
        return false;
    }
    let has = |code: u16| bits[code as usize / 8] & (1 << (code % 8)) != 0;
    has(keys.forward) && has(keys.back)
}

/// Find the event node of the touchscreen.
pub fn find_touchscreen() -> io::Result<(String, String)> {
    let devices = input_devices()?;
    // The controllers we know by name first.
    for (name, path) in &devices {
        if TOUCH_NAMES.contains(&name.as_str()) {
            return Ok((path.clone(), name.clone()));
        }
    }
    // Then any device with multi-touch position axes: the Oasis 2's i2c
    // controller is called something this list does not have.
    for (name, path) in &devices {
        if is_touchscreen(path) {
            return Ok((path.clone(), name.clone()));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no touchscreen in /proc/bus/input/devices",
    ))
}

/// Find the event node of the page-turn buttons.
pub fn find_page_keys(keys: PageTurnKeys) -> io::Result<(String, String)> {
    let devices = input_devices()?;
    // The device the buttons are on first: on the Oasis 2 it is the gpio-keys
    // device.
    for (name, path) in &devices {
        if PAGE_KEY_NAMES.contains(&name.as_str()) {
            return Ok((path.clone(), name.clone()));
        }
    }
    // Then any device that can report the key codes.
    for (name, path) in &devices {
        if has_page_keys(path, keys) {
            return Ok((path.clone(), name.clone()));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no page-turn buttons in /proc/bus/input/devices",
    ))
}

/// The kernel's value of the multi-touch axis `code` in each slot.
fn mt_slots(file: &File, code: u16) -> io::Result<[i32; MAX_SLOTS]> {
    let mut buf = [0i32; MAX_SLOTS + 1];
    buf[0] = code as i32;
    // SAFETY: EVIOCGMTSLOTS reads the code from the first 4 bytes and fills
    // at most len - 4 bytes of values after it; len is the buffer's size.
    let rc = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            eviocgmtslots(std::mem::size_of_val(&buf)) as _,
            buf.as_mut_ptr(),
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut values = [0; MAX_SLOTS];
    values.copy_from_slice(&buf[1..]);
    Ok(values)
}

fn abs_info(file: &File, axis: u16) -> io::Result<AbsInfo> {
    let mut info = AbsInfo::default();
    // SAFETY: EVIOCGABS fills a 24-byte input_absinfo, which AbsInfo mirrors.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), eviocgabs(axis) as _, &mut info) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

fn configure_touch_clock(file: &File) -> io::Result<bool> {
    let clock: libc::c_int = libc::CLOCK_MONOTONIC;
    // SAFETY: EVIOCSCLOCKID takes a pointer to an int clock id.
    if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCSCLOCKID as _, &clock) } >= 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // Linux 3.0 evdev returns EINVAL for unknown requests.
        Some(libc::EINVAL | libc::ENOTTY) => Ok(false),
        _ => Err(error),
    }
}

impl TouchScreen {
    /// Open the touchscreen and map it onto a `width` x `height` framebuffer.
    ///
    /// `transform` is what the model says the digitizer does. The device's
    /// own axis ranges have the last word on the swap: a digitizer whose X
    /// axis is the longer one describes the panel in its own orientation,
    /// which is not the framebuffer's.
    pub fn open(width: u32, height: u32, transform: Transform) -> io::Result<Self> {
        Self::open_with(width, height, transform, None)
    }

    /// Open a Kobo touchscreen speaking `protocol`.
    pub fn open_kobo(
        width: u32,
        height: u32,
        transform: Transform,
        protocol: KoboProtocol,
    ) -> io::Result<Self> {
        Self::open_with(width, height, transform, Some(KoboState::new(protocol)))
    }

    fn open_with(
        width: u32,
        height: u32,
        transform: Transform,
        kobo: Option<KoboState>,
    ) -> io::Result<Self> {
        let (path, name) = find_touchscreen()?;
        let file = File::open(&path)?;
        let kernel_monotonic = configure_touch_clock(&file)?;
        if !kernel_monotonic {
            eprintln!(
                "aristotle: evdev has no monotonic clock selection; using touch receipt times"
            );
        }
        let (abs_x, abs_y) = match (
            abs_info(&file, ABS_MT_POSITION_X),
            abs_info(&file, ABS_MT_POSITION_Y),
        ) {
            (Ok(x), Ok(y)) if x.maximum > 0 && y.maximum > 0 => (x, y),
            _ => (abs_info(&file, ABS_X)?, abs_info(&file, ABS_Y)?),
        };
        let transform = Transform {
            swap_xy: transform.swap_xy
                || model::touch_transform(width, height, abs_x.maximum, abs_y.maximum).swap_xy,
            ..transform
        };
        // The input core drops a value equal to the last one it passed on
        // for that slot, before we opened the device too: a first touch at
        // the same x (or y) as the last touch before the app started arrives
        // without it. Start every slot where the kernel says it is. For
        // multi-touch axes that takes EVIOCGMTSLOTS: the kernel keeps their
        // values per slot, and EVIOCGABS reports one it never updates (seen
        // on the device: a first tap at 950,800, where the app before had
        // its last tap, opened the reader's menu).
        let slot = abs_info(&file, ABS_MT_SLOT)
            .map_or(0, |s| s.value.clamp(0, MAX_SLOTS as i32 - 1) as usize);
        let mut slots = [Slot::default(); MAX_SLOTS];
        match (
            mt_slots(&file, ABS_MT_POSITION_X),
            mt_slots(&file, ABS_MT_POSITION_Y),
        ) {
            (Ok(xs), Ok(ys)) => {
                for (s, (x, y)) in slots.iter_mut().zip(xs.into_iter().zip(ys)) {
                    s.x = x;
                    s.y = y;
                }
            }
            _ => {
                slots[slot].x = abs_x.value;
                slots[slot].y = abs_y.value;
            }
        }
        Ok(Self {
            file,
            path,
            name,
            abs_x,
            abs_y,
            screen: (width, height),
            transform,
            slots,
            slot,
            grabbed: false,
            kernel_monotonic,
            raw_events: 0,
            kobo,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raw kernel events read so far (diagnostics).
    pub fn raw_events(&self) -> u64 {
        self.raw_events
    }

    pub fn axes(&self) -> (AbsInfo, AbsInfo) {
        (self.abs_x, self.abs_y)
    }

    /// The transform in use: what the model says, plus the swap the device's
    /// own axis ranges call for.
    pub fn transform(&self) -> Transform {
        self.transform
    }

    /// Reopen the touchscreen evdev node and refresh slot states from the kernel.
    pub fn reopen(&mut self) -> io::Result<()> {
        let file = File::open(&self.path)?;
        let kernel_monotonic = configure_touch_clock(&file)?;
        let slot = abs_info(&file, ABS_MT_SLOT)
            .map_or(0, |s| s.value.clamp(0, MAX_SLOTS as i32 - 1) as usize);
        let mut slots = [Slot::default(); MAX_SLOTS];
        match (
            mt_slots(&file, ABS_MT_POSITION_X),
            mt_slots(&file, ABS_MT_POSITION_Y),
        ) {
            (Ok(xs), Ok(ys)) => {
                for (s, (x, y)) in slots.iter_mut().zip(xs.into_iter().zip(ys)) {
                    s.x = x;
                    s.y = y;
                }
            }
            _ => {
                slots[slot].x = self.abs_x.value;
                slots[slot].y = self.abs_y.value;
            }
        }
        let was_grabbed = self.grabbed;
        self.file = file;
        self.kernel_monotonic = kernel_monotonic;
        self.slots = slots;
        self.slot = slot;
        self.grabbed = false;
        if was_grabbed {
            let _ = self.grab(true);
        }
        Ok(())
    }

    /// Take exclusive access so the stock UI stops receiving touches.
    pub fn grab(&mut self, exclusive: bool) -> io::Result<()> {
        // SAFETY: EVIOCGRAB takes an int by value.
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                EVIOCGRAB as _,
                exclusive as libc::c_int,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        self.grabbed = exclusive;
        Ok(())
    }

    fn map(&self, raw_x: i32, raw_y: i32) -> (u32, u32) {
        let (w, h) = self.screen;
        let t = self.transform;
        let (ax, ay, rx, ry) = if t.swap_xy {
            (self.abs_y, self.abs_x, raw_y, raw_x)
        } else {
            (self.abs_x, self.abs_y, raw_x, raw_y)
        };
        let scale = |v: i32, info: AbsInfo, size: u32, mirror: bool| -> u32 {
            let span = (info.maximum - info.minimum).max(1);
            let offset = (v - info.minimum).clamp(0, span);
            // The digitizer reports the panel's own coordinates, so a
            // mirrored axis is mirrored about the digitizer's maximum.
            let offset = if mirror { span - offset } else { offset };
            (offset as i64 * (size as i64 - 1) / span as i64) as u32
        };
        (scale(rx, ax, w, t.mirror_x), scale(ry, ay, h, t.mirror_y))
    }

    /// Wait up to `timeout` for input and return the touch events it produced.
    pub fn poll(&mut self, timeout: Duration) -> io::Result<Vec<TouchEvent>> {
        self.poll_with(timeout, None).map(|(events, _)| events)
    }

    /// [`poll`](Self::poll), also waking when `other` becomes readable (the
    /// powerd events' descriptor, say). Returns the touch events and whether
    /// `other` is readable.
    pub fn poll_with(
        &mut self,
        timeout: Duration,
        other: Option<RawFd>,
    ) -> io::Result<(Vec<TouchEvent>, bool)> {
        self.poll_with_worker(timeout, other, None)
    }

    /// Poll touch, power events and worker completions in one wait.
    pub fn poll_with_worker(
        &mut self,
        timeout: Duration,
        other: Option<RawFd>,
        worker: Option<RawFd>,
    ) -> io::Result<(Vec<TouchEvent>, bool)> {
        let mut fds = [
            libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: other.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: worker.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let count = fds.len() as libc::nfds_t;
        // SAFETY: `count` valid pollfds for the duration of the call.
        let n = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                count,
                if timeout == Duration::MAX {
                    -1
                } else {
                    timeout.as_millis().min(i32::MAX as u128) as i32
                },
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let other_ready = other.is_some() && fds[1].revents & libc::POLLIN != 0;
        if fds[0].revents & libc::POLLIN == 0 {
            return Ok((Vec::new(), other_ready));
        }
        Ok((self.read_events()?, other_ready))
    }

    /// The touch events the device has reported since the last call.
    fn read_events(&mut self) -> io::Result<Vec<TouchEvent>> {
        let mut buf = [0u8; std::mem::size_of::<RawEvent>() * 64];
        let len = self.file.read(&mut buf)?;
        let mut out = Vec::new();
        let (events, _) = buf[..len].as_chunks::<{ std::mem::size_of::<RawEvent>() }>();
        for chunk in events {
            // SAFETY: chunk is exactly one RawEvent; read_unaligned handles alignment.
            let ev: RawEvent = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
            self.raw_events += 1;
            self.feed(ev, &mut out);
        }
        Ok(out)
    }

    fn feed(&mut self, ev: RawEvent, out: &mut Vec<TouchEvent>) {
        if let Some(mut kobo) = self.kobo {
            if let Some((phase, x, y)) = kobo.feed(ev.kind, ev.code, ev.value) {
                let (x, y) = self.map(x, y);
                out.push(TouchEvent {
                    phase,
                    id: 0,
                    x,
                    y,
                    raw_x: kobo.x,
                    raw_y: kobo.y,
                    time_us: ev.time_us(self.kernel_monotonic),
                });
            }
            self.kobo = Some(kobo);
            return;
        }
        match (ev.kind, ev.code) {
            (EV_ABS, ABS_MT_SLOT) => self.slot = (ev.value.max(0) as usize).min(MAX_SLOTS - 1),
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let s = &mut self.slots[self.slot];
                s.active = ev.value >= 0;
                s.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_X) | (EV_ABS, ABS_X) => {
                let s = &mut self.slots[self.slot];
                s.x = ev.value;
                s.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) | (EV_ABS, ABS_Y) => {
                let s = &mut self.slots[self.slot];
                s.y = ev.value;
                s.dirty = true;
            }
            // Some controllers never send TRACKING_ID -1; BTN_TOUCH 0 lifts everything.
            (EV_KEY, BTN_TOUCH) => {
                if ev.value == 0 {
                    for s in self.slots.iter_mut().filter(|s| s.active) {
                        s.active = false;
                        s.dirty = true;
                    }
                } else if !self.slots[self.slot].active {
                    self.slots[self.slot].active = true;
                    self.slots[self.slot].dirty = true;
                }
            }
            (EV_SYN, SYN_REPORT) => {
                let time_us = ev.time_us(self.kernel_monotonic);
                for id in 0..MAX_SLOTS {
                    let s = self.slots[id];
                    if !s.dirty {
                        continue;
                    }
                    let phase = match (s.was_active, s.active) {
                        (false, true) => Phase::Down,
                        (true, true) => Phase::Move,
                        (true, false) => Phase::Up,
                        (false, false) => {
                            self.slots[id].dirty = false;
                            continue;
                        }
                    };
                    let (x, y) = self.map(s.x, s.y);
                    out.push(TouchEvent {
                        phase,
                        id,
                        x,
                        y,
                        raw_x: s.x,
                        raw_y: s.y,
                        time_us,
                    });
                    self.slots[id].was_active = s.active;
                    self.slots[id].dirty = false;
                }
            }
            _ => {}
        }
    }
}

impl Drop for TouchScreen {
    fn drop(&mut self) {
        if self.grabbed {
            let _ = self.grab(false);
        }
    }
}

/// A press of one of the page-turn buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// The key that turns the page forward.
    PageForward,
    /// The key that turns the page back.
    PageBack,
}

/// The page-turn buttons of the device.
///
/// On the Oasis 2 they are the two keys on its left edge: the top one turns
/// the page forward and the bottom one turns it back, the reverse of the
/// other Kindles. Their codes are the model's; see
/// [`aristotle_screen::model::Model::page_turn_keys`].
pub struct PageKeys {
    file: File,
    path: String,
    name: String,
    keys: PageTurnKeys,
}

impl PageKeys {
    /// Open the page-turn buttons of the device.
    pub fn open(keys: PageTurnKeys) -> io::Result<Self> {
        let (path, name) = find_page_keys(keys)?;
        let file = File::open(&path)?;
        Ok(Self {
            file,
            path,
            name,
            keys,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The key codes of the buttons: the one that turns the page forward
    /// and the one that turns it back. The framebuffer's rotation swaps
    /// them, so they say which button is which.
    pub fn codes(&self) -> PageTurnKeys {
        self.keys
    }

    /// Reopen the page-turn button evdev node.
    pub fn reopen(&mut self) -> io::Result<()> {
        let file = File::open(&self.path)?;
        self.file = file;
        Ok(())
    }

    /// The button presses since the last call. Only presses count: the
    /// kernel repeats a held key, and a page turn should not repeat with it.
    pub fn read(&mut self) -> io::Result<Vec<Key>> {
        let mut buf = [0u8; std::mem::size_of::<RawEvent>() * 16];
        let len = self.file.read(&mut buf)?;
        let mut out = Vec::new();
        let (events, _) = buf[..len].as_chunks::<{ std::mem::size_of::<RawEvent>() }>();
        for chunk in events {
            // SAFETY: chunk is exactly one RawEvent; read_unaligned handles alignment.
            let ev: RawEvent = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
            if ev.kind != EV_KEY || ev.value != 1 {
                continue;
            }
            let key = if ev.code == self.keys.forward {
                Some(Key::PageForward)
            } else if ev.code == self.keys.back {
                Some(Key::PageBack)
            } else {
                None
            };
            if let Some(key) = key {
                out.push(key);
            }
        }
        Ok(out)
    }
}

/// Linux input bus IDs for Bluetooth (`BUS_BLUETOOTH`) and virtual/uinput HID (`BUS_VIRTUAL`).
const BUS_BLUETOOTH: u16 = 0x0005;
const BUS_VIRTUAL: u16 = 0x0006;
/// Interval between lightweight `/proc/bus/input/devices` scans for newly
/// connected Bluetooth HID page turners.
const BT_RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Map a Linux `EV_KEY` code from an external Bluetooth HID page turner into a
/// page-turn [`Key`].
///
/// Covers the common HID profiles shipped by commercial Bluetooth page turners,
/// selfie shutters, and mini gamepads (8BitDo Micro, TikTok rings, Kindle clickers).
pub fn map_bluetooth_key(code: u16) -> Option<Key> {
    match code {
        // PageDown, Down, Right, VolumeDown, Space, Enter, KpEnter, NextSong, J
        109 | 108 | 106 | 114 | 57 | 28 | 96 | 163 | 36 => Some(Key::PageForward),
        // PageUp, Up, Left, VolumeUp, Backspace, PreviousSong, K
        104 | 103 | 105 | 115 | 14 | 165 | 37 => Some(Key::PageBack),
        _ => None,
    }
}

/// An opened external Bluetooth HID evdev node.
pub struct BluetoothKeys {
    file: File,
    path: String,
    name: String,
}

impl BluetoothKeys {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn read(&mut self) -> io::Result<Vec<Key>> {
        let mut buf = [0u8; std::mem::size_of::<RawEvent>() * 16];
        let len = self.file.read(&mut buf)?;
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "bluetooth input disconnected",
            ));
        }
        let mut out = Vec::new();
        let (events, _) = buf[..len].as_chunks::<{ std::mem::size_of::<RawEvent>() }>();
        for chunk in events {
            // SAFETY: chunk is exactly one RawEvent; read_unaligned handles alignment.
            let ev: RawEvent = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
            if ev.kind != EV_KEY || ev.value != 1 {
                continue;
            }
            if let Some(key) = map_bluetooth_key(ev.code) {
                out.push(key);
            }
        }
        Ok(out)
    }
}

/// Parse `/proc/bus/input/devices` and return `(name, /dev/input/eventN)` for
/// Bluetooth (`Bus=0005`) or virtual HID (`Bus=0006`) event devices.
fn parse_bluetooth_input_devices(proc_text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for block in proc_text.split("\n\n") {
        let bus = block
            .lines()
            .find_map(|l| l.strip_prefix("I: "))
            .and_then(|rest| {
                rest.split_whitespace()
                    .find_map(|tok| tok.strip_prefix("Bus="))
            })
            .and_then(|hex| u16::from_str_radix(hex, 16).ok());
        if !matches!(bus, Some(BUS_BLUETOOTH | BUS_VIRTUAL)) {
            continue;
        }
        let name = block
            .lines()
            .find_map(|l| l.strip_prefix("N: Name=\""))
            .map(|n| n.trim_end_matches('"').to_string());
        let handler = block
            .lines()
            .find_map(|l| l.strip_prefix("H: Handlers="))
            .and_then(|h| {
                h.split_whitespace()
                    .find(|t| t.starts_with("event"))
                    .map(str::to_string)
            });
        if let (Some(name), Some(handler)) = (name, handler) {
            out.push((name, format!("/dev/input/{handler}")));
        }
    }
    out
}

/// The device's input devices: the touchscreen, built-in page-turn buttons,
/// and any hot-plugged Bluetooth HID page turners.
pub struct Input {
    touch: TouchScreen,
    keys: Option<PageKeys>,
    /// Why the page-turn buttons are missing, when they are.
    keys_error: Option<String>,
    /// Hot-plugged Bluetooth HID input nodes.
    bt_keys: Vec<BluetoothKeys>,
    /// Last time `/proc/bus/input/devices` was checked for Bluetooth HID nodes.
    last_bt_scan: Instant,
}

/// What the input devices produced.
pub struct Events {
    /// The touch events.
    pub touch: Vec<TouchEvent>,
    /// The page-turn button presses.
    pub keys: Vec<Key>,
    /// Whether the other descriptor (powerd's) became readable.
    pub other: bool,
}

impl Input {
    /// Open the touchscreen of `model` and the buttons it has.
    ///
    /// `rotate` is the framebuffer's rotation, read once at startup. The
    /// touchscreen follows it, because the digitizer reports the panel's
    /// own coordinates; see [`Model::touch_transform_at`]. The buttons
    /// follow it too, because holding the device upside down swaps their
    /// places.
    ///
    /// A model with no page-turn buttons has none to open. Buttons that
    /// cannot be found are not fatal: the screen and the touchscreen still
    /// work, and [`Input::keys_error`] says why the buttons are missing.
    pub fn open(model: Model, width: u32, height: u32, rotate: u32) -> io::Result<Self> {
        let touch = TouchScreen::open(width, height, model.touch_transform_at(rotate))?;
        let (keys, keys_error) = match model.page_turn_keys_at(rotate) {
            None => (None, None),
            Some(keys) => match PageKeys::open(keys) {
                Ok(keys) => (Some(keys), None),
                Err(e) => (None, Some(e.to_string())),
            },
        };
        let mut input = Self {
            touch,
            keys,
            keys_error,
            bt_keys: Vec::new(),
            last_bt_scan: Instant::now() - BT_RESCAN_INTERVAL,
        };
        input.refresh_bluetooth_inputs();
        Ok(input)
    }

    /// Open the touchscreen and page-turn buttons of a Kobo `model`.
    ///
    /// The Kobo digitizer protocol (MultiB on the Clara HD, MultiC on the
    /// Libra 2) is selected from the model; the button codes come from the
    /// same table. Rotation handling is the same contract as [`Self::open`],
    /// with Kobo's mark-specific transform.
    pub fn open_kobo(model: KoboModel, width: u32, height: u32, rotate: u32) -> io::Result<Self> {
        let touch = TouchScreen::open_kobo(
            width,
            height,
            model.touch_transform_at(rotate),
            model.touch_protocol(),
        )?;
        let (keys, keys_error) = match model.page_turn_keys() {
            None => (None, None),
            Some(keys) => match PageKeys::open(keys) {
                Ok(keys) => (Some(keys), None),
                Err(e) => (None, Some(e.to_string())),
            },
        };
        let mut input = Self {
            touch,
            keys,
            keys_error,
            bt_keys: Vec::new(),
            last_bt_scan: Instant::now() - BT_RESCAN_INTERVAL,
        };
        input.refresh_bluetooth_inputs();
        Ok(input)
    }

    /// Currently opened Bluetooth HID page-turner devices.
    pub fn bluetooth_keys(&self) -> &[BluetoothKeys] {
        &self.bt_keys
    }

    /// Scan `/proc/bus/input/devices` for newly connected Bluetooth HID
    /// page turners and drop any that have vanished.
    pub fn refresh_bluetooth_inputs(&mut self) {
        self.last_bt_scan = Instant::now();
        let Ok(proc_text) = fs::read_to_string("/proc/bus/input/devices") else {
            return;
        };
        let candidates = parse_bluetooth_input_devices(&proc_text);
        let touch_path = self.touch.path().to_string();
        let keys_path = self.keys.as_ref().map(|k| k.path().to_string());

        // Drop devices that are no longer listed in /proc/bus/input/devices.
        self.bt_keys
            .retain(|opened| candidates.iter().any(|(_, path)| path == &opened.path));

        for (name, path) in candidates {
            if path == touch_path || keys_path.as_deref() == Some(path.as_str()) {
                continue;
            }
            if self.bt_keys.iter().any(|opened| opened.path == path) {
                continue;
            }
            if let Ok(file) = File::open(&path) {
                self.bt_keys.push(BluetoothKeys { file, path, name });
            }
        }
    }

    pub fn touch(&self) -> &TouchScreen {
        &self.touch
    }

    pub fn touch_mut(&mut self) -> &mut TouchScreen {
        &mut self.touch
    }

    /// The page-turn buttons, if this model has any and they were found.
    pub fn keys(&self) -> Option<&PageKeys> {
        self.keys.as_ref()
    }

    /// Why the page-turn buttons are missing, when they are.
    pub fn keys_error(&self) -> Option<&str> {
        self.keys_error.as_deref()
    }

    /// Reopen all input file descriptors (touchscreen, built-in keys, and Bluetooth HID).
    pub fn reopen(&mut self) -> io::Result<()> {
        self.touch.reopen()?;
        if let Some(keys) = &mut self.keys {
            let _ = keys.reopen();
        }
        self.bt_keys.clear();
        self.refresh_bluetooth_inputs();
        Ok(())
    }

    /// Poll touch, buttons, Bluetooth HID devices, power events and worker completions in one wait.
    pub fn poll_with_worker(
        &mut self,
        timeout: Duration,
        other: Option<RawFd>,
        worker: Option<RawFd>,
    ) -> io::Result<Events> {
        if self.last_bt_scan.elapsed() >= BT_RESCAN_INTERVAL {
            self.refresh_bluetooth_inputs();
        }
        let mut fds = Vec::with_capacity(4 + self.bt_keys.len());
        fds.push(libc::pollfd {
            fd: self.touch.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: other.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: worker.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: self.keys.as_ref().map_or(-1, |k| k.file.as_raw_fd()),
            events: libc::POLLIN,
            revents: 0,
        });
        for bt in &self.bt_keys {
            fds.push(libc::pollfd {
                fd: bt.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let count = fds.len() as libc::nfds_t;
        // SAFETY: `count` valid pollfds for the duration of the call.
        let n = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                count,
                if timeout == Duration::MAX {
                    -1
                } else {
                    timeout.as_millis().min(i32::MAX as u128) as i32
                },
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let other_ready = other.is_some() && fds[1].revents & libc::POLLIN != 0;
        let mut events = Events {
            touch: Vec::new(),
            keys: Vec::new(),
            other: other_ready,
        };
        let touch_err = fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0;
        if touch_err {
            eprintln!(
                "aristotle: poll reported error/hangup on touch fd ({:#x}), reopening",
                fds[0].revents
            );
            std::thread::sleep(Duration::from_millis(50));
            if let Err(e) = self.touch.reopen() {
                eprintln!("aristotle: reopening touch fd failed ({e}), backing off 100ms");
                std::thread::sleep(Duration::from_millis(100));
            }
        } else if fds[0].revents & libc::POLLIN != 0 {
            match self.touch.read_events() {
                Ok(t) => events.touch = t,
                Err(e) => {
                    eprintln!("aristotle: reading touch events failed ({e}), reopening");
                    std::thread::sleep(Duration::from_millis(50));
                    let _ = self.touch.reopen();
                }
            }
        }
        if fds[3].revents & libc::POLLIN != 0 {
            if let Some(keys) = self.keys.as_mut() {
                match keys.read() {
                    Ok(k) => events.keys.extend(k),
                    Err(e) => {
                        eprintln!("aristotle: reading key events failed ({e}), reopening");
                        std::thread::sleep(Duration::from_millis(50));
                        let _ = keys.reopen();
                    }
                }
            }
        }
        let mut disconnected = Vec::new();
        for (idx, bt) in self.bt_keys.iter_mut().enumerate() {
            let revents = fds[4 + idx].revents;
            if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                disconnected.push(idx);
            } else if revents & libc::POLLIN != 0 {
                match bt.read() {
                    Ok(k) => events.keys.extend(k),
                    Err(_) => disconnected.push(idx),
                }
            }
        }
        for idx in disconnected.into_iter().rev() {
            self.bt_keys.remove(idx);
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_touch_clock_uses_monotonic_receive_time() {
        let file = File::open("/dev/null").unwrap();
        let kernel_monotonic = configure_touch_clock(&file).unwrap();
        assert!(!kernel_monotonic);
        let event = RawEvent {
            sec: 1_700_000_000,
            usec: 123,
            ..RawEvent::default()
        };
        let before = monotonic_us();
        let received = event.time_us(kernel_monotonic);
        assert!((before..=monotonic_us()).contains(&received));
        assert_eq!(event.time_us(true), 1_700_000_000_000_123);
    }

    fn feed(state: &mut KoboState, events: &[(u16, u16, i32)]) -> Vec<(Phase, i32, i32)> {
        let mut out = Vec::new();
        for &(kind, code, value) in events {
            if let Some(event) = state.feed(kind, code, value) {
                out.push(event);
            }
        }
        out
    }

    #[test]
    fn multib_releases_a_contact_that_is_absent_from_the_report() {
        let mut state = KoboState::new(KoboProtocol::MultiB);
        let down = feed(
            &mut state,
            &[
                (EV_ABS, ABS_MT_TRACKING_ID, 7),
                (EV_ABS, ABS_MT_POSITION_X, 100),
                (EV_ABS, ABS_MT_POSITION_Y, 200),
                (EV_ABS, ABS_MT_PRESSURE, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(down, vec![(Phase::Down, 100, 200)]);

        let moved = feed(
            &mut state,
            &[(EV_ABS, ABS_MT_POSITION_X, 120), (EV_SYN, SYN_REPORT, 0)],
        );
        assert_eq!(moved, vec![(Phase::Move, 120, 200)]);

        // MultiB's pressure is not the release signal: an absent contact is.
        let up = feed(&mut state, &[(EV_SYN, SYN_REPORT, 0)]);
        assert_eq!(up, vec![(Phase::Up, 120, 200)]);
        assert!(!state.active);
    }

    #[test]
    fn a_repeated_tracking_id_is_not_a_move() {
        let mut state = KoboState::new(KoboProtocol::MultiB);
        feed(
            &mut state,
            &[
                (EV_ABS, ABS_MT_TRACKING_ID, 7),
                (EV_ABS, ABS_MT_POSITION_X, 50),
                (EV_ABS, ABS_MT_POSITION_Y, 60),
                (EV_ABS, ABS_MT_PRESSURE, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        let repeated = feed(
            &mut state,
            &[(EV_ABS, ABS_MT_TRACKING_ID, 7), (EV_SYN, SYN_REPORT, 0)],
        );
        assert_eq!(repeated, vec![]);
    }

    #[test]
    fn multib_up_from_a_tracking_id_release() {
        let mut state = KoboState::new(KoboProtocol::MultiB);
        feed(
            &mut state,
            &[
                (EV_ABS, ABS_MT_POSITION_X, 10),
                (EV_ABS, ABS_MT_POSITION_Y, 20),
                (EV_ABS, ABS_MT_PRESSURE, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        let up = feed(
            &mut state,
            &[(EV_ABS, ABS_MT_TRACKING_ID, -1), (EV_SYN, SYN_REPORT, 0)],
        );
        assert_eq!(up, vec![(Phase::Up, 10, 20)]);
    }

    #[test]
    fn multic_releases_a_contact_when_pressure_reaches_zero() {
        let mut state = KoboState::new(KoboProtocol::MultiC);
        let down = feed(
            &mut state,
            &[
                (EV_ABS, ABS_MT_POSITION_X, 300),
                (EV_ABS, ABS_MT_POSITION_Y, 400),
                (EV_ABS, ABS_MT_PRESSURE, 5),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(down, vec![(Phase::Down, 300, 400)]);

        let moved = feed(
            &mut state,
            &[(EV_ABS, ABS_MT_POSITION_Y, 410), (EV_SYN, SYN_REPORT, 0)],
        );
        assert_eq!(moved, vec![(Phase::Move, 300, 410)]);

        let up = feed(
            &mut state,
            &[(EV_ABS, ABS_MT_PRESSURE, 0), (EV_SYN, SYN_REPORT, 0)],
        );
        assert_eq!(up, vec![(Phase::Up, 300, 410)]);
        assert!(!state.active);
    }

    #[test]
    fn multic_ignores_an_empty_frame_but_multib_releases() {
        let mut b = KoboState::new(KoboProtocol::MultiB);
        feed(
            &mut b,
            &[
                (EV_ABS, ABS_MT_POSITION_X, 1),
                (EV_ABS, ABS_MT_POSITION_Y, 2),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(
            feed(&mut b, &[(EV_SYN, SYN_REPORT, 0)]),
            vec![(Phase::Up, 1, 2)]
        );

        let mut c = KoboState::new(KoboProtocol::MultiC);
        feed(
            &mut c,
            &[
                (EV_ABS, ABS_MT_POSITION_X, 1),
                (EV_ABS, ABS_MT_POSITION_Y, 2),
                (EV_ABS, ABS_MT_PRESSURE, 3),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(feed(&mut c, &[(EV_SYN, SYN_REPORT, 0)]), vec![]);
        assert!(c.active);
    }

    #[test]
    fn the_two_kobo_models_pick_their_own_protocol_and_buttons() {
        assert_eq!(KoboModel::ClaraHd.touch_protocol(), KoboProtocol::MultiB);
        assert_eq!(KoboModel::Libra2.touch_protocol(), KoboProtocol::MultiC);
        assert!(KoboModel::ClaraHd.page_turn_keys().is_none());
        assert_eq!(
            KoboModel::Libra2.page_turn_keys(),
            Some(PageTurnKeys {
                forward: 194,
                back: 193,
            })
        );
    }

    #[test]
    fn maps_common_bluetooth_page_turner_keycodes() {
        assert_eq!(map_bluetooth_key(109), Some(Key::PageForward)); // KEY_PAGEDOWN
        assert_eq!(map_bluetooth_key(114), Some(Key::PageForward)); // KEY_VOLUMEDOWN
        assert_eq!(map_bluetooth_key(106), Some(Key::PageForward)); // KEY_RIGHT
        assert_eq!(map_bluetooth_key(28), Some(Key::PageForward)); // KEY_ENTER
        assert_eq!(map_bluetooth_key(104), Some(Key::PageBack)); // KEY_PAGEUP
        assert_eq!(map_bluetooth_key(115), Some(Key::PageBack)); // KEY_VOLUMEUP
        assert_eq!(map_bluetooth_key(105), Some(Key::PageBack)); // KEY_LEFT
        assert_eq!(map_bluetooth_key(1), None); // KEY_ESC
    }

    #[test]
    fn parses_only_bluetooth_and_virtual_hid_devices() {
        let sample = "\
I: Bus=0019 Vendor=0001 Product=0001 Version=0100
N: Name=\"gpio-keys\"
H: Handlers=kbd event0

I: Bus=0005 Vendor=2dc8 Product=9020 Version=0100
N: Name=\"8BitDo Micro Keyboard\"
H: Handlers=sysrq kbd event3

I: Bus=0006 Vendor=0000 Product=0000 Version=0000
N: Name=\"bt-uinput-turner\"
H: Handlers=kbd event4
";
        let found = parse_bluetooth_input_devices(sample);
        assert_eq!(
            found,
            vec![
                (
                    "8BitDo Micro Keyboard".to_string(),
                    "/dev/input/event3".to_string()
                ),
                (
                    "bt-uinput-turner".to_string(),
                    "/dev/input/event4".to_string()
                ),
            ]
        );
    }
}
