// SPDX-License-Identifier: AGPL-3.0-or-later
//! System facts for the settings screen: storage, memory, firmware, local
//! time, and the USB mode of the device agent.
//!
//! Everything here is read-only except [`set_usb_mode`] and [`net_allow`],
//! which ask the agent (`device/extension/agent/`) through its own
//! `aristotlectl`, the same command a developer runs over SSH.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// Process-wide fallback clock offset (seconds) when the platform denies
/// writing the system clock / RTC. Every clock read in the app adds this
/// offset so the status bar, home clock, reader bars, and settings stay in
/// sync.
static CLOCK_OFFSET_SECS: AtomicI64 = AtomicI64::new(0);

/// Process-wide 24-hour clock preference (default: true).
static CLOCK_24H: AtomicBool = AtomicBool::new(true);

/// Set the process-wide fallback clock offset in seconds.
pub fn set_clock_offset(offset_secs: i64) {
    CLOCK_OFFSET_SECS.store(offset_secs, Ordering::Relaxed);
}

/// Current process-wide fallback clock offset in seconds.
pub fn clock_offset() -> i64 {
    CLOCK_OFFSET_SECS.load(Ordering::Relaxed)
}

/// Set the process-wide 24-hour clock preference.
pub fn set_clock_24h(clock_24h: bool) {
    CLOCK_24H.store(clock_24h, Ordering::Relaxed);
}

/// Current process-wide 24-hour clock preference.
pub fn clock_24h() -> bool {
    CLOCK_24H.load(Ordering::Relaxed)
}

/// Raw system UTC UNIX timestamp in seconds (without the fallback offset).
pub fn raw_unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Effective UTC UNIX timestamp in seconds, including the process-wide
/// fallback offset. Because the kernel `CLOCK_REALTIME` advances from the
/// hardware RTC across CPU suspend, this clock continues advancing while
/// the device is asleep.
pub fn unix_now() -> i64 {
    raw_unix_now().saturating_add(clock_offset())
}

/// Size and free space of a filesystem, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storage {
    pub total: u64,
    /// Free for unprivileged users (`f_bavail`), what a copy can use.
    pub free: u64,
}

/// `statvfs` of the filesystem holding `path` (the userstore is `/mnt/us`).
pub fn storage(path: &Path) -> io::Result<Storage> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: statvfs only writes into the struct passed; all-zero is valid.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` lives for the call.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let block = st.f_frsize as u64;
    Ok(Storage {
        total: st.f_blocks as u64 * block,
        free: st.f_bavail as u64 * block,
    })
}

/// Memory from `/proc/meminfo`, in kilobytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Memory {
    pub total_kb: u64,
    pub available_kb: u64,
}

pub fn memory() -> io::Result<Memory> {
    Ok(parse_meminfo(&fs::read_to_string("/proc/meminfo")?))
}

fn parse_meminfo(s: &str) -> Memory {
    let mut m = Memory::default();
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(v)) = (it.next(), it.next()) else {
            continue;
        };
        let v = v.parse().unwrap_or(0);
        match key {
            "MemTotal:" => m.total_kb = v,
            "MemAvailable:" => m.available_kb = v,
            _ => {}
        }
    }
    m
}

/// The stock firmware version, e.g. "5.13.6", from `/etc/prettyversion.txt`
/// ("Kindle 5.13.6 (4110410039) ..." on firmware 5.x, per KOReader's Kindle
/// support [inference for our unit]); `None` off the device.
pub fn firmware_version() -> Option<String> {
    let s = fs::read_to_string("/etc/prettyversion.txt").ok()?;
    parse_pretty_version(&s)
}

fn parse_pretty_version(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))
        .map(String::from)
}

/// Broken-down local time. musl reads the zone from `TZ` or `/etc/localtime`
/// (a link to `/var/local/system/tz` on the Kindle).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct LocalTime {
    pub year: i32,
    /// 1 to 12.
    pub month: u32,
    pub day: u32,
    /// 0 is Sunday, as in C.
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    /// The second inside the minute, 0 to 59.
    pub second: u32,
}

/// Sets `TZ` from `/etc/localtime` when musl cannot read that file itself.
///
/// The Kindle's zone file (measured on the device) is TZif2 with one
/// local time type and an empty POSIX footer. musl then
/// falls back to UTC, so this turns the single offset into a `TZ` string.
/// Call it at the start of `main`, before any other thread exists.
pub fn init_timezone() {
    if std::env::var_os("TZ").is_some() {
        return;
    }
    let Ok(data) = std::fs::read("/etc/localtime") else {
        return;
    };
    if let Some(tz) = tz_from_tzif(&data) {
        std::env::set_var("TZ", tz);
    }
}

/// Builds a POSIX `TZ` value from a TZif file that musl would misread:
/// the footer is empty and the zone has one local time type. Returns `None` when
/// musl can handle the file or it is malformed.
fn tz_from_tzif(data: &[u8]) -> Option<String> {
    fn be32(d: &[u8], at: usize) -> Option<usize> {
        let b = d.get(at..at + 4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    }
    if data.get(..4)? != b"TZif" {
        return None;
    }
    let (isutc, isstd, leap, timecnt, typecnt, charcnt) = (
        be32(data, 20)?,
        be32(data, 24)?,
        be32(data, 28)?,
        be32(data, 32)?,
        be32(data, 36)?,
        be32(data, 40)?,
    );
    let v1 = 44 + timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isutc;
    if data.get(4).copied().unwrap_or(0) >= b'2' {
        let h = data.get(v1..)?;
        if h.get(..4)? != b"TZif" {
            return None;
        }
        let (isutc, isstd, leap, timecnt, typecnt, charcnt) = (
            be32(h, 20)?,
            be32(h, 24)?,
            be32(h, 28)?,
            be32(h, 32)?,
            be32(h, 36)?,
            be32(h, 40)?,
        );
        let types = 44 + timecnt * 9;
        let end = types + typecnt * 6 + charcnt + leap * 12 + isstd + isutc;
        let footer = h.get(end..)?;
        let footer = std::str::from_utf8(footer).ok()?.trim_matches('\n');
        if !footer.is_empty() || typecnt != 1 {
            return None;
        }
        return posix_offset(be32(h, types)? as i32);
    }
    if typecnt != 1 {
        return None;
    }
    posix_offset(be32(data, 44 + timecnt * 5)? as i32)
}

/// Formats a UTC offset in seconds east as a POSIX `TZ`, e.g. `<+08>-8`.
fn posix_offset(gmtoff: i32) -> Option<String> {
    if gmtoff == 0 {
        return None;
    }
    let sign = if gmtoff >= 0 { '+' } else { '-' };
    let a = gmtoff.unsigned_abs();
    let (h, m) = (a / 3600, a % 3600 / 60);
    let posix_sign = if gmtoff >= 0 { "-" } else { "" };
    Some(if m == 0 {
        format!("<{sign}{h:02}>{posix_sign}{h}")
    } else {
        format!("<{sign}{h:02}{m:02}>{posix_sign}{h}:{m:02}")
    })
}

/// Broken-down local time using the process-wide clock offset.
pub fn local_time() -> Option<LocalTime> {
    local_time_with_offset(clock_offset())
}

/// Broken-down local time with an explicit offset in seconds relative to the
/// raw system clock.
pub fn local_time_with_offset(offset_secs: i64) -> Option<LocalTime> {
    local_time_at(raw_unix_now().saturating_add(offset_secs))
}

/// Broken-down local time for a given UTC UNIX timestamp.
#[allow(deprecated)]
pub fn local_time_at(unix_secs: i64) -> Option<LocalTime> {
    // SAFETY: localtime_r only reads `t` and writes into `tm`.
    unsafe {
        let t = unix_secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        Some(LocalTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            weekday: tm.tm_wday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
        })
    }
}

/// Format a UTC UNIX timestamp as `"YYYY-MM-DD HH:MM:SS"` for BusyBox/GNU `date -u -s`.
#[allow(deprecated)]
fn format_utc_datetime(unix_secs: i64) -> Option<String> {
    // SAFETY: gmtime_r only reads `t` and writes into `tm`.
    unsafe {
        let t = unix_secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::gmtime_r(&t, &mut tm).is_null() {
            return None;
        }
        Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ))
    }
}

/// Apply a timezone by setting `TZ` for the running process, refreshing libc
/// timezone state (`tzset`), and best-effort updating `/var/local/system/tz`
/// on Kindle when `/usr/share/zoneinfo/<iana>` is available.
pub fn apply_timezone(posix_tz: &str, iana_id: &str) {
    extern "C" {
        fn tzset();
    }
    if !posix_tz.trim().is_empty() {
        std::env::set_var("TZ", posix_tz);
        // SAFETY: POSIX `tzset` takes no arguments and refreshes libc's cached TZ state.
        unsafe {
            tzset();
        }
    }
    let zonefile = Path::new("/usr/share/zoneinfo").join(iana_id);
    if zonefile.is_file() {
        let _ = fs::copy(&zonefile, "/var/local/system/tz");
    }
}

/// Best-effort set the system clock to `unix_secs` (UTC) and persist it to the
/// hardware RTC so time keeps advancing accurately across CPU suspend/sleep.
///
/// Uses `clock_settime(CLOCK_REALTIME)` first (no shell/BusyBox flag quirks),
/// falling back to `date -u -s "YYYY-MM-DD HH:MM:SS"`, then writes the RTC via
/// `hwclock -w -u` (falling back to `hwclock -w`). Returns `Err` if the
/// platform denies setting the system clock (callers fall back to the
/// process-wide offset).
#[allow(deprecated)]
pub fn set_system_time(unix_secs: i64) -> io::Result<()> {
    let ts = libc::timespec {
        tv_sec: unix_secs as libc::time_t,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid timespec passed by pointer to clock_settime.
    let set_via_syscall = unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } == 0;

    if !set_via_syscall {
        let formatted = format_utc_datetime(unix_secs)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid unix timestamp"))?;
        let status = Command::new("date")
            .args(["-u", "-s", &formatted])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "date -u -s {formatted}: {status}"
            )));
        }
    }

    // Persist to hardware RTC so sleep/suspend preserves the adjusted clock.
    let rtc_utc = Command::new("hwclock")
        .args(["-w", "-u"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !rtc_utc.is_ok_and(|s| s.success()) {
        let _ = Command::new("hwclock")
            .arg("-w")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

/// Request a clean system reboot (Kindle / Kobo Linux).
pub fn reboot_device() -> io::Result<()> {
    let status = Command::new("reboot")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("reboot: {status}")))
    }
}

/// How the USB port is used, as the device agent keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbMode {
    /// USB network with SSH (the agent's default).
    Network,
    /// USB drive until the given UNIX time, then back to the network.
    Storage { until: i64 },
}

/// The agent's state directory.
pub const AGENT_DIR: &str = "/var/local/aristotle";

/// The agent's directory: [`AGENT_DIR`], or `$ARISTOTLE_AGENT_DIR` for a
/// development copy (with `AGENT_TEST_ROOT` set for its scripts, as in
/// `device/tests/`). The Wi-Fi policy files `net-mode` and `net-allow`
/// are read from here.
pub fn agent_dir() -> PathBuf {
    std::env::var_os("ARISTOTLE_AGENT_DIR").map_or_else(|| PathBuf::from(AGENT_DIR), PathBuf::from)
}

/// The installed agent's state directory and control script.
fn agent() -> Option<(PathBuf, PathBuf)> {
    let dir = agent_dir();
    let ctl = dir.join("aristotlectl");
    ctl.exists().then_some((dir, ctl))
}

/// The mode the agent applies, from its `usb-mode` file. `None` when the
/// agent is not installed.
pub fn usb_mode() -> Option<UsbMode> {
    let (dir, _) = agent()?;
    let s = fs::read_to_string(dir.join("usb-mode")).unwrap_or_default();
    Some(parse_usb_mode(&s, now()))
}

fn parse_usb_mode(s: &str, now: i64) -> UsbMode {
    let mut it = s.split_whitespace();
    match (it.next(), it.next().and_then(|v| v.parse::<i64>().ok())) {
        (Some("storage"), Some(until)) if until > now => UsbMode::Storage { until },
        _ => UsbMode::Network,
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Ask the agent for a USB mode: `aristotlectl mode net`, or `mode storage N`
/// for drive mode lasting `minutes`. The agent applies it within 15 s. In
/// drive mode SSH drops and the device cannot write `/mnt/us` while the host
/// has it mounted.
pub fn set_usb_mode(storage_minutes: Option<u32>) -> io::Result<()> {
    let (_, ctl) = agent().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(&ctl).arg("mode");
    match storage_minutes {
        Some(m) => cmd.arg("storage").arg(m.to_string()),
        None => cmd.arg("net"),
    };
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{}: {status}", ctl.display())))
    }
}

/// Add (`add`) or remove a `net-allow` entry the app needs, tagged with
/// its feature: `aristotlectl net allow add|remove ENTRY FEATURE`, applied
/// at once in `scoped` mode. Returns the script's message. `NotFound`
/// when the agent is not installed, in which case the stock firewall lets
/// everything out and nothing needs adding.
pub fn net_allow(add: bool, entry: &str, feature: &str) -> io::Result<String> {
    let (_, ctl) = agent().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let out = Command::new("/bin/sh")
        .arg(&ctl)
        .args([
            "net",
            "allow",
            if add { "add" } else { "remove" },
            entry,
            feature,
        ])
        .stdin(Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(io::Error::other(format!(
            "{}: {}",
            ctl.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tzif_one_type(gmtoff: i32, footer: &[u8]) -> Vec<u8> {
        let header = |out: &mut Vec<u8>| {
            out.extend_from_slice(b"TZif2");
            out.extend_from_slice(&[0; 15]);
            for n in [0u32, 0, 0, 0, 1, 4] {
                out.extend_from_slice(&n.to_be_bytes());
            }
            out.extend_from_slice(&gmtoff.to_be_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(b"CST\0");
        };
        let mut out = Vec::new();
        header(&mut out);
        header(&mut out);
        out.extend_from_slice(footer);
        out
    }

    #[test]
    fn kindle_device_zone_file() {
        // /etc/localtime as read from a Kindle set to GMT+8.
        const DEVICE: &[u8] = &[
            84, 90, 105, 102, 50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
            1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 11, 0, 0, 112, 128, 0, 0, 71, 77, 84,
            43, 56, 58, 48, 49, 48, 48, 0, 0, 0, 84, 90, 105, 102, 50, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0,
            11, 255, 255, 255, 255, 124, 85, 129, 128, 0, 0, 0, 112, 128, 0, 0, 71, 77, 84, 43, 56,
            58, 48, 49, 48, 48, 0, 0, 0, 10, 10,
        ];
        assert_eq!(tz_from_tzif(DEVICE).as_deref(), Some("<+08>-8"));
    }

    #[test]
    fn kindle_zone_without_footer() {
        assert_eq!(
            tz_from_tzif(&tzif_one_type(28800, b"\n\n")).as_deref(),
            Some("<+08>-8")
        );
        assert_eq!(
            tz_from_tzif(&tzif_one_type(-34200, b"\n\n")).as_deref(),
            Some("<-0930>9:30")
        );
        assert_eq!(tz_from_tzif(&tzif_one_type(28800, b"\nCST-8\n")), None);
        assert_eq!(tz_from_tzif(b"nope"), None);
    }

    #[test]
    fn meminfo() {
        let m = parse_meminfo("MemTotal:  505704 kB\nMemFree: 1 kB\nMemAvailable:   401000 kB\n");
        assert_eq!(
            m,
            Memory {
                total_kb: 505704,
                available_kb: 401000
            }
        );
    }

    #[test]
    fn pretty_version() {
        assert_eq!(
            parse_pretty_version("Kindle 5.13.6 (4110410039) Thu Apr 8 2021\n").as_deref(),
            Some("5.13.6")
        );
        assert_eq!(parse_pretty_version("nothing here"), None);
    }

    #[test]
    fn usb_mode_file() {
        assert_eq!(parse_usb_mode("", 100), UsbMode::Network);
        assert_eq!(
            parse_usb_mode("storage 200\n", 100),
            UsbMode::Storage { until: 200 }
        );
        // Expired: the agent is already back on the network.
        assert_eq!(parse_usb_mode("storage 50", 100), UsbMode::Network);
    }

    #[test]
    fn storage_of_the_build_dir() {
        let s = storage(Path::new(".")).unwrap();
        assert!(s.total > 0 && s.free <= s.total);
    }

    #[test]
    fn format_utc_datetime_matches_busybox_format() {
        assert_eq!(
            format_utc_datetime(0).as_deref(),
            Some("1970-01-01 00:00:00")
        );
        assert_eq!(
            format_utc_datetime(1_795_402_510).as_deref(),
            Some("2026-11-24 02:55:10")
        );
    }

    #[test]
    fn global_clock_offset_applies_to_unix_now_and_local_time() {
        let prev_offset = clock_offset();
        let prev_24h = clock_24h();

        set_clock_offset(3600);
        set_clock_24h(false);
        assert_eq!(clock_offset(), 3600);
        assert!(!clock_24h());
        let diff = unix_now() - raw_unix_now();
        assert!((3599..=3601).contains(&diff), "diff was {diff}");
        assert!(local_time().is_some());

        set_clock_offset(prev_offset);
        set_clock_24h(prev_24h);
    }
}
