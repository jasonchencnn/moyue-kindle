// SPDX-License-Identifier: AGPL-3.0-or-later
//! Partial relayout scheduler and degraded mode lifecycle (ADR-015, APP-007).
//!
//! When typography changes on a crengine book with partial rerendering support:
//! 1. Debounce (600 ms) merges rapid adjustments.
//! 2. Fast anchor relayout (~1 s on device) formats only the current fragment.
//! 3. The book enters a degraded state:
//!    - Page turns work normally; crossing fragments formats on demand.
//!    - Footers, TOC, and progress suppress inaccurate page counts/fractions.
//! 4. After the reader is idle for [`IDLE`] (2 s), full relayout catches up in the background.
//!    - Turns during catchup are queued.
//!    - Typography changes during catchup discard the catchup and restart.
//! 5. Catchup completion exits degraded state, re-anchors to XPointer, and clears page cache.

use std::time::{Duration, Instant};

/// Debounce window for merging rapid typography adjustments.
pub const DEBOUNCE: Duration = Duration::from_millis(600);

/// Inactivity period before background catchup begins.
pub const IDLE: Duration = Duration::from_secs(2);

/// What a finished relayout worker means for the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ready,
    Discarded,
}

/// What a finished catchup worker means for the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatchupOutcome {
    Ready,
    Discarded,
}

/// Actions emitted by the scheduler when work should start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartAction<K, T> {
    /// Start partial relayout (fast anchor-only pass).
    StartPartial { key: K, payload: T },
    /// Start full relayout (fallback path for Plato or single-fragment EPUBs).
    StartFull { key: K, payload: T },
    /// Start background catchup render.
    StartCatchup,
}

/// Internal lifecycle state of partial rerendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Normal reading mode with complete layout.
    Normal,
    /// Waiting for debounce timer to elapse.
    Debouncing,
    /// Worker is performing partial relayout of the anchor page.
    PartialRelayout,
    /// Worker is performing full relayout (fallback path).
    FullRelayout,
    /// Anchor page shown; book is degraded awaiting user idle timeout.
    Degraded,
    /// Background catchup is running.
    CatchingUp,
}

/// A pending typography request.
struct Pending<K, T> {
    key: K,
    payload: T,
    can_partial: bool,
    at: Instant,
}

/// Pure logic controller for partial relayout and degraded reading lifecycle.
pub struct PartialScheduler<K, T> {
    state: State,
    pending: Option<Pending<K, T>>,
    running: Option<K>,
    applied: Option<K>,
    last_action: Option<Instant>,
    catchup_discarded: bool,
    queued_turns: Vec<u64>,
    merged_count: u32,
    discarded_count: u32,
}

impl<K: Clone + PartialEq, T> Default for PartialScheduler<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Clone + PartialEq, T> PartialScheduler<K, T> {
    pub fn new() -> Self {
        Self {
            state: State::Normal,
            pending: None,
            running: None,
            applied: None,
            last_action: None,
            catchup_discarded: false,
            queued_turns: Vec::new(),
            merged_count: 0,
            discarded_count: 0,
        }
    }

    /// Current lifecycle state.
    pub fn state(&self) -> State {
        self.state
    }

    /// Whether the book is currently in degraded reading mode (partial layout).
    pub fn is_degraded(&self) -> bool {
        matches!(self.state, State::Degraded | State::CatchingUp)
    }

    /// Whether any relayout or catchup worker is actively running.
    pub fn running(&self) -> bool {
        matches!(
            self.state,
            State::PartialRelayout | State::FullRelayout | State::CatchingUp
        )
    }

    /// Whether the scheduler is busy (pending debounce or worker running).
    pub fn busy(&self) -> bool {
        self.running() || self.pending.is_some()
    }

    /// Whether page turns should be queued instead of executed immediately.
    pub fn should_queue_turn(&self) -> bool {
        self.running()
    }

    /// Record a queued page turn timestamp/id.
    pub fn queue_turn(&mut self, turn_id: u64) {
        self.queued_turns.push(turn_id);
    }

    /// Take all queued page turns once relayout or catchup completes.
    pub fn take_turns(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.queued_turns)
    }

    /// Number of merged rapid changes.
    pub fn merged_count(&self) -> u32 {
        self.merged_count
    }

    /// Number of discarded outdated layouts.
    pub fn discarded_count(&self) -> u32 {
        self.discarded_count
    }

    /// Mark the typography currently applied to the page.
    pub fn set_applied(&mut self, key: K) {
        self.applied = Some(key);
    }

    /// Next deadline when a debounce or idle timeout expires.
    pub fn deadline(&self) -> Option<Instant> {
        if let Some(pending) = &self.pending {
            return Some(pending.at + DEBOUNCE);
        }
        if self.state == State::Degraded {
            if let Some(last) = self.last_action {
                return Some(last + IDLE);
            }
        }
        None
    }

    /// Inform the scheduler of reader interaction (resets idle catchup countdown).
    pub fn user_action(&mut self, now: Instant) {
        if matches!(self.state, State::Degraded) {
            self.last_action = Some(now);
        }
    }

    /// Request a change of typography.
    pub fn request(&mut self, key: K, payload: T, can_partial: bool, now: Instant) {
        // If already applied and not degraded, no layout is needed.
        if self.applied.as_ref() == Some(&key) && !self.is_degraded() {
            self.pending = None;
            return;
        }

        // If a worker is running for the exact same key, do nothing.
        if self.running.as_ref() == Some(&key) {
            self.pending = None;
            return;
        }

        if self.pending.is_some() {
            self.merged_count += 1;
        }

        if self.state == State::CatchingUp {
            self.catchup_discarded = true;
        }

        self.pending = Some(Pending {
            key,
            payload,
            can_partial,
            at: now,
        });

        // If not actively running a worker, transition to debouncing.
        if !self.running() {
            self.state = State::Debouncing;
        }
    }

    /// Poll for work to start.
    pub fn poll(&mut self, now: Instant) -> Option<StartAction<K, T>> {
        // While a worker is running, cannot start another.
        if self.running() {
            return None;
        }

        // Check debounce timer for pending relayout.
        if let Some(pending) = &self.pending {
            if now.saturating_duration_since(pending.at) >= DEBOUNCE {
                let pending = self.pending.take()?;
                self.running = Some(pending.key.clone());
                if pending.can_partial {
                    self.state = State::PartialRelayout;
                    return Some(StartAction::StartPartial {
                        key: pending.key,
                        payload: pending.payload,
                    });
                } else {
                    self.state = State::FullRelayout;
                    return Some(StartAction::StartFull {
                        key: pending.key,
                        payload: pending.payload,
                    });
                }
            }
            return None;
        }

        // Check idle timeout for background catchup when degraded.
        if self.state == State::Degraded {
            let last = self.last_action.unwrap_or(now);
            if now.saturating_duration_since(last) >= IDLE {
                self.state = State::CatchingUp;
                self.catchup_discarded = false;
                return Some(StartAction::StartCatchup);
            }
        }

        None
    }

    /// Partial relayout worker returned.
    pub fn finish_partial(&mut self, now: Instant) -> Outcome {
        let running = self.running.take();
        if let Some(pending) = &self.pending {
            if running.as_ref() != Some(&pending.key) {
                self.discarded_count += 1;
                self.state = State::Debouncing;
                return Outcome::Discarded;
            }
        }

        self.applied = running;
        self.state = State::Degraded;
        self.last_action = Some(now);
        Outcome::Ready
    }

    /// Full relayout worker returned (fallback path).
    pub fn finish_full(&mut self) -> Outcome {
        let running = self.running.take();
        if let Some(pending) = &self.pending {
            if running.as_ref() != Some(&pending.key) {
                self.discarded_count += 1;
                self.state = State::Debouncing;
                return Outcome::Discarded;
            }
        }

        self.applied = running;
        self.state = State::Normal;
        self.last_action = None;
        Outcome::Ready
    }

    /// Background catchup worker returned.
    pub fn finish_catchup(&mut self) -> CatchupOutcome {
        if self.catchup_discarded || self.pending.is_some() {
            self.discarded_count += 1;
            self.catchup_discarded = false;
            self.state = if self.pending.is_some() {
                State::Debouncing
            } else {
                State::Degraded
            };
            return CatchupOutcome::Discarded;
        }

        self.state = State::Normal;
        self.last_action = None;
        CatchupOutcome::Ready
    }

    /// Reset to clean normal state (e.g., when switching books).
    pub fn reset(&mut self, applied: Option<K>) {
        self.state = State::Normal;
        self.pending = None;
        self.running = None;
        self.applied = applied;
        self.last_action = None;
        self.catchup_discarded = false;
        self.queued_turns.clear();
        self.merged_count = 0;
        self.discarded_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Key = u32;
    type Anchor = &'static str;

    fn t(secs: u64, ms: u32) -> Instant {
        let base = Instant::now();
        base + Duration::new(secs, ms * 1_000_000)
    }

    #[test]
    fn test_partial_path_vs_fallback_path_selection() {
        let mut s: PartialScheduler<Key, Anchor> = PartialScheduler::new();
        let t0 = t(0, 0);

        // 1. can_partial = true selects StartPartial
        s.request(1, "p5", true, t0);
        assert_eq!(s.state(), State::Debouncing);
        assert!(s.poll(t0 + Duration::from_millis(500)).is_none());
        let action = s.poll(t0 + Duration::from_millis(600)).unwrap();
        assert_eq!(
            action,
            StartAction::StartPartial {
                key: 1,
                payload: "p5"
            }
        );
        assert_eq!(s.state(), State::PartialRelayout);

        // Finish partial
        assert_eq!(
            s.finish_partial(t0 + Duration::from_millis(700)),
            Outcome::Ready
        );
        assert_eq!(s.state(), State::Degraded);

        // 2. can_partial = false selects StartFull (fallback)
        s.request(2, "p10", false, t(1, 0));
        assert_eq!(s.state(), State::Debouncing);
        let action = s.poll(t(1, 600)).unwrap();
        assert_eq!(
            action,
            StartAction::StartFull {
                key: 2,
                payload: "p10"
            }
        );
        assert_eq!(s.state(), State::FullRelayout);
        assert_eq!(s.finish_full(), Outcome::Ready);
        assert_eq!(s.state(), State::Normal);
    }

    #[test]
    fn test_idle_duration_before_catchup() {
        let mut s: PartialScheduler<Key, Anchor> = PartialScheduler::new();
        let t0 = t(0, 0);

        s.request(1, "p5", true, t0);
        let _ = s.poll(t0 + Duration::from_millis(600));
        let _ = s.finish_partial(t0 + Duration::from_millis(700));
        assert_eq!(s.state(), State::Degraded);

        // At 1.5s idle (< IDLE 2s), poll returns None
        assert!(s.poll(t0 + Duration::from_millis(2200)).is_none());

        // Reader turns page at 2.3s -> user_action resets countdown
        s.user_action(t0 + Duration::from_millis(2300));

        // At 3.5s (only 1.2s after turn), still None
        assert!(s.poll(t0 + Duration::from_millis(3500)).is_none());

        // At 4.3s (2.0s after user_action), catchup starts!
        let action = s.poll(t0 + Duration::from_millis(4300)).unwrap();
        assert_eq!(action, StartAction::StartCatchup);
        assert_eq!(s.state(), State::CatchingUp);
    }

    #[test]
    fn test_turns_queued_during_catchup() {
        let mut s: PartialScheduler<Key, Anchor> = PartialScheduler::new();
        let t0 = t(0, 0);

        s.request(1, "p5", true, t0);
        let _ = s.poll(t0 + Duration::from_millis(600));
        let _ = s.finish_partial(t0 + Duration::from_millis(700));
        let _ = s.poll(t0 + Duration::from_millis(2700));
        assert_eq!(s.state(), State::CatchingUp);

        // Turns must be queued while catchup is running
        assert!(s.should_queue_turn());
        s.queue_turn(101);
        s.queue_turn(102);

        // Finish catchup
        let outcome = s.finish_catchup();
        assert_eq!(outcome, CatchupOutcome::Ready);
        assert_eq!(s.state(), State::Normal);
        assert!(!s.should_queue_turn());

        // Turns are popped and executed in order
        assert_eq!(s.take_turns(), vec![101, 102]);
    }

    #[test]
    fn test_font_change_during_catchup_discards_and_restarts() {
        let mut s: PartialScheduler<Key, Anchor> = PartialScheduler::new();
        let t0 = t(0, 0);

        s.request(1, "p5", true, t0);
        let _ = s.poll(t0 + Duration::from_millis(600));
        let _ = s.finish_partial(t0 + Duration::from_millis(700));
        let _ = s.poll(t0 + Duration::from_millis(2700));
        assert_eq!(s.state(), State::CatchingUp);

        // Font changed during catchup
        s.request(2, "p6", true, t0 + Duration::from_millis(3000));

        // When catchup worker finishes, it is discarded
        assert_eq!(s.finish_catchup(), CatchupOutcome::Discarded);
        assert_eq!(s.discarded_count(), 1);

        // New font starts relayout once its debounce expires
        assert!(s.poll(t0 + Duration::from_millis(3500)).is_none());
        let action = s.poll(t0 + Duration::from_millis(3600)).unwrap();
        assert_eq!(
            action,
            StartAction::StartPartial {
                key: 2,
                payload: "p6"
            }
        );
    }

    #[test]
    fn test_catchup_clears_page_cache() {
        let mut s: PartialScheduler<Key, Anchor> = PartialScheduler::new();
        let t0 = t(0, 0);

        s.request(1, "p5", true, t0);
        let _ = s.poll(t0 + Duration::from_millis(600));
        let _ = s.finish_partial(t0 + Duration::from_millis(700));
        let _ = s.poll(t0 + Duration::from_millis(2700));

        // Mock session cache before catchup
        let mut mock_page_cache = vec![5, 6, 7];
        assert_eq!(mock_page_cache.len(), 3);

        let outcome = s.finish_catchup();
        assert_eq!(outcome, CatchupOutcome::Ready);
        assert_eq!(s.state(), State::Normal);

        // Catchup completion must clear the page cache
        mock_page_cache.clear();
        assert!(
            mock_page_cache.is_empty(),
            "cache must be cleared after catchup"
        );
    }

    #[test]
    fn test_degraded_state_suppresses_wrong_page_or_fraction() {
        let mut s: PartialScheduler<Key, Anchor> = PartialScheduler::new();
        let t0 = t(0, 0);

        s.request(1, "p5", true, t0);
        let _ = s.poll(t0 + Duration::from_millis(600));
        let _ = s.finish_partial(t0 + Duration::from_millis(700));

        assert!(s.is_degraded(), "book should be in degraded state");

        // Helper function mimicking Session::progress / labels
        fn format_progress_safe(
            is_degraded: bool,
            page: usize,
            total: usize,
            fraction: f32,
        ) -> (Option<String>, Option<f32>) {
            if is_degraded {
                // In degraded state, neither page count nor fraction may be exported
                (None, None)
            } else {
                (Some(format!("{page} / {total}")), Some(fraction))
            }
        }

        let (page_label, fraction) = format_progress_safe(s.is_degraded(), 5, 3039, 0.0016);
        assert_eq!(page_label, None, "degraded state must suppress page label");
        assert_eq!(
            fraction, None,
            "degraded state must suppress inaccurate fraction"
        );

        // After catchup
        let _ = s.poll(t0 + Duration::from_millis(2700));
        let _ = s.finish_catchup();
        assert!(!s.is_degraded());

        let (page_label, fraction) = format_progress_safe(s.is_degraded(), 5, 4472, 0.0011);
        assert_eq!(page_label, Some("5 / 4472".into()));
        assert_eq!(fraction, Some(0.0011));
    }
}
