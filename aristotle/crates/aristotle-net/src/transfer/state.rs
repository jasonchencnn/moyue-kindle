// SPDX-License-Identifier: AGPL-3.0-or-later
//! How the transfer is going: what has arrived, how much is left and how
//! long it will take.
//!
//! The numbers the page shows come from here, so that both the device's
//! screen and the browser's page say the same thing about the same file.

use std::time::Duration;

/// How a transfer is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// The page is open, but nothing has come in yet.
    #[default]
    Waiting,
    /// A file is on its way.
    Receiving,
    /// Everything that was sent has arrived.
    Done,
}

impl Stage {
    /// The stage's number, for the three-part bar on the screen.
    pub fn index(self) -> usize {
        match self {
            Stage::Waiting => 0,
            Stage::Receiving => 1,
            Stage::Done => 2,
        }
    }
}

/// A file that has arrived in full.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    /// The name it was stored under.
    pub name: String,
    /// Its size in bytes.
    pub size: u64,
}

/// What the screen shows while a file is on its way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InFlight {
    /// The name of the file being written.
    pub name: String,
    /// How many bytes have arrived.
    pub received: u64,
    /// How many bytes the file has, when the browser said.
    pub total: u64,
    /// How long it has been coming.
    pub elapsed: Duration,
}

impl InFlight {
    /// The share of the file that has arrived, from 0 to 1.
    ///
    /// A file whose size the browser never said has no bar to fill, so it
    /// reads as empty rather than as a wrong number.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.received as f64 / self.total as f64).clamp(0.0, 1.0) as f32
    }

    /// The share of the file that has arrived, as a whole percent.
    pub fn percent(&self) -> u32 {
        (self.fraction() * 100.0).round() as u32
    }

    /// How long the rest of the file will take, if the speed holds.
    ///
    /// None when there is nothing left to wait for, or when the file has
    /// not been going long enough to guess from.
    pub fn remaining(&self) -> Option<Duration> {
        if self.total == 0 || self.received >= self.total {
            return None;
        }
        let secs = self.elapsed.as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        let rate = self.received as f64 / secs;
        if rate <= 0.0 {
            return None;
        }
        let left = (self.total - self.received) as f64 / rate;
        Some(Duration::from_secs_f64(left.max(0.0)))
    }
}

/// How many seconds left, rounded up, for the page.
pub fn seconds_left(remaining: Option<Duration>) -> Option<u64> {
    remaining.map(|d| {
        let secs = d.as_secs_f64().ceil();
        if secs <= 0.0 {
            0
        } else {
            secs as u64
        }
    })
}

/// The whole state of a transfer, as the screen shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Which stage the transfer is in.
    pub stage: Stage,
    /// The file on its way, if any.
    pub current: Option<InFlight>,
    /// The files that have arrived, newest last.
    pub received: Vec<Received>,
    /// The total size of everything that has arrived.
    pub total_bytes: u64,
}

impl State {
    /// A transfer that has just started.
    pub fn new() -> Self {
        Self {
            stage: Stage::Waiting,
            current: None,
            received: Vec::new(),
            total_bytes: 0,
        }
    }

    /// How many files have arrived.
    pub fn count(&self) -> usize {
        self.received.len()
    }

    /// Notes that a file has arrived in full.
    pub fn add_received(&mut self, name: String, size: u64) {
        self.total_bytes = self.total_bytes.saturating_add(size);
        self.received.push(Received { name, size });
        self.stage = Stage::Done;
    }

    /// Notes that the file on its way stopped before it was whole: the page
    /// goes back to what it said before that file started.
    pub fn abandon_current(&mut self) {
        self.current = None;
        self.stage = if self.received.is_empty() {
            Stage::Waiting
        } else {
            Stage::Done
        };
    }

    /// Notes that a file is on its way.
    pub fn set_current(&mut self, current: InFlight) {
        self.stage = Stage::Receiving;
        self.current = Some(current);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_numbers_match_the_bar() {
        assert_eq!(Stage::Waiting.index(), 0);
        assert_eq!(Stage::Receiving.index(), 1);
        assert_eq!(Stage::Done.index(), 2);
    }

    #[test]
    fn the_bar_follows_the_bytes() {
        let f = InFlight {
            name: "a.epub".into(),
            received: 31,
            total: 50,
            elapsed: Duration::from_secs(10),
        };
        assert!((f.fraction() - 0.62).abs() < 0.01);
        assert_eq!(f.percent(), 62);
    }

    #[test]
    fn a_file_of_unknown_size_has_no_bar() {
        let f = InFlight {
            name: "a.epub".into(),
            received: 100,
            total: 0,
            elapsed: Duration::from_secs(10),
        };
        assert_eq!(f.fraction(), 0.0);
        assert_eq!(f.percent(), 0);
        assert_eq!(f.remaining(), None);
    }

    #[test]
    fn the_rest_is_guessed_from_the_speed() {
        // Half a megabyte in ten seconds: the other half takes ten more.
        let f = InFlight {
            name: "a.epub".into(),
            received: 500_000,
            total: 1_000_000,
            elapsed: Duration::from_secs(10),
        };
        let left = f.remaining().unwrap();
        assert_eq!(seconds_left(Some(left)), Some(10));
    }

    #[test]
    fn a_file_that_has_not_moved_has_no_guess() {
        let f = InFlight {
            name: "a.epub".into(),
            received: 0,
            total: 1_000_000,
            elapsed: Duration::from_secs(10),
        };
        assert_eq!(f.remaining(), None);
    }

    #[test]
    fn a_file_that_stops_leaves_the_stage_it_found() {
        let flight = || InFlight {
            name: "a.txt".to_string(),
            received: 1,
            total: 2,
            elapsed: std::time::Duration::from_secs(1),
        };
        let mut s = State::new();
        s.set_current(flight());
        s.abandon_current();
        assert_eq!(s.stage, Stage::Waiting);
        assert!(s.current.is_none());
        s.add_received("b.txt".to_string(), 1);
        s.set_current(flight());
        s.abandon_current();
        assert_eq!(s.stage, Stage::Done);
        assert!(s.current.is_none());
    }

    #[test]
    fn a_finished_file_has_no_rest() {
        let f = InFlight {
            name: "a.epub".into(),
            received: 1_000_000,
            total: 1_000_000,
            elapsed: Duration::from_secs(10),
        };
        assert_eq!(f.remaining(), None);
    }

    #[test]
    fn the_state_counts_what_arrived() {
        let mut s = State::new();
        assert_eq!(s.stage, Stage::Waiting);
        assert_eq!(s.count(), 0);
        s.set_current(InFlight {
            name: "a.epub".into(),
            received: 1,
            total: 2,
            elapsed: Duration::from_secs(1),
        });
        assert_eq!(s.stage, Stage::Receiving);
        s.add_received("a.epub".into(), 500);
        assert_eq!(s.stage, Stage::Done);
        assert_eq!(s.count(), 1);
        assert_eq!(s.total_bytes, 500);
    }
}
