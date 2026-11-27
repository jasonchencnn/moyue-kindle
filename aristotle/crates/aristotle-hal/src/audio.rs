// SPDX-License-Identifier: AGPL-3.0-or-later
//! Audio output sink for Bluetooth audiobook reading on Kindle.
//!
//! On Kindle Oasis 2 / PW4 (firmware 5.16.x), there is no ALSA hardware
//! soundcard and no PulseAudio daemon. Audio goes through Amazon's proprietary
//! GStreamer 0.10 `mixersink` element (`libmixerAPI.so.1` -> `audiomgrd` ->
//! `btd` -> Bluetooth A2DP).
//!
//! [`KindleMixerSink`] pipes raw 16-bit signed little-endian PCM directly into
//! `gst-launch-0.10 fdsrc ! audio/x-raw-int,... ! mixersink`.
//! [`NullTimingSink`] simulates real-time playback without hardware so the full
//! TTS pipeline (prefetch, auto-page-turn, sleep keepalive) can be tested
//! without Bluetooth earphones or on a host machine.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// PCM stream format (always 16-bit signed little-endian).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    /// Sample rate in Hz (e.g. `24000`).
    pub sample_rate: u32,
    /// Number of channels (`1` = mono, `2` = stereo).
    pub channels: u16,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 24_000,
            channels: 1,
        }
    }
}

impl AudioFormat {
    /// Bytes per second for 16-bit signed PCM.
    pub fn bytes_per_sec(self) -> usize {
        let rate = self.sample_rate.max(1) as usize;
        let ch = self.channels.max(1) as usize;
        rate.saturating_mul(ch).saturating_mul(2)
    }

    /// Playback duration of `byte_len` PCM bytes.
    pub fn duration_of(self, byte_len: usize) -> Duration {
        let bps = self.bytes_per_sec();
        let millis = (byte_len as u128).saturating_mul(1000) / (bps as u128);
        Duration::from_millis(millis as u64)
    }
}

/// Abstract blocking PCM playback sink.
pub trait AudioSink: Send {
    /// Play raw 16-bit signed little-endian PCM, stopping promptly when
    /// `cancel` becomes `true`.
    fn play_pcm(&mut self, pcm: &[u8], fmt: AudioFormat, cancel: &AtomicBool) -> io::Result<()>;
}

/// Kindle Bluetooth A2DP sink backed by `gst-launch-0.10 ... ! mixersink`.
pub struct KindleMixerSink {
    gst_bin: String,
}

impl Default for KindleMixerSink {
    fn default() -> Self {
        Self::new()
    }
}

impl KindleMixerSink {
    /// Create a sink using the stock `/usr/bin/gst-launch-0.10` binary.
    pub fn new() -> Self {
        Self {
            gst_bin: "/usr/bin/gst-launch-0.10".to_string(),
        }
    }

    /// Whether the Kindle GStreamer 0.10 binary is present on this system.
    pub fn is_available() -> bool {
        Path::new("/usr/bin/gst-launch-0.10").is_file()
    }
}

impl AudioSink for KindleMixerSink {
    fn play_pcm(&mut self, pcm: &[u8], fmt: AudioFormat, cancel: &AtomicBool) -> io::Result<()> {
        if pcm.is_empty() || cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let caps = format!(
            "audio/x-raw-int,rate={},channels={},width=16,depth=16,signed=true,endianness=1234",
            fmt.sample_rate.max(8000),
            fmt.channels.clamp(1, 2),
        );
        let mut child = Command::new(&self.gst_bin)
            .args(["fdsrc", "!", &caps, "!", "mixersink"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            // Write in ~100 ms chunks so cancellation interrupts quickly.
            let chunk_size = (fmt.bytes_per_sec() / 10).max(2048);
            for chunk in pcm.chunks(chunk_size) {
                if cancel.load(Ordering::Acquire) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
                if let Err(e) = stdin.write_all(chunk) {
                    if cancel.load(Ordering::Acquire) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(());
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
            }
            let _ = stdin.flush();
        }

        // Wait for playback to finish while still honoring cancel.
        loop {
            if cancel.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
            match child.try_wait()? {
                Some(_) => return Ok(()),
                None => thread::sleep(Duration::from_millis(40)),
            }
        }
    }
}

/// Timing-accurate silent sink for testing without Bluetooth earphones.
pub struct NullTimingSink;

impl AudioSink for NullTimingSink {
    fn play_pcm(&mut self, pcm: &[u8], fmt: AudioFormat, cancel: &AtomicBool) -> io::Result<()> {
        let total = fmt.duration_of(pcm.len());
        let deadline = Instant::now() + total;
        while Instant::now() < deadline {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remain.min(Duration::from_millis(50)));
        }
        Ok(())
    }
}

/// Automatically picks [`KindleMixerSink`] on Kindle or [`NullTimingSink`] when
/// `simulate` is requested or GStreamer is unavailable.
pub fn open_sink(simulate: bool) -> Box<dyn AudioSink> {
    if !simulate && KindleMixerSink::is_available() {
        Box::new(KindleMixerSink::new())
    } else {
        Box::new(NullTimingSink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_format_duration_calculation() {
        let fmt = AudioFormat {
            sample_rate: 24_000,
            channels: 1,
        };
        assert_eq!(fmt.bytes_per_sec(), 48_000);
        assert_eq!(fmt.duration_of(48_000), Duration::from_secs(1));
        assert_eq!(fmt.duration_of(24_000), Duration::from_millis(500));
    }

    #[test]
    fn null_timing_sink_stops_immediately_on_cancel() {
        let mut sink = NullTimingSink;
        let cancel = AtomicBool::new(true);
        let start = Instant::now();
        // 10 seconds of audio should return immediately when cancelled.
        let pcm = vec![0u8; 480_000];
        sink.play_pcm(&pcm, AudioFormat::default(), &cancel)
            .unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));
    }
}
