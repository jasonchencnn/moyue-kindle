// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bluetooth radio, device discovery, PIN/Passkey pairing, and HID connection
//! management for Kindle (`bluetoothctl` + `lipc` fallback) and Kobo (`bluetoothctl`).
//!
//! All external commands detach stdin/stderr unless driving an interactive
//! `bluetoothctl` pairing agent, and fall back cleanly off-device (host tests
//! and desktop previews).

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Which system backend is driving Bluetooth on this device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BluetoothBackend {
    /// Standard BlueZ CLI (`bluetoothctl`) with full scan, pairing, and HID support.
    BlueZ,
    /// Kindle stock `com.lab126.btfd` via `lipc` (radio toggle only; no BlueZ CLI).
    KindleLipc,
    /// No Bluetooth controller or CLI available (e.g. host tests or unsupported model).
    #[default]
    Unavailable,
}

/// A discovered, paired, or connected Bluetooth device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothDevice {
    /// Canonical uppercase MAC address (`AA:BB:CC:DD:EE:FF`).
    pub mac: String,
    /// Human-readable name (falls back to MAC when unnamed).
    pub name: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    /// Last known RSSI in dBm, if reported.
    pub rssi: Option<i32>,
}

/// Snapshot of the local Bluetooth adapter state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothStatus {
    pub backend: BluetoothBackend,
    pub available: bool,
    pub powered: bool,
    pub discovering: bool,
    pub connected_device: Option<BluetoothDevice>,
}

/// Result of attempting to pair and connect a Bluetooth device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairOutcome {
    /// Device is paired, trusted, and connected.
    PairedAndConnected,
    /// Pairing succeeded, but the subsequent connection attempt failed.
    PairedNotConnected(String),
    /// The remote device requested a PIN code or passkey (or rejected the previous one).
    PinRequired { mac: String, prompt: String },
    /// SSP numeric comparison requested user confirmation of a 6-digit passkey.
    ConfirmPasskey { mac: String, passkey: String },
    /// Pairing failed for a non-PIN reason.
    Failed(String),
}

/// Validate a canonical 6-octet Bluetooth MAC address (`XX:XX:XX:XX:XX:XX`).
pub fn is_valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Sanitize a user-entered PIN or passkey (1..=16 printable ASCII chars, no newlines).
pub fn sanitize_pin(pin: &str) -> Option<String> {
    let trimmed = pin.trim();
    if trimmed.is_empty() || trimmed.len() > 16 {
        return None;
    }
    if trimmed.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Query current Bluetooth adapter status and connected device.
pub fn status() -> BluetoothStatus {
    if let Ok(show_out) = run_btctl(&["show"]) {
        if let Some((powered, discovering)) = parse_controller_show(&show_out) {
            let connected = if powered {
                list_devices().into_iter().find(|d| d.connected)
            } else {
                None
            };
            return BluetoothStatus {
                backend: BluetoothBackend::BlueZ,
                available: true,
                powered,
                discovering,
                connected_device: connected,
            };
        }
    }

    // Fallback: Kindle stock `com.lab126.btfd` property via lipc.
    if let Some(powered) = query_kindle_bt_powered() {
        return BluetoothStatus {
            backend: BluetoothBackend::KindleLipc,
            available: true,
            powered,
            discovering: false,
            connected_device: None,
        };
    }

    BluetoothStatus::default()
}

/// Enable or disable the Bluetooth radio.
pub fn set_powered(on: bool) -> io::Result<()> {
    let arg = if on { "on" } else { "off" };
    if on {
        // Best-effort unblock rfkill before powering on BlueZ.
        let _ = Command::new("rfkill")
            .args(["unblock", "bluetooth"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    if let Ok(out) = run_btctl(&["power", arg]) {
        if !out.contains("No default controller available") && !out.contains("Failed to set power")
        {
            // Keep Kindle btfd in sync if present, ignoring errors.
            let _ = set_kindle_bt_powered(on);
            return Ok(());
        }
    }

    if set_kindle_bt_powered(on).is_ok() {
        return Ok(());
    }

    // Last resort: rfkill toggle.
    let status = Command::new("rfkill")
        .args([if on { "unblock" } else { "block" }, "bluetooth"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("bluetooth power {arg} failed")))
    }
}

/// Scan for nearby Bluetooth devices for `timeout_secs` seconds and return the
/// deduplicated, sorted device list.
pub fn scan(timeout_secs: u64) -> io::Result<Vec<BluetoothDevice>> {
    let secs = timeout_secs.clamp(2, 15).to_string();
    // `bluetoothctl --timeout N scan on` exits automatically after N seconds.
    let _ = run_btctl(&["--timeout", &secs, "scan", "on"]);
    Ok(list_devices())
}

/// List known (discovered, paired, and connected) Bluetooth devices without
/// starting a new scan.
pub fn list_devices() -> Vec<BluetoothDevice> {
    let Ok(all_out) = run_btctl(&["devices"]) else {
        return Vec::new();
    };
    let paired_out = run_btctl(&["devices", "Paired"]).unwrap_or_default();
    let connected_out = run_btctl(&["devices", "Connected"]).unwrap_or_default();
    let trusted_out = run_btctl(&["devices", "Trusted"]).unwrap_or_default();

    let mut devices = parse_device_lists(&all_out, &paired_out, &connected_out, &trusted_out);

    // Enrich with `info <MAC>` when the BlueZ version does not support
    // `devices Paired` / `devices Connected` subcommands or for RSSI.
    for dev in &mut devices {
        if let Ok(info) = run_btctl(&["info", &dev.mac]) {
            apply_device_info(dev, &info);
        }
    }
    sort_devices(&mut devices);
    devices
}

/// Pair (with optional PIN/passkey), trust, and connect to a Bluetooth device.
///
/// - Pass `pin = None` on the first attempt; if the remote device asks for a
///   PIN or passkey, this returns [`PairOutcome::PinRequired`] so the UI can
///   prompt the user with an on-screen keyboard.
/// - Pass `pin = Some("0000")` (or user-entered code) to answer a PIN/passkey
///   prompt.
/// - Pass `confirm_passkey = true` to auto-confirm SSP numeric comparison
///   prompts (`Confirm passkey XXXXXX (yes/no):`).
pub fn pair_and_connect(mac: &str, pin: Option<&str>, confirm_passkey: bool) -> PairOutcome {
    if !is_valid_mac(mac) {
        return PairOutcome::Failed(format!("invalid MAC address: {mac}"));
    }
    let clean_pin = match pin {
        Some(p) => match sanitize_pin(p) {
            Some(clean) => Some(clean),
            None => return PairOutcome::Failed("invalid PIN format".to_string()),
        },
        None => None,
    };

    // If already paired, skip straight to trust + connect.
    if let Ok(info) = run_btctl(&["info", mac]) {
        if info.contains("Paired: yes") {
            let _ = run_btctl(&["trust", mac]);
            return match connect(mac) {
                Ok(()) => PairOutcome::PairedAndConnected,
                Err(e) => PairOutcome::PairedNotConnected(e.to_string()),
            };
        }
    }

    let pair_res = run_interactive_pair(mac, clean_pin.as_deref(), confirm_passkey);
    match pair_res {
        PairOutcome::PairedAndConnected => {
            let _ = run_btctl(&["trust", mac]);
            match connect(mac) {
                Ok(()) => PairOutcome::PairedAndConnected,
                Err(e) => PairOutcome::PairedNotConnected(e.to_string()),
            }
        }
        other => other,
    }
}

/// Connect to an already-paired Bluetooth device.
pub fn connect(mac: &str) -> io::Result<()> {
    if !is_valid_mac(mac) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid MAC address",
        ));
    }
    let _ = run_btctl(&["trust", mac]);
    let out = run_btctl(&["connect", mac])?;
    if out.contains("Connection successful") || out.contains("AlreadyConnected") {
        Ok(())
    } else {
        let msg = extract_bt_error(&out).unwrap_or_else(|| "connection failed".to_string());
        Err(io::Error::other(msg))
    }
}

/// Disconnect a connected Bluetooth device.
pub fn disconnect(mac: &str) -> io::Result<()> {
    if !is_valid_mac(mac) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid MAC address",
        ));
    }
    let out = run_btctl(&["disconnect", mac])?;
    if out.contains("Successful disconnected")
        || out.contains("NotConnected")
        || !out.contains("Failed")
    {
        Ok(())
    } else {
        let msg = extract_bt_error(&out).unwrap_or_else(|| "disconnect failed".to_string());
        Err(io::Error::other(msg))
    }
}

/// Remove (unpair / forget) a Bluetooth device.
pub fn remove(mac: &str) -> io::Result<()> {
    if !is_valid_mac(mac) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid MAC address",
        ));
    }
    let out = run_btctl(&["remove", mac])?;
    if out.contains("Device has been removed") || !out.contains("Failed") {
        Ok(())
    } else {
        let msg = extract_bt_error(&out).unwrap_or_else(|| "remove failed".to_string());
        Err(io::Error::other(msg))
    }
}

/// Run an interactive `bluetoothctl` session to drive `agent KeyboardDisplay`
/// and handle PIN / Passkey / SSP confirmation prompts.
fn run_interactive_pair(mac: &str, pin: Option<&str>, confirm_passkey: bool) -> PairOutcome {
    let mut child = match Command::new("bluetoothctl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return PairOutcome::Failed(format!("bluetoothctl unavailable: {e}")),
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return PairOutcome::Failed("failed to open bluetoothctl stdin".to_string());
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        return PairOutcome::Failed("failed to open bluetoothctl stdout".to_string());
    };

    // Send agent registration and pair command.
    let init_cmds = format!("agent KeyboardDisplay\ndefault-agent\npair {mac}\n");
    if stdin.write_all(init_cmds.as_bytes()).is_err() || stdin.flush().is_err() {
        let _ = child.kill();
        return PairOutcome::Failed("failed to send pair command".to_string());
    }

    // Read stdout chunks on a helper thread so we can enforce a hard timeout.
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            let chunk = strip_ansi(&String::from_utf8_lossy(&buf[..n]));
            if tx.send(chunk).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(18);
    let mut accumulated = String::new();
    let mut pin_sent = false;
    let mut confirm_sent = false;

    let outcome = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break PairOutcome::Failed("pairing timed out".to_string());
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(chunk) => {
                accumulated.push_str(&chunk);
                match classify_pair_transcript(&accumulated, mac) {
                    TranscriptEvent::Pending => {}
                    TranscriptEvent::Paired => break PairOutcome::PairedAndConnected,
                    TranscriptEvent::PinPrompt(prompt) => {
                        if let Some(code) = pin {
                            if !pin_sent {
                                pin_sent = true;
                                let _ = writeln!(stdin, "{code}");
                                let _ = stdin.flush();
                                accumulated.clear();
                            }
                        } else {
                            break PairOutcome::PinRequired {
                                mac: mac.to_string(),
                                prompt,
                            };
                        }
                    }
                    TranscriptEvent::ConfirmPasskey(passkey) => {
                        if confirm_passkey {
                            if !confirm_sent {
                                confirm_sent = true;
                                let _ = writeln!(stdin, "yes");
                                let _ = stdin.flush();
                                accumulated.clear();
                            }
                        } else {
                            break PairOutcome::ConfirmPasskey {
                                mac: mac.to_string(),
                                passkey,
                            };
                        }
                    }
                    TranscriptEvent::AuthFailed(msg) => {
                        break PairOutcome::PinRequired {
                            mac: mac.to_string(),
                            prompt: msg,
                        };
                    }
                    TranscriptEvent::Error(msg) => break PairOutcome::Failed(msg),
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break PairOutcome::Failed(
                    extract_bt_error(&accumulated)
                        .unwrap_or_else(|| "bluetoothctl exited unexpectedly".to_string()),
                );
            }
        }
    };

    let _ = writeln!(stdin, "quit");
    let _ = stdin.flush();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    outcome
}

#[derive(Debug, PartialEq, Eq)]
enum TranscriptEvent {
    Pending,
    Paired,
    PinPrompt(String),
    ConfirmPasskey(String),
    AuthFailed(String),
    Error(String),
}

/// Classify accumulated `bluetoothctl` interactive output during pairing.
fn classify_pair_transcript(text: &str, _mac: &str) -> TranscriptEvent {
    let lower = text.to_ascii_lowercase();
    if lower.contains("pairing successful") || lower.contains("alreadyexists") {
        return TranscriptEvent::Paired;
    }
    if lower.contains("authenticationfailed")
        || lower.contains("authenticationrejected")
        || lower.contains("authenticationcanceled")
    {
        return TranscriptEvent::AuthFailed("PIN/Passkey authentication failed".to_string());
    }
    if let Some(passkey) = extract_confirm_passkey(text) {
        return TranscriptEvent::ConfirmPasskey(passkey);
    }
    if lower.contains("enter pin code")
        || lower.contains("enter passkey")
        || lower.contains("request pin code")
        || lower.contains("request passkey")
    {
        let prompt = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("Enter PIN code")
            .trim()
            .to_string();
        return TranscriptEvent::PinPrompt(prompt);
    }
    if lower.contains("failed to pair") || lower.contains("not available") {
        let msg = extract_bt_error(text).unwrap_or_else(|| "pairing failed".to_string());
        return TranscriptEvent::Error(msg);
    }
    TranscriptEvent::Pending
}

/// Extract the 6-digit passkey from `Confirm passkey 123456 (yes/no):`.
fn extract_confirm_passkey(text: &str) -> Option<String> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(pos) = lower.find("confirm passkey") {
            let rest = &line[pos + "confirm passkey".len()..];
            for tok in rest.split_whitespace() {
                let digits: String = tok.chars().filter(|c| c.is_ascii_digit()).collect();
                if digits.len() >= 4 {
                    return Some(digits);
                }
            }
        }
    }
    None
}

fn run_btctl(args: &[&str]) -> io::Result<String> {
    let out = Command::new("bluetoothctl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    let clean = strip_ansi(&combined);
    if out.status.success() || !clean.trim().is_empty() {
        Ok(clean)
    } else {
        Err(io::Error::other(format!(
            "bluetoothctl failed: {}",
            out.status
        )))
    }
}

fn query_kindle_bt_powered() -> Option<bool> {
    for prop in ["BTstatus", "enable", "power"] {
        if let Ok(v) = crate::powerd::get_prop("com.lab126.btfd", prop, true) {
            let t = v.trim();
            if t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("on") {
                return Some(true);
            }
            if t == "0" || t.eq_ignore_ascii_case("false") || t.eq_ignore_ascii_case("off") {
                return Some(false);
            }
        }
    }
    None
}

fn set_kindle_bt_powered(on: bool) -> io::Result<()> {
    let val = if on { "1" } else { "0" };
    for prop in ["BTstatus", "enable", "power"] {
        if crate::powerd::set_prop("com.lab126.btfd", prop, val, true).is_ok() {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Kindle btfd lipc property unavailable",
    ))
}

/// Strip ANSI escape sequences emitted by interactive `bluetoothctl`.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for nc in chars.by_ref() {
                    if nc.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Parse `bluetoothctl show` output into `(powered, discovering)`.
pub fn parse_controller_show(output: &str) -> Option<(bool, bool)> {
    if output.contains("No default controller available") {
        return None;
    }
    let mut saw_controller = false;
    let mut powered = false;
    let mut discovering = false;
    for line in output.lines() {
        let t = line.trim();
        if t.starts_with("Controller ") {
            saw_controller = true;
        } else if let Some(v) = t.strip_prefix("Powered:") {
            powered = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = t.strip_prefix("Discovering:") {
            discovering = v.trim().eq_ignore_ascii_case("yes");
        }
    }
    saw_controller.then_some((powered, discovering))
}

/// Parse `Device AA:BB:CC:DD:EE:FF Name` lines from `bluetoothctl devices`.
fn parse_device_lines(output: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("Device ") else {
            continue;
        };
        let mut parts = rest.splitn(2, ' ');
        let Some(mac) = parts.next() else {
            continue;
        };
        if !is_valid_mac(mac) {
            continue;
        }
        let mac_upper = mac.to_ascii_uppercase();
        let name = parts
            .next()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&mac_upper)
            .to_string();
        out.push((mac_upper, name));
    }
    out
}

/// Combine `devices`, `devices Paired`, `devices Connected`, and `devices Trusted` outputs.
pub fn parse_device_lists(
    all_out: &str,
    paired_out: &str,
    connected_out: &str,
    trusted_out: &str,
) -> Vec<BluetoothDevice> {
    let paired: HashSet<String> = parse_device_lines(paired_out)
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    let connected: HashSet<String> = parse_device_lines(connected_out)
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    let trusted: HashSet<String> = parse_device_lines(trusted_out)
        .into_iter()
        .map(|(m, _)| m)
        .collect();

    let mut by_mac: HashMap<String, BluetoothDevice> = HashMap::new();
    for (mac, name) in parse_device_lines(all_out)
        .into_iter()
        .chain(parse_device_lines(paired_out))
        .chain(parse_device_lines(connected_out))
    {
        let entry = by_mac
            .entry(mac.clone())
            .or_insert_with(|| BluetoothDevice {
                mac: mac.clone(),
                name: name.clone(),
                paired: false,
                trusted: false,
                connected: false,
                rssi: None,
            });
        if entry.name == entry.mac && name != mac {
            entry.name = name;
        }
        entry.paired |= paired.contains(&mac);
        entry.connected |= connected.contains(&mac);
        entry.trusted |= trusted.contains(&mac);
    }

    let mut list: Vec<BluetoothDevice> = by_mac.into_values().collect();
    sort_devices(&mut list);
    list
}

/// Merge detailed fields from `bluetoothctl info <MAC>` into a [`BluetoothDevice`].
pub fn apply_device_info(dev: &mut BluetoothDevice, info: &str) {
    for line in info.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Alias:").or_else(|| t.strip_prefix("Name:")) {
            let v = v.trim();
            if !v.is_empty() && (dev.name == dev.mac || t.starts_with("Alias:")) {
                dev.name = v.to_string();
            }
        } else if let Some(v) = t.strip_prefix("Paired:") {
            dev.paired = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = t.strip_prefix("Trusted:") {
            dev.trusted = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = t.strip_prefix("Connected:") {
            dev.connected = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = t.strip_prefix("RSSI:") {
            // Format may be `-58` or `0xffffffc6 (-58)`.
            if let Some(paren_start) = v.find('(') {
                let inner = v[paren_start + 1..].trim_end_matches(')').trim();
                if let Ok(dbm) = inner.parse::<i32>() {
                    dev.rssi = Some(dbm);
                }
            } else if let Ok(dbm) = v.trim().parse::<i32>() {
                dev.rssi = Some(dbm);
            }
        }
    }
}

fn sort_devices(devices: &mut [BluetoothDevice]) {
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| {
                let a_named = a.name != a.mac;
                let b_named = b.name != b.mac;
                b_named.cmp(&a_named)
            })
            .then_with(|| b.rssi.unwrap_or(-120).cmp(&a.rssi.unwrap_or(-120)))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn extract_bt_error(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| l.contains("Failed") || l.contains("Error") || l.contains("not available"))
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_mac_and_pin_strictly() {
        assert!(is_valid_mac("AA:BB:CC:DD:EE:FF"));
        assert!(is_valid_mac("01:23:45:67:89:ab"));
        assert!(!is_valid_mac("AA:BB:CC:DD:EE"));
        assert!(!is_valid_mac("AA:BB:CC:DD:EE:GG"));
        assert!(!is_valid_mac("AA:BB:CC:DD:EE:FF; rm -rf /"));

        assert_eq!(sanitize_pin("0000"), Some("0000".to_string()));
        assert_eq!(sanitize_pin(" 123456 "), Some("123456".to_string()));
        assert_eq!(sanitize_pin(""), None);
        assert_eq!(sanitize_pin("1234\nquit"), None);
        assert_eq!(sanitize_pin("12345678901234567"), None);
    }

    #[test]
    fn parses_controller_show_output() {
        let sample = "\
Controller 11:22:33:44:55:66 (public)
\tName: Kindle
\tPowered: yes
\tDiscoverable: no
\tDiscovering: yes
";
        assert_eq!(parse_controller_show(sample), Some((true, true)));
        assert_eq!(
            parse_controller_show("No default controller available\n"),
            None
        );
    }

    #[test]
    fn parses_and_sorts_device_lists_and_info() {
        let all = "\
Device 11:22:33:44:55:66 8BitDo Micro
Device AA:BB:CC:DD:EE:FF Page Turner
Device 99:88:77:66:55:44 99:88:77:66:55:44
";
        let paired = "Device AA:BB:CC:DD:EE:FF Page Turner\n";
        let connected = "Device AA:BB:CC:DD:EE:FF Page Turner\n";
        let trusted = "Device AA:BB:CC:DD:EE:FF Page Turner\n";

        let mut list = parse_device_lists(all, paired, connected, trusted);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].mac, "AA:BB:CC:DD:EE:FF");
        assert!(list[0].connected && list[0].paired && list[0].trusted);
        assert_eq!(list[1].mac, "11:22:33:44:55:66");
        assert_eq!(list[2].mac, "99:88:77:66:55:44");

        let info = "\
Device 11:22:33:44:55:66 (public)
\tName: 8BitDo Micro
\tAlias: 8BitDo Micro Gamepad
\tPaired: yes
\tTrusted: yes
\tConnected: no
\tRSSI: 0xffffffc6 (-58)
";
        apply_device_info(&mut list[1], info);
        assert_eq!(list[1].name, "8BitDo Micro Gamepad");
        assert!(list[1].paired);
        assert_eq!(list[1].rssi, Some(-58));
    }

    #[test]
    fn classifies_interactive_pairing_prompts() {
        assert_eq!(
            classify_pair_transcript(
                "Request PIN code\n[agent] Enter PIN code:",
                "AA:BB:CC:DD:EE:FF"
            ),
            TranscriptEvent::PinPrompt("[agent] Enter PIN code:".to_string())
        );
        assert_eq!(
            classify_pair_transcript(
                "[agent] Confirm passkey 482910 (yes/no):",
                "AA:BB:CC:DD:EE:FF"
            ),
            TranscriptEvent::ConfirmPasskey("482910".to_string())
        );
        assert_eq!(
            classify_pair_transcript("Pairing successful\n", "AA:BB:CC:DD:EE:FF"),
            TranscriptEvent::Paired
        );
        assert_eq!(
            classify_pair_transcript(
                "Failed to pair: org.bluez.Error.AuthenticationFailed",
                "AA:BB:CC:DD:EE:FF"
            ),
            TranscriptEvent::AuthFailed("PIN/Passkey authentication failed".to_string())
        );
    }

    #[test]
    fn strips_ansi_escape_codes() {
        let raw = "\x1b[0;94m[bluetooth]#\x1b[0m Device AA:BB:CC:DD:EE:FF Turner\r\n";
        assert_eq!(
            strip_ansi(raw),
            "[bluetooth]# Device AA:BB:CC:DD:EE:FF Turner\n"
        );
    }
}
