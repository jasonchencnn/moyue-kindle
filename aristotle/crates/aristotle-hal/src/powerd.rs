// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sleep through the stock power daemon, `powerd`: its screensaver and
//! suspend events, and the few properties an app sets, over LIPC.
//!
//! Aristotle is static musl and cannot link `liblipc.so`, so it runs the
//! stock command-line tools, as KOReader does for the events
//! (`lipc-wait-event -m` in a child, read line by line) [ko]:
//!
//! - [`PowerEvents`] follows `com.lab126.powerd` from a child process and a
//!   reader thread, and exposes a file descriptor to add to the input
//!   loop's `poll`, so an event wakes the loop at once;
//! - [`Lipc`] sets properties from a thread of its own, so the input loop
//!   never waits for a `lipc-set-prop`.
//!
//! The flow, read from powerd's strings and logs on our PW4 [dev]: a press
//! of the power button (or `powerButton 1`, or the idle timer T1, 600 s)
//! moves powerd from `active` to `screenSaver` and emits
//! `goingToScreenSaver`; the app draws its screensaver. About 60 s later
//! (T2) it emits `readyToSuspend` levels before suspend;
//! the e-ink picture stays. The next press emits `outOfScreenSaver` and the
//! app redraws. `touchScreenSaverTimeout 1` restarts T1. powerd ignores the
//! button while `preventScreenSaver` is 1 or its drive-mode enum is 3 [firmware].
//! Drive-mode enum 1 means off; an ignored button during USB networking did not prove its cause [dev].
//! Firmware also requires KAF frameworkStarted and no blanket splash module.

use std::io::{self, BufRead, BufReader};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const POWERD: &str = "com.lab126.powerd";
const VOLUMD: &str = "com.lab126.volumd";
const BLANKET: &str = "com.lab126.blanket";

/// The events [`PowerEvents`] asks powerd for.
const POWERD_EVENTS: &str =
    "goingToScreenSaver,outOfScreenSaver,exitingScreenSaver,readyToSuspend,\
                             suspending,wakeupFromSuspend,charging,notCharging";

/// The events [`PowerEvents`] asks volumd for (USB mass storage and userstore availability).
const VOLUMD_EVENTS: &str =
    "driveModeStateChanged,userstoreAvailable,userstoreUnAvailable,userstoreIsLikelyToUnMount";

/// A powerd event. The number is the event's first argument: for the
/// screensaver events the source (1 button wake, 2 button, 3 idle timeout,
/// 4 cover close, 6 cover open [device logs]); source 9 is unidentified.
/// `readyToSuspend` carries a level, not seconds (10, 8, 7, 6, 2, 1 at about 5 s intervals [device logs]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerEvent {
    GoingToScreenSaver(i32),
    OutOfScreenSaver(i32),
    ExitingScreenSaver,
    ReadyToSuspend(i32),
    Suspending,
    WakeupFromSuspend,
    Charging,
    NotCharging,
    DriveMode(bool),
    UserstoreAvailable,
    UserstoreUnavailable,
    UserstoreLikelyToUnmount,
}

/// One line of `lipc-wait-event` output: the event name, then its
/// arguments separated by spaces. Unknown events are `None`.
pub fn parse_event(line: &str) -> Option<PowerEvent> {
    let mut it = line.split_whitespace();
    let name = it.next()?;
    let arg = it
        .next()
        .and_then(|a| a.trim_matches('"').parse::<i32>().ok())
        .unwrap_or(0);
    Some(match name {
        "goingToScreenSaver" => PowerEvent::GoingToScreenSaver(arg),
        "outOfScreenSaver" => PowerEvent::OutOfScreenSaver(arg),
        "exitingScreenSaver" => PowerEvent::ExitingScreenSaver,
        "readyToSuspend" => PowerEvent::ReadyToSuspend(arg),
        "suspending" => PowerEvent::Suspending,
        "wakeupFromSuspend" => PowerEvent::WakeupFromSuspend,
        "charging" => PowerEvent::Charging,
        "notCharging" => PowerEvent::NotCharging,
        "driveModeStateChanged" => PowerEvent::DriveMode(arg > 0),
        "userstoreAvailable" => PowerEvent::UserstoreAvailable,
        "userstoreUnAvailable" => PowerEvent::UserstoreUnavailable,
        "userstoreIsLikelyToUnMount" => PowerEvent::UserstoreLikelyToUnmount,
        _ => return None,
    })
}

/// powerd and volumd events, from `lipc-wait-event -m` children that threads read.
/// Dropping it stops the children.
pub struct PowerEvents {
    rx: Receiver<PowerEvent>,
    /// Readable whenever an event is queued (one byte per event).
    wake: OwnedFd,
    child: Arc<Mutex<ListenerChild>>,
    stop: Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
    volumd_child: Option<Arc<Mutex<ListenerChild>>>,
    volumd_stop: Option<Sender<()>>,
    volumd_worker: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct ListenerChild {
    process: Option<Child>,
    stopped: bool,
}

impl ListenerChild {
    fn reap(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_powerd() -> io::Result<Child> {
    Command::new("lipc-wait-event")
        .args(["-m", "-s", "0", POWERD, POWERD_EVENTS])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

fn spawn_volumd() -> io::Result<Child> {
    Command::new("lipc-wait-event")
        .args(["-m", "-s", "0", VOLUMD, VOLUMD_EVENTS])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

impl PowerEvents {
    /// Start listening to powerd and volumd. Fails off the device (no `lipc-wait-event`).
    /// If a child dies later (lipcd restarting, say), the thread starts another
    /// after a pause.
    pub fn listen() -> io::Result<PowerEvents> {
        Self::listen_pair(spawn_powerd, Some(spawn_volumd), Duration::from_secs(5))
    }

    pub fn listen_with<F>(spawn: F, retry: Duration) -> io::Result<PowerEvents>
    where
        F: FnMut() -> io::Result<Child> + Send + 'static,
    {
        Self::listen_pair(spawn, None::<fn() -> io::Result<Child>>, retry)
    }

    fn listen_pair<F1, F2>(
        mut spawn1: F1,
        spawn2: Option<F2>,
        retry: Duration,
    ) -> io::Result<PowerEvents>
    where
        F1: FnMut() -> io::Result<Child> + Send + 'static,
        F2: FnMut() -> io::Result<Child> + Send + 'static,
    {
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills the two-element array on success.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just created and are owned here.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let child = Arc::new(Mutex::new(ListenerChild {
            process: Some(spawn1()?),
            stopped: false,
        }));
        let (tx, rx) = mpsc::channel();
        let (stop, halt) = mpsc::channel();
        let process = child.clone();
        let write_primary = match write.try_clone() {
            Ok(w) => w,
            Err(error) => {
                child.lock().unwrap().reap();
                return Err(error);
            }
        };
        let tx_primary = tx.clone();
        let worker = match thread::Builder::new()
            .name("powerd-events".into())
            .spawn(move || read_events(tx_primary, write_primary, process, halt, spawn1, retry))
        {
            Ok(worker) => worker,
            Err(error) => {
                child.lock().unwrap().reap();
                return Err(error);
            }
        };

        let (volumd_child, volumd_stop, volumd_worker) = if let Some(mut spawn2) = spawn2 {
            let initial_proc = match spawn2() {
                Ok(child_proc) => Some(child_proc),
                Err(e) => {
                    eprintln!("aristotle: initial volumd listener spawn failed ({e}), worker will retry");
                    None
                }
            };
            let child2 = Arc::new(Mutex::new(ListenerChild {
                process: initial_proc,
                stopped: false,
            }));
            let (stop2, halt2) = mpsc::channel();
            let process2 = child2.clone();
            let tx2 = tx.clone();
            let write_sec = match write.try_clone() {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("aristotle: cloning wake pipe for volumd: {e}");
                    child2.lock().unwrap().reap();
                    return Ok(PowerEvents {
                        rx,
                        wake: read,
                        child,
                        stop,
                        worker: Some(worker),
                        volumd_child: None,
                        volumd_stop: None,
                        volumd_worker: None,
                    });
                }
            };
            let worker2 = thread::Builder::new()
                .name("volumd-events".into())
                .spawn(move || read_events(tx2, write_sec, process2, halt2, spawn2, retry));
            match worker2 {
                Ok(w) => (Some(child2), Some(stop2), Some(w)),
                Err(e) => {
                    eprintln!("aristotle: starting volumd listener thread: {e}");
                    child2.lock().unwrap().reap();
                    (None, None, None)
                }
            }
        } else {
            (None, None, None)
        };

        Ok(PowerEvents {
            rx,
            wake: read,
            child,
            stop,
            worker: Some(worker),
            volumd_child,
            volumd_stop,
            volumd_worker,
        })
    }

    /// The descriptor to poll with the touchscreen.
    pub fn fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    /// The events received since the last call.
    pub fn drain(&self) -> Vec<PowerEvent> {
        let mut buf = [0u8; 64];
        // SAFETY: reads into a local buffer from our own non-blocking pipe.
        while unsafe { libc::read(self.wake.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {
        }
        self.rx.try_iter().collect()
    }
}

impl Drop for PowerEvents {
    fn drop(&mut self) {
        {
            let mut child = self.child.lock().unwrap();
            child.stopped = true;
            child.reap();
        }
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(volumd_child) = &self.volumd_child {
            let mut child = volumd_child.lock().unwrap();
            child.stopped = true;
            child.reap();
        }
        if let Some(stop) = self.volumd_stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.volumd_worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_events<F>(
    tx: Sender<PowerEvent>,
    wake: OwnedFd,
    child: Arc<Mutex<ListenerChild>>,
    stop: Receiver<()>,
    mut spawn: F,
    retry: Duration,
) where
    F: FnMut() -> io::Result<Child>,
{
    loop {
        let stdout = {
            let mut child = child.lock().unwrap();
            child.process.as_mut().and_then(|c| c.stdout.take())
        };
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if let Some(ev) = parse_event(&line) {
                    if tx.send(ev).is_err() {
                        child.lock().unwrap().reap();
                        return;
                    }
                    // SAFETY: one byte to our nonblocking wake pipe; EAGAIN means it is already readable.
                    unsafe { libc::write(wake.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
                }
            }
        }
        {
            let mut child = child.lock().unwrap();
            child.reap();
            if child.stopped {
                return;
            }
        }
        if !matches!(
            stop.recv_timeout(retry),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            return;
        }
        let mut child = child.lock().unwrap();
        if child.stopped {
            return;
        }
        match spawn() {
            Ok(process) => child.process = Some(process),
            Err(error) => eprintln!("aristotle: restarting powerd listener: {error}"),
        }
    }
}

/// `lipc-set-prop`, waiting for it. `-i` for integer values.
pub fn set_prop(service: &str, prop: &str, value: &str, int: bool) -> io::Result<()> {
    let mut cmd = Command::new("lipc-set-prop");
    if int {
        cmd.arg("-i");
    }
    let status = crate::lipc_command::run(cmd.args([service, prop, value]))?.status;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "lipc-set-prop {service} {prop} {value}: {status}"
        )))
    }
}

/// powerd's state (`active`, `screenSaver`, `readyToSuspend`, ...).
pub fn state() -> io::Result<String> {
    get_prop(POWERD, "state", false)
}

/// powerd's frontlight intensity, `0..=24` (`flIntensity`), waiting for
/// `lipc-get-prop`.
pub fn light_intensity() -> io::Result<u32> {
    get_prop(POWERD, "flIntensity", true)?
        .trim()
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("flIntensity: {e}")))
}

pub(crate) fn get_prop(service: &str, prop: &str, int: bool) -> io::Result<String> {
    let mut command = Command::new("lipc-get-prop");
    command
        .arg(if int { "-i" } else { "-s" })
        .args([service, prop]);
    let out = crate::lipc_command::run(&mut command)?;
    if !out.status.success() {
        return Err(io::Error::other(format!("lipc-get-prop: {}", out.status)));
    }
    String::from_utf8(out.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Load (`true`) or unload a blanket module, such as `screensaver`, the
/// stock screensaver that would draw over the app. Waits.
pub fn blanket(load: bool, module: &str) -> io::Result<()> {
    set_prop(BLANKET, if load { "load" } else { "unload" }, module, false)
}

/// The current driveModeState from volumd (`0` = inactive, `1` = active drive mode).
pub fn drive_mode_state() -> io::Result<i32> {
    get_prop(VOLUMD, "driveModeState", true)?
        .trim()
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("driveModeState: {e}")))
}

/// Whether volumd reports the userstore (`/mnt/us`) is mounted and available.
pub fn is_userstore_available() -> io::Result<bool> {
    let val: i32 = get_prop(VOLUMD, "userstoreIsAvailable", true)?
        .trim()
        .parse()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("userstoreIsAvailable: {e}"),
            )
        })?;
    Ok(val != 0)
}

type Property = (String, String, String, bool);

/// Bounded requests to powerd, run in order and cancelled when shutting down.
pub struct Lipc {
    tx: Option<SyncSender<Property>>,
    worker: Option<thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Default for Lipc {
    fn default() -> Self {
        Self::start()
    }
}

impl Lipc {
    pub fn start() -> Lipc {
        Self::start_with(set_prop)
    }

    fn start_with<F>(mut set: F) -> Lipc
    where
        F: FnMut(&str, &str, &str, bool) -> io::Result<()> + Send + 'static,
    {
        let (tx, rx) = mpsc::sync_channel::<Property>(8);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new().name("lipc".into()).spawn(move || {
            for (service, prop, value, int) in rx {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                if let Err(e) = set(&service, &prop, &value, int) {
                    eprintln!("aristotle: {e}");
                }
            }
        });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                eprintln!("aristotle: starting LIPC worker: {error}");
                None
            }
        };
        Lipc {
            tx: Some(tx),
            worker,
            stop,
        }
    }

    fn send(&self, service: &str, prop: &str, value: &str, int: bool) {
        if let Some(tx) = &self.tx {
            if let Err(error) = tx.try_send((service.into(), prop.into(), value.into(), int)) {
                eprintln!("aristotle: queueing LIPC request: {error}");
            }
        }
    }

    /// Cancel queued commands and wait for the bounded in-flight command.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    /// Restart powerd's idle timer (T1), as a touch on the stock UI does.
    pub fn touch_idle_timer(&self) {
        self.send(POWERD, "touchScreenSaverTimeout", "1", true);
    }

    /// The same as a press of the power button.
    pub fn power_button(&self) {
        self.send(POWERD, "powerButton", "1", true);
    }

    /// Set powerd's frontlight intensity, `0..=24` as on the stock UI
    /// (`flMaxIntensity` is 24 on our Oasis 2 [dev]). powerd sets the light
    /// and ramps it down and back up around its screensaver to this value.
    pub fn set_light_intensity(&self, step: u32) {
        self.send(POWERD, "flIntensity", &step.to_string(), true);
    }
}

impl Drop for Lipc {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lipc_shutdown_cancels_queued_commands() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let (started, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let mut lipc = Lipc::start_with(move |_, prop, _, _| {
            recorded.lock().unwrap().push(prop.to_string());
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(())
        });
        lipc.touch_idle_timer();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        lipc.power_button();
        let stop = lipc.stop.clone();
        let shutdown = thread::spawn(move || {
            lipc.shutdown();
            lipc.power_button();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !stop.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
        release.send(()).unwrap();
        shutdown.join().unwrap();
        assert_eq!(*calls.lock().unwrap(), ["touchScreenSaverTimeout"]);
    }

    fn listener(script: &'static str, retry: Duration) -> PowerEvents {
        PowerEvents::listen_with(
            move || {
                Command::new("sh")
                    .args(["-c", script])
                    .stdout(Stdio::piped())
                    .spawn()
            },
            retry,
        )
        .unwrap()
    }

    fn receive(events: &PowerEvents) -> Vec<PowerEvent> {
        let mut fd = libc::pollfd {
            fd: events.fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll receives one initialized descriptor for a bounded wait.
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 2000) }, 1);
        events.drain()
    }

    #[test]
    fn dropping_listener_reaps_its_child() {
        let events = listener(
            "printf 'goingToScreenSaver 2\n'; exec sleep 60",
            Duration::from_secs(5),
        );
        let pid = events.child.lock().unwrap().process.as_ref().unwrap().id();
        assert_eq!(receive(&events), vec![PowerEvent::GoingToScreenSaver(2)]);
        drop(events);
        // SAFETY: WNOHANG only inspects the former child; it cannot block.
        assert_eq!(
            unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn listener_restarts_after_exit() {
        let events = listener("printf 'outOfScreenSaver 1\n'", Duration::from_millis(10));
        assert!(receive(&events).contains(&PowerEvent::OutOfScreenSaver(1)));
        assert!(receive(&events).contains(&PowerEvent::OutOfScreenSaver(1)));
    }

    #[test]
    fn listener_retries_a_failed_restart() {
        let mut attempts = 0;
        let events = PowerEvents::listen_with(
            move || {
                let attempt = attempts;
                attempts += 1;
                if attempt == 1 {
                    return Err(io::Error::other("temporary listener failure"));
                }
                Command::new("sh")
                    .args(["-c", "printf 'charging\n'"])
                    .stdout(Stdio::piped())
                    .spawn()
            },
            Duration::from_millis(10),
        )
        .unwrap();
        assert!(receive(&events).contains(&PowerEvent::Charging));
        assert!(receive(&events).contains(&PowerEvent::Charging));
    }

    #[test]
    fn dropping_listener_interrupts_restart_delay() {
        let events = listener("printf 'readyToSuspend 5\n'", Duration::from_secs(60));
        assert_eq!(receive(&events), vec![PowerEvent::ReadyToSuspend(5)]);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while events.child.lock().unwrap().process.is_some() {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let started = std::time::Instant::now();
        drop(events);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn missing_listener_does_not_start_a_worker() {
        let result = PowerEvents::listen_with(
            || Err(io::Error::new(io::ErrorKind::NotFound, "test listener")),
            Duration::from_secs(5),
        );
        assert!(matches!(result, Err(e) if e.kind() == io::ErrorKind::NotFound));
    }

    #[test]
    fn events() {
        assert_eq!(
            parse_event("goingToScreenSaver 2"),
            Some(PowerEvent::GoingToScreenSaver(2))
        );
        assert_eq!(
            parse_event("outOfScreenSaver 1\n"),
            Some(PowerEvent::OutOfScreenSaver(1))
        );
        assert_eq!(
            parse_event("readyToSuspend 5"),
            Some(PowerEvent::ReadyToSuspend(5))
        );
        assert_eq!(
            parse_event("exitingScreenSaver"),
            Some(PowerEvent::ExitingScreenSaver)
        );
        assert_eq!(
            parse_event("wakeupFromSuspend 1760000000"),
            Some(PowerEvent::WakeupFromSuspend)
        );
        assert_eq!(
            parse_event("driveModeStateChanged 1"),
            Some(PowerEvent::DriveMode(true))
        );
        assert_eq!(
            parse_event("driveModeStateChanged 0"),
            Some(PowerEvent::DriveMode(false))
        );
        assert_eq!(
            parse_event("userstoreAvailable"),
            Some(PowerEvent::UserstoreAvailable)
        );
        assert_eq!(
            parse_event("userstoreUnAvailable"),
            Some(PowerEvent::UserstoreUnavailable)
        );
        assert_eq!(
            parse_event("userstoreIsLikelyToUnMount"),
            Some(PowerEvent::UserstoreLikelyToUnmount)
        );
        assert_eq!(parse_event("battLevelChanged 80"), None);
        assert_eq!(parse_event(""), None);
    }
}
