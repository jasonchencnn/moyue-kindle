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
use std::time::Duration;

use aristotle_screen::model::{self, Model, PageTurnKeys};

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
    /// Kernel timestamp of the report, CLOCK_MONOTONIC microseconds
    /// (comparable with [`monotonic_us`]).
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
    raw_events: u64,
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

impl TouchScreen {
    /// Open the touchscreen and map it onto a `width` x `height` framebuffer.
    ///
    /// `transform` is what the model says the digitizer does. The device's
    /// own axis ranges have the last word on the swap: a digitizer whose X
    /// axis is the longer one describes the panel in its own orientation,
    /// which is not the framebuffer's.
    pub fn open(width: u32, height: u32, transform: Transform) -> io::Result<Self> {
        let (path, name) = find_touchscreen()?;
        let file = File::open(&path)?;
        // Timestamps default to CLOCK_REALTIME; monotonic makes them comparable
        // with `monotonic_us()` for latency measurements.
        // SAFETY: EVIOCSCLOCKID takes a pointer to an int clock id.
        let clock: libc::c_int = libc::CLOCK_MONOTONIC;
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCSCLOCKID as _, &clock) } < 0 {
            return Err(io::Error::last_os_error());
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
            raw_events: 0,
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
        // Timestamps default to CLOCK_REALTIME; monotonic makes them comparable
        // with `monotonic_us()` for latency measurements and gesture deadlines.
        // SAFETY: EVIOCSCLOCKID takes a pointer to an int clock id.
        let clock: libc::c_int = libc::CLOCK_MONOTONIC;
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCSCLOCKID as _, &clock) } < 0 {
            return Err(io::Error::last_os_error());
        }
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
                    let time_us = ev.sec as u64 * 1_000_000 + ev.usec as u64;
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

/// The device's input devices: the touchscreen and the page-turn buttons.
pub struct Input {
    touch: TouchScreen,
    keys: Option<PageKeys>,
    /// Why the page-turn buttons are missing, when they are.
    keys_error: Option<String>,
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
        Ok(Self {
            touch,
            keys,
            keys_error,
        })
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

    /// Reopen all input file descriptors (touchscreen and keys).
    pub fn reopen(&mut self) -> io::Result<()> {
        self.touch.reopen()?;
        if let Some(keys) = &mut self.keys {
            let _ = keys.reopen();
        }
        Ok(())
    }

    /// Poll touch, buttons, power events and worker completions in one wait.
    pub fn poll_with_worker(
        &mut self,
        timeout: Duration,
        other: Option<RawFd>,
        worker: Option<RawFd>,
    ) -> io::Result<Events> {
        let mut fds = [
            libc::pollfd {
                fd: self.touch.file.as_raw_fd(),
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
            libc::pollfd {
                fd: self.keys.as_ref().map_or(-1, |k| k.file.as_raw_fd()),
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
                    Ok(k) => events.keys = k,
                    Err(e) => {
                        eprintln!("aristotle: reading key events failed ({e}), reopening");
                        std::thread::sleep(Duration::from_millis(50));
                        let _ = keys.reopen();
                    }
                }
            }
        }
        Ok(events)
    }
}
