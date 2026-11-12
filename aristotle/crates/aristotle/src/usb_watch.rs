// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background reconciliation of the USB drive mode with volumd (USB-001).
//!
//! Every query of volumd forks `lipc-get-prop`, which can take seconds when
//! the daemon is busy (see `aristotle-hal`'s `lipc_command`). Such a query
//! must never run in the input loop, so a thread of its own polls once a
//! second in drive mode and once every thirty seconds otherwise, and wakes
//! the input loop (through the wake eventfd) only when what volumd reports
//! disagrees with the mode the app is in.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use aristotle_hal::powerd;
use aristotle_screen::usb::{poll_interval, reconcile, Status, UsbMode};

use crate::wake;

/// The state the worker shares with the app.
struct Shared {
    mode: AtomicU8,
    stop: AtomicBool,
    /// When the last poll was made: the next one is an interval after it.
    last_poll: Mutex<Instant>,
    /// Set to make the worker recompute its deadline (a mode change).
    notified: Mutex<bool>,
    condvar: Condvar,
}

impl Shared {
    fn mode(&self) -> UsbMode {
        mode_of(self.mode.load(Ordering::Acquire))
    }

    fn set_mode(&self, mode: UsbMode) {
        if self.mode.swap(code(mode), Ordering::AcqRel) != code(mode) {
            self.nudge();
        }
    }

    fn nudge(&self) {
        let mut notified = self.notified.lock().unwrap_or_else(|e| e.into_inner());
        *notified = true;
        drop(notified);
        self.condvar.notify_all();
    }

    /// Wait until the next poll is due; `true` when the watch is stopping.
    fn wait(&self) -> bool {
        let mut notified = self.notified.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.stop.load(Ordering::Acquire) {
                return true;
            }
            *notified = false;
            let last = *self.last_poll.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let due = last + poll_interval(self.mode());
            if now >= due {
                return false;
            }
            let (guard, _) = self
                .condvar
                .wait_timeout(notified, due - now)
                .unwrap_or_else(|e| e.into_inner());
            notified = guard;
        }
    }
}

fn code(mode: UsbMode) -> u8 {
    match mode {
        UsbMode::Disconnected => 0,
        UsbMode::DriveMode => 1,
    }
}

fn mode_of(raw: u8) -> UsbMode {
    if raw == code(UsbMode::DriveMode) {
        UsbMode::DriveMode
    } else {
        UsbMode::Disconnected
    }
}

/// A thread polling volumd for drive mode changes the app missed.
pub struct UsbWatch {
    shared: Arc<Shared>,
    rx: mpsc::Receiver<(UsbMode, Status)>,
    worker: Option<JoinHandle<()>>,
}

fn query() -> Status {
    Status {
        drive_mode: powerd::drive_mode_state().ok(),
        userstore_available: powerd::is_userstore_available().ok(),
    }
}

impl UsbWatch {
    /// Start watching from `mode`.
    pub fn new(mode: UsbMode) -> UsbWatch {
        Self::start(mode, query)
    }

    fn start<F>(mode: UsbMode, mut poll: F) -> UsbWatch
    where
        F: FnMut() -> Status + Send + 'static,
    {
        let (tx, rx) = wake::channel();
        let shared = Arc::new(Shared {
            mode: AtomicU8::new(code(mode)),
            stop: AtomicBool::new(false),
            // Poll once at startup, then on the interval: the app already
            // queried volumd, and this repeats the check off the loop.
            last_poll: Mutex::new(Instant::now() - poll_interval(UsbMode::Disconnected)),
            notified: Mutex::new(false),
            condvar: Condvar::new(),
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("usb-watch".into())
            .spawn(move || loop {
                if worker_shared.wait() {
                    return;
                }
                let mode = worker_shared.mode();
                let status = poll();
                *worker_shared
                    .last_poll
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Instant::now();
                // The send wakes the input loop's poll; a result it cannot
                // use (the mode moved on) is dropped there.
                if reconcile(mode, status).is_some() && tx.send((mode, status)).is_err() {
                    return;
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                eprintln!("aristotle: starting the USB watch thread: {error}");
                None
            }
        };
        UsbWatch { shared, rx, worker }
    }

    /// Tell the watch which mode the app is in: the poll cadence follows.
    pub fn set_mode(&self, mode: UsbMode) {
        self.shared.set_mode(mode);
    }

    /// A poll that disagreed with the mode it was made in.
    pub fn try_recv(&self) -> Option<(UsbMode, Status)> {
        self.rx.try_recv().ok()
    }
}

impl Drop for UsbWatch {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.nudge();
        // The worker may be inside a lipc query with a five second
        // deadline: detach it rather than make quitting wait for it.
        self.worker.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aristotle_screen::usb::UsbEvent;
    use std::time::{Duration, Instant};

    /// A poll that says what the test wants it to say.
    fn status(drive_mode: Option<i32>, userstore_available: Option<bool>) -> Status {
        Status {
            drive_mode,
            userstore_available,
        }
    }

    impl UsbWatch {
        fn with_poll<F>(mode: UsbMode, poll: F) -> UsbWatch
        where
            F: FnMut() -> Status + Send + 'static,
        {
            UsbWatch::start(mode, poll)
        }
    }

    #[test]
    fn the_watch_reports_a_transition_from_its_thread() {
        let watch = UsbWatch::with_poll(UsbMode::Disconnected, || status(Some(1), Some(true)));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reported = None;
        while Instant::now() < deadline {
            if let Some(event) = watch.try_recv() {
                reported = Some(event);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let (mode, polled) = reported.expect("the watch reports the transition");
        assert_eq!(mode, UsbMode::Disconnected);
        assert_eq!(reconcile(mode, polled), Some(UsbEvent::DriveModeStarted));
    }
}
