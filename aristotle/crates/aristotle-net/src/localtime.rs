// SPDX-License-Identifier: AGPL-3.0-or-later
//! The device's local wall-clock format, `YYYY-MM-DD HH:MM:SS`.
//!
//! Annotations and clippings are written by hand and by older importers in
//! this format; the sync layer must read and write it unchanged instead of
//! replacing it with a UTC ISO timestamp.

/// Format `unix` seconds as the device's local time.
// libc's `time_t` alias is deprecated only on the 32-bit device target; there
// is no other spelling that works on both the device and the host.
#[allow(deprecated)]
pub fn format(unix: i64) -> String {
    // SAFETY: localtime_r only writes into the tm passed.
    unsafe {
        let t = unix as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// Parse `YYYY-MM-DD HH:MM:SS` in the device's local time.
#[allow(deprecated)]
pub fn parse(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b' '
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let field = |from: usize, to: usize| s.get(from..to)?.parse::<i32>().ok();
    let year = field(0, 4)?;
    let month = field(5, 7)?;
    let day = field(8, 10)?;
    let hour = field(11, 13)?;
    let minute = field(14, 16)?;
    let second = field(17, 19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    tm.tm_isdst = -1;
    // SAFETY: mktime reads and normalises the tm passed.
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then_some(t as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_the_local_convention() {
        let s = "2026-11-09 12:00:00";
        let unix = parse(s).expect("valid local time");
        assert_eq!(format(unix), s);
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(parse("2026-11-09T12:00:00Z"), None);
        assert_eq!(parse("not a time"), None);
        assert_eq!(parse("2026-13-09 12:00:00"), None);
        assert_eq!(parse("2026-11-09 25:00:00"), None);
    }
}
