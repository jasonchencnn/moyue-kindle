// SPDX-License-Identifier: AGPL-3.0-or-later
//! Laying the book out again after the typography changes.
//!
//! Laying a book out again takes seconds: crengine-ng lays the whole book
//! out again, and Plato's engine lays out the chapter the reader is in. That
//! is far longer than a reader waits for a screen, so the layout happens on
//! a worker thread and this module is the policy around that work:
//!
//! - changes made in quick succession are merged into one layout
//!   ([`DEBOUNCE`]), so a burst of taps on "bigger" lays the book out once;
//! - a layout that comes back after the typography changed again is out of
//!   date and is thrown away ([`Outcome::Discarded`]). The engine cannot be
//!   stopped halfway through a layout, so the only way out is to let it
//!   finish and drop the result;
//! - the layout that is wanted is the one the reader asked for last, and it
//!   is laid out with the text it was asked at, so the reader comes back to
//!   the same words in the new typography.
//!
//! The policy is pure: the caller passes the time in, so it can be tested
//! without waiting for anything.

use std::time::{Duration, Instant};

/// How long the reader waits after the last change of typography before the
/// book is laid out again. Long enough to swallow a burst of taps, short
/// enough not to feel slow.
pub const DEBOUNCE: Duration = Duration::from_millis(600);

/// What a finished layout means for the reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The layout is the one the reader asked for: use it.
    Ready,
    /// The typography changed while the book was being laid out: the result
    /// is out of date and is thrown away.
    Discarded,
}

/// Decides when the book is laid out again.
///
/// `K` says which requests are the same (the typography a layout is made
/// with), `T` is what the worker gets besides it (the text the layout is
/// asked at).
pub struct Scheduler<K, T> {
    /// The change waiting for the debounce to pass.
    pending: Option<Pending<K, T>>,
    /// The typography being laid out now.
    running: Option<K>,
    /// The typography the book is laid out with now. A change back to it
    /// needs no layout: the book is already that way.
    applied: Option<K>,
    /// The layout in flight is not wanted any more: the reader asked for
    /// the typography the book already has.
    unwanted: bool,
    /// How many changes were merged into one layout.
    merged: u32,
    /// How many layouts were thrown away as out of date.
    discarded: u32,
    /// How many layouts were started.
    layouts: u32,
}

/// A change waiting to be laid out.
struct Pending<K, T> {
    key: K,
    payload: T,
    at: Instant,
}

impl<K: Clone + PartialEq, T> Default for Scheduler<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Clone + PartialEq, T> Scheduler<K, T> {
    /// A scheduler with nothing asked of it.
    pub fn new() -> Self {
        Self {
            pending: None,
            running: None,
            applied: None,
            unwanted: false,
            merged: 0,
            discarded: 0,
            layouts: 0,
        }
    }

    /// The typography the book is laid out with now.
    pub fn set_applied(&mut self, key: K) {
        self.applied = Some(key);
    }

    /// A change of typography, asked at `now`, and the text it is asked at.
    ///
    /// A change made while the same typography is being laid out needs no
    /// layout of its own: the one running will do. A change back to the
    /// typography the book already has needs no layout at all.
    pub fn request(&mut self, key: K, payload: T, now: Instant) {
        if self.applied.as_ref() == Some(&key) {
            // The book is already laid out this way: the change waiting is
            // dropped, and a layout in flight is not wanted any more.
            self.pending = None;
            self.unwanted = true;
            return;
        }
        if self.running.as_ref() == Some(&key) {
            // The layout in flight is what the reader asks for.
            self.pending = None;
            self.unwanted = false;
            return;
        }
        self.unwanted = false;
        if self.pending.is_some() {
            self.merged += 1;
        }
        self.pending = Some(Pending {
            key,
            payload,
            at: now,
        });
    }

    /// The layout to start, if the debounce has passed and no other layout
    /// is running.
    pub fn poll(&mut self, now: Instant) -> Option<(K, T)> {
        if self.running.is_some() {
            return None;
        }
        let pending = self.pending.as_ref()?;
        if now.saturating_duration_since(pending.at) < DEBOUNCE {
            return None;
        }
        let pending = self.pending.take()?;
        self.running = Some(pending.key.clone());
        self.layouts += 1;
        Some((pending.key, pending.payload))
    }

    /// The layout came back.
    ///
    /// The result is the one the reader wants, unless the typography changed
    /// while the book was being laid out: then it is out of date and is
    /// thrown away, and the newest typography is laid out instead.
    pub fn finish(&mut self) -> Outcome {
        let Some(running) = self.running.take() else {
            return Outcome::Ready;
        };
        if self.unwanted {
            // The reader asked for the typography the book already has: the
            // layout that came back is not wanted.
            self.unwanted = false;
            self.discarded += 1;
            return Outcome::Discarded;
        }
        match &self.pending {
            Some(pending) if pending.key != running => {
                self.discarded += 1;
                Outcome::Discarded
            }
            Some(_) => {
                // The layout that came back is what the reader asked for
                // last: the change waiting behind it is the same one.
                self.pending = None;
                Outcome::Ready
            }
            None => Outcome::Ready,
        }
    }

    /// When a layout will start, if one is waiting for the debounce.
    pub fn deadline(&self) -> Option<Instant> {
        if self.running.is_some() {
            return None;
        }
        self.pending.as_ref().map(|pending| pending.at + DEBOUNCE)
    }

    /// Whether a layout is running.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Whether something is asked for: a change waiting or a layout running.
    pub fn busy(&self) -> bool {
        self.running.is_some() || self.pending.is_some()
    }

    /// How many changes were merged into one layout.
    pub fn merged(&self) -> u32 {
        self.merged
    }

    /// How many layouts were thrown away as out of date.
    pub fn discarded(&self) -> u32 {
        self.discarded
    }

    /// How many layouts were started.
    pub fn layouts(&self) -> u32 {
        self.layouts
    }

    /// Reset the counters of merged changes and discarded layouts.
    pub fn reset_stats(&mut self) {
        self.merged = 0;
        self.discarded = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typography a layout is made with.
    type Key = u32;

    /// The text a layout is asked at.
    type Anchor = &'static str;

    fn scheduler() -> Scheduler<Key, Anchor> {
        Scheduler::new()
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_burst_of_changes_is_merged_into_one_layout() {
        let t0 = Instant::now();
        let mut s = scheduler();
        // Five taps on "bigger", 100 ms apart.
        for i in 0..5u32 {
            s.request(i + 1, "page 12", at(t0, u64::from(i) * 100));
        }
        // Nothing starts before the debounce has passed: the last change
        // was at 400 ms, so the layout starts at 1000 ms.
        assert!(s.poll(at(t0, 400)).is_none());
        assert!(s.poll(at(t0, 999)).is_none());
        assert_eq!(s.merged(), 4);
        // After it, one layout, for the last change.
        let (key, anchor) = s.poll(at(t0, 1_000)).expect("the layout starts");
        assert_eq!(key, 5);
        assert_eq!(anchor, "page 12");
        assert_eq!(s.layouts(), 1);
        // And no second layout while that one runs.
        assert!(s.poll(at(t0, 5_000)).is_none());
        assert_eq!(s.layouts(), 1);
    }

    #[test]
    fn a_layout_that_comes_back_after_a_change_is_thrown_away() {
        let t0 = Instant::now();
        let mut s = scheduler();
        s.request(1, "page 12", t0);
        let (key, _) = s.poll(at(t0, 600)).expect("the layout starts");
        assert_eq!(key, 1);
        // The typography changes while the book is being laid out.
        s.request(2, "page 12", at(t0, 1_000));
        assert_eq!(s.finish(), Outcome::Discarded);
        assert_eq!(s.discarded(), 1);
        // The newest typography is laid out next.
        let (key, _) = s.poll(at(t0, 1_600)).expect("the layout starts");
        assert_eq!(key, 2);
        assert_eq!(s.finish(), Outcome::Ready);
        assert_eq!(s.layouts(), 2);
        assert_eq!(s.discarded(), 1);
    }

    #[test]
    fn a_layout_carries_the_text_it_was_asked_at() {
        let t0 = Instant::now();
        let mut s = scheduler();
        // The reader is on the page "12" and asks for bigger text.
        s.request(1, "page 12", t0);
        let (_, anchor) = s.poll(at(t0, 600)).expect("the layout starts");
        assert_eq!(anchor, "page 12");
        // The typography changes while the book is being laid out.
        s.request(2, "page 13", at(t0, 1_000));
        assert_eq!(s.finish(), Outcome::Discarded);
        // The next layout is asked at the text the reader is on now.
        let (key, anchor) = s.poll(at(t0, 1_600)).expect("the layout starts");
        assert_eq!(key, 2);
        assert_eq!(anchor, "page 13");
        assert_eq!(s.finish(), Outcome::Ready);
    }

    #[test]
    fn a_change_back_to_the_running_typography_needs_no_second_layout() {
        let t0 = Instant::now();
        let mut s = scheduler();
        s.request(1, "page 12", t0);
        let _ = s.poll(at(t0, 600)).expect("the layout starts");
        // The reader changes their mind and asks for what is being laid out.
        s.request(1, "page 12", at(t0, 700));
        assert!(s.deadline().is_none());
        assert_eq!(s.finish(), Outcome::Ready);
        assert_eq!(s.layouts(), 1);
        assert_eq!(s.discarded(), 0);
    }

    #[test]
    fn a_change_back_to_the_typography_of_the_panel_needs_no_layout() {
        let t0 = Instant::now();
        let mut s = scheduler();
        // The book is laid out with the typography 1.
        s.set_applied(1);
        // The reader asks for it again: nothing to do.
        s.request(1, "page 12", t0);
        assert!(s.poll(at(t0, 600)).is_none());
        assert_eq!(s.layouts(), 0);
        assert!(!s.busy());
    }

    #[test]
    fn a_layout_for_a_typography_the_reader_gave_up_on_is_thrown_away() {
        let t0 = Instant::now();
        let mut s = scheduler();
        s.set_applied(1);
        s.request(2, "page 12", t0);
        let _ = s.poll(at(t0, 600)).expect("the layout starts");
        // The reader changes their mind and asks for what is on the panel.
        s.request(1, "page 12", at(t0, 700));
        assert_eq!(s.finish(), Outcome::Discarded);
        // And nothing is laid out after it: the panel is already right.
        assert!(s.poll(at(t0, 1_300)).is_none());
        assert_eq!(s.layouts(), 1);
    }

    #[test]
    fn the_deadline_is_when_the_layout_starts() {
        let t0 = Instant::now();
        let mut s = scheduler();
        assert!(s.deadline().is_none());
        s.request(1, "page 12", t0);
        assert_eq!(s.deadline(), Some(at(t0, 600)));
        let _ = s.poll(at(t0, 600)).expect("the layout starts");
        // A layout is running: nothing else starts until it comes back.
        assert!(s.deadline().is_none());
        assert!(s.running());
        assert!(s.busy());
        assert_eq!(s.finish(), Outcome::Ready);
        assert!(!s.busy());
    }

    #[test]
    fn resetting_stats_clears_the_counters() {
        let t0 = Instant::now();
        let mut s = scheduler();
        s.request(1, "page 12", t0);
        s.request(2, "page 12", at(t0, 100));
        assert_eq!(s.merged(), 1);
        let _ = s.poll(at(t0, 700)).expect("the layout starts");
        s.request(3, "page 12", at(t0, 800));
        assert_eq!(s.finish(), Outcome::Discarded);
        assert_eq!(s.discarded(), 1);
        s.reset_stats();
        assert_eq!(s.merged(), 0);
        assert_eq!(s.discarded(), 0);
    }
}
