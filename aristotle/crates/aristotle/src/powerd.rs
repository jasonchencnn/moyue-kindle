// SPDX-License-Identifier: AGPL-3.0-or-later
//! Follow stock powerd by default; ARISTOTLE_POWERD=0 selects the fallback.
//!
//! Successful takeover unloads blanket's screensaver and releases preventScreenSaver.
//! Firmware readiness checks still apply to the power button.
//! powerd's `goingToScreenSaver` (power button, cover, its idle timer, or
//! our own timer pressing the button through `powerButton`) makes the app
//! save and draw its screensaver card; powerd then suspends the CPU a minute
//! later with the card on the panel. `outOfScreenSaver` (the button again)
//! brings the screen back. Touch does not wake managed sleep.
//!
//! The app's "sleep after" setting can exceed powerd's own idle timer (T1,
//! 600 s on our unit [dev]), so the app restarts T1 after touches (at most
//! once a minute) and every five minutes while awake, and presses the
//! button itself when its own time is up. "Never" keeps restarting T1.
//!
//! When powerd does not react, the app falls back to its own screensaver without suspend.
//! A touch wakes that fallback; managed sleep supplies KAF readiness after takeover.
//! Firmware drive_mode_state=1 means off, not a USB-network suspend blocker.
//!
//! powerd only acts on the power button when something answers for the
//! framework (FIX-108). The stock framework answers until a takeover stops
//! it, a few seconds after the app starts; the KAF shim takes over then, and
//! the app looks for it at most once every two seconds.

use std::os::fd::RawFd;
use std::time::{Duration, Instant};

use aristotle_hal::powerd::{self as hal, Lipc, PowerEvent, PowerEvents};
use aristotle_hal::screensaver::Screensaver;

use crate::perf;

/// How long to wait for `goingToScreenSaver` after pressing the button for
/// our own timer.
const SLEEP_WAIT: Duration = Duration::from_secs(5);
/// T1 restarts: after a touch at most this often...
const PING_TOUCHED: Duration = Duration::from_secs(60);
/// ... and at least this often while awake (T1 is 600 s on our unit).
const PING_ALWAYS: Duration = Duration::from_secs(300);
/// How often the framework's KAF source is checked (FIX-108).
const KAF_CHECK: Duration = Duration::from_secs(2);

/// The next check for the framework's KAF source.
///
/// powerd asks lipcd whether the framework is up before it acts on the power
/// button. The original framework answers that until it stops: a takeover
/// kills it ten to thirteen seconds after the app starts, and then nothing
/// answers any more, so powerd ignores the button (FIX-108). The check
/// starts the shim that takes the answer over.
#[derive(Debug)]
struct KafCheck {
    /// When the next check is due.
    at: Instant,
}

impl KafCheck {
    /// The first check is due one interval from now.
    fn new(now: Instant) -> Self {
        Self {
            at: now + KAF_CHECK,
        }
    }

    /// Whether the check is due now, and when the next one is.
    fn due(&mut self, now: Instant) -> bool {
        if now < self.at {
            return false;
        }
        self.at = now + KAF_CHECK;
        true
    }
}

/// What the loop should do after [`Sleep::tick`].
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Show the screensaver without powerd (it did not react).
    Sleep,
}

pub struct Sleep {
    events: Option<PowerEvents>,
    lipc: Lipc,
    /// Restores stock screensaver state on every exit path.
    screensaver: Option<Screensaver>,
    last_ping: Instant,
    touched: bool,
    /// The button pressed for our own timer, waiting for powerd.
    asked_at: Option<Instant>,
    /// The screensaver on the panel is powerd's (it will suspend).
    powerd_asleep: bool,
    /// After a wake powerd did not confirm: check its state then.
    check_at: Option<Instant>,
    resync_sent: bool,
    /// The next check for the framework's KAF source.
    kaf: KafCheck,
}

impl Sleep {
    /// Follow powerd unless explicitly disabled; a failed takeover aborts startup.
    /// ARISTOTLE_POWERD=0 retains the non-suspending screensaver fallback.
    pub fn start() -> std::io::Result<Sleep> {
        let wanted = enabled(std::env::var_os("ARISTOTLE_POWERD").as_deref());
        let (events, screensaver) = if wanted {
            let events = PowerEvents::listen()?;
            let screensaver = Screensaver::acquire(true)?;
            perf!("sleep: following powerd's events");
            (Some(events), screensaver)
        } else {
            eprintln!(
                "aristotle: ARISTOTLE_POWERD=0 disables CPU suspend; touch wakes the fallback"
            );
            (None, Screensaver::acquire(false)?)
        };
        Ok(Sleep {
            events,
            lipc: Lipc::start(),
            screensaver: Some(screensaver),
            // Restart T1 on the first tick. (Early in a boot the monotonic
            // clock may be below PING_ALWAYS.)
            last_ping: Instant::now()
                .checked_sub(PING_ALWAYS)
                .unwrap_or_else(Instant::now),
            touched: false,
            asked_at: None,
            powerd_asleep: false,
            check_at: None,
            resync_sent: false,
            kaf: KafCheck::new(Instant::now()),
        })
    }

    /// A Sleep for the host preview: no powerd, nothing changed.
    pub fn none() -> Sleep {
        Sleep {
            events: None,
            lipc: Lipc::start(),
            screensaver: None,
            last_ping: Instant::now(),
            touched: false,
            asked_at: None,
            powerd_asleep: false,
            check_at: None,
            resync_sent: false,
            kaf: KafCheck::new(Instant::now()),
        }
    }

    /// Whether powerd owns sleep (and so the frontlight around it).
    pub fn managed(&self) -> bool {
        self.events.is_some()
    }

    /// powerd's frontlight intensity (`0..=24`) when it owns sleep: the
    /// light the reader starts with, on powerd's own scale.
    pub fn light(&self) -> Option<u32> {
        if !self.managed() {
            return None;
        }
        hal::light_intensity().ok()
    }

    /// Hand the reader's light step to powerd when it owns sleep. powerd
    /// ramps the light off and back on around its screensaver to its own
    /// intensity; one it does not know (we set the hardware directly) made
    /// it ramp to 0 on wake and fight the app's own off/on (H-008).
    pub fn set_light(&self, step: u32) {
        if self.managed() {
            self.lipc.set_light_intensity(step);
        }
    }

    /// The descriptor to poll with the touchscreen.
    pub fn fd(&self) -> Option<RawFd> {
        self.events.as_ref().map(|e| e.fd())
    }

    /// powerd's events since the last call.
    pub fn events(&self) -> Vec<PowerEvent> {
        self.events.as_ref().map(|e| e.drain()).unwrap_or_default()
    }

    /// Whether the button was pressed for our timer and powerd has not
    /// answered yet.
    pub fn sleep_pending(&self) -> bool {
        self.asked_at.is_some()
    }

    /// A touch while awake.
    pub fn touched(&mut self) {
        self.touched = true;
    }

    /// Our own idle time is up. True: show the screensaver now (no powerd);
    /// false: wait for `goingToScreenSaver`.
    pub fn request_sleep(&mut self, now: Instant) -> bool {
        if !self.managed() {
            return true;
        }
        if self.asked_at.is_none() {
            perf!("sleep: idle time up, pressing powerd's button");
            self.lipc.power_button();
            self.asked_at = Some(now);
        }
        false
    }

    /// The screensaver is on the panel; `by_powerd` when powerd put it
    /// there.
    pub fn asleep(&mut self, by_powerd: bool) {
        self.resync_sent = false;
        self.powerd_asleep = by_powerd;
        self.asked_at = None;
    }

    /// powerd went to its screensaver over one of ours: it may suspend now.
    pub fn powerd_took_over(&mut self) {
        self.powerd_asleep = true;
    }

    /// Touch wakes only the non-suspending fallback.
    pub fn touch_asleep(&mut self, _now: Instant) -> bool {
        !self.powerd_asleep
    }

    /// The screen is back; `by_powerd` after `outOfScreenSaver`.
    pub fn awake(&mut self, by_powerd: bool, now: Instant) {
        if self.powerd_asleep && !by_powerd {
            // powerd may still be in its screensaver and suspend us.
            self.check_at = Some(now + Duration::from_millis(2500));
        }
        self.powerd_asleep = false;
        self.asked_at = None;
        self.touched = true;
    }

    /// powerd is about to suspend while the app is awake: it missed a wake.
    pub fn resync(&mut self) {
        if self.resync_sent {
            return;
        }
        self.resync_sent = true;
        perf!("sleep: powerd wants to suspend while awake; pressing its button");
        self.lipc.power_button();
    }

    /// Set warm frontlight step (`0..=24`) via powerd on dual-tone models.
    pub fn set_warmth(&self, step: u32) {
        self.lipc.set_warmth_intensity(step);
    }

    /// Enable or disable ambient-light auto-brightness via powerd.
    pub fn set_auto_brightness(&self, on: bool) {
        self.lipc.set_auto_brightness(on);
    }

    /// Defer kernel suspend by `seconds` while powerd is in `readyToSuspend`,
    /// keeping both Bluetooth (`btfd`) and Wi-Fi (`wifid`) awake for online TTS.
    pub fn defer_suspend(&self, seconds: u32) {
        if self.managed() {
            self.lipc.defer_suspend(seconds);
        }
    }

    /// Work due now: restarting powerd's idle timer while awake, and the
    /// fallbacks when powerd does not answer.
    pub fn tick(&mut self, now: Instant, asleep: bool) -> Action {
        if !self.managed() {
            return Action::None;
        }
        // The framework's KAF source: without it powerd ignores the power
        // button (FIX-108). The check runs while the app sleeps too: the
        // source has to be there when the device comes back.
        if self.kaf.due(now) {
            self.ensure_kaf();
        }
        if asleep {
            return Action::None;
        }
        if self
            .asked_at
            .is_some_and(|t| now.duration_since(t) >= SLEEP_WAIT)
        {
            self.asked_at = None;
            perf!("sleep: powerd ignored the button; screensaver without suspend");
            return Action::Sleep;
        }
        if self.check_at.is_some_and(|t| now >= t) {
            self.check_at = None;
            match hal::state() {
                Ok(s) if s != "active" => {
                    perf!("sleep: powerd still in state {s}; pressing its button");
                    self.lipc.power_button();
                }
                _ => {}
            }
        }
        let since = now.duration_since(self.last_ping);
        if (self.touched && since >= PING_TOUCHED) || since >= PING_ALWAYS {
            self.lipc.touch_idle_timer();
            self.last_ping = now;
            self.touched = false;
        }
        Action::None
    }

    /// The next time [`tick`](Self::tick) has work.
    pub fn deadline(&self, asleep: bool) -> Option<Instant> {
        if !self.managed() {
            return None;
        }
        // The KAF source is checked while the app sleeps too: the power button
        // has to work when the device comes back.
        let kaf = self.kaf.at;
        if asleep {
            return Some(kaf);
        }
        let ping = self.last_ping
            + if self.touched {
                PING_TOUCHED
            } else {
                PING_ALWAYS
            };
        [
            self.asked_at.map(|t| t + SLEEP_WAIT),
            self.check_at,
            Some(ping),
            Some(kaf),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Make sure the framework's KAF source is there.
    ///
    /// The original framework answers until it stops, a few seconds after a
    /// takeover; the shim takes the answer over then, and takes it back if
    /// it dies (FIX-108).
    fn ensure_kaf(&mut self) {
        let Some(screensaver) = self.screensaver.as_mut() else {
            return;
        };
        match screensaver.ensure_framework() {
            Ok(true) => perf!("sleep: the KAF shim took over for the framework"),
            Ok(false) => {}
            Err(error) => eprintln!("aristotle: the KAF shim: {error}"),
        }
    }

    /// Give the screensaver back to the stock system: blanket's module, and
    /// `preventScreenSaver` as it was.
    pub fn restore(&mut self) {
        self.events.take();
        self.lipc.shutdown();
        if let Some(state) = &mut self.screensaver {
            state.restore();
        }
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        self.restore();
    }
}

fn enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value != Some(std::ffi::OsStr::new("0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_sleep_is_default_with_an_explicit_zero_opt_out() {
        assert!(enabled(None));
        assert!(enabled(Some(std::ffi::OsStr::new("1"))));
        assert!(!enabled(Some(std::ffi::OsStr::new("0"))));
    }

    #[test]
    fn the_kaf_check_waits_two_seconds() {
        let start = Instant::now();
        let mut check = KafCheck::new(start);
        assert!(!check.due(start));
        assert!(!check.due(start + Duration::from_millis(1999)));
        assert!(check.due(start + Duration::from_secs(2)));
        // Not again right away: the loop does not wake up for it more often.
        assert!(!check.due(start + Duration::from_secs(2)));
        assert!(check.due(start + Duration::from_secs(4)));
    }
}
