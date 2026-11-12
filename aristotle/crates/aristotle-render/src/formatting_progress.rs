// SPDX-License-Identifier: AGPL-3.0-or-later
//! State machine for formatting progress display.
//!
//! When opening a book or changing font size/margins, layout may take
//! seconds. This policy decides when the progress bar appears, when it
//! updates, and when it hides:
//!
//! - Delay: only appears if formatting takes at least 500 ms ([`DELAY`]).
//! - Step: refreshes roughly every 10% ([`STEP`]), never every 1%, to
//!   avoid excess e-ink waveform triggers.
//! - Hides when formatting completes (if it was ever shown).
//!
//! The policy is pure: the caller passes time in, so it can be tested
//! deterministically without waiting.

use std::time::{Duration, Instant};

/// Threshold before the progress bar is shown (500 ms).
pub const DELAY: Duration = Duration::from_millis(500);

/// Minimum percentage step between display refreshes (10%).
pub const STEP: u8 = 10;

/// Action to be taken by the UI renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressAction {
    /// No change to the display.
    None,
    /// First time showing the progress bar on screen with given percentage.
    Show(u8),
    /// Update the progress bar to a new percentage.
    Update(u8),
    /// Formatting finished; hide the progress bar.
    Hide,
}

/// State machine tracking formatting progress and display visibility.
#[derive(Clone, Debug)]
pub struct FormattingProgress {
    /// When formatting started.
    started_at: Option<Instant>,
    /// Whether the progress bar is currently visible on screen.
    visible: bool,
    /// The percentage at which the progress bar was last shown or updated.
    last_rendered: Option<u8>,
    /// Whether formatting has finished.
    finished: bool,
}

impl Default for FormattingProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl FormattingProgress {
    /// Create a new idle formatting progress tracker.
    pub fn new() -> Self {
        Self {
            started_at: None,
            visible: false,
            last_rendered: None,
            finished: false,
        }
    }

    /// Reset state for a new formatting operation.
    pub fn reset(&mut self) {
        self.started_at = None;
        self.visible = false;
        self.last_rendered = None;
        self.finished = false;
    }

    /// Mark formatting started at `now`.
    pub fn start(&mut self, now: Instant) {
        self.started_at = Some(now);
        self.visible = false;
        self.last_rendered = None;
        self.finished = false;
    }

    /// Whether formatting is active (started and not yet finished).
    pub fn is_active(&self) -> bool {
        self.started_at.is_some() && !self.finished
    }

    /// Whether the progress bar is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The percentage at which the progress bar was last shown or updated.
    pub fn last_rendered_percent(&self) -> Option<u8> {
        self.last_rendered
    }

    /// Update with current progress (0..=100) and current time `now`.
    pub fn update(&mut self, progress: u8, now: Instant) -> ProgressAction {
        let Some(started) = self.started_at else {
            return ProgressAction::None;
        };
        if self.finished {
            return ProgressAction::None;
        }

        let progress = progress.min(100);
        if progress >= 100 {
            return self.finish();
        }

        let elapsed = now.saturating_duration_since(started);
        if elapsed < DELAY {
            return ProgressAction::None;
        }

        if !self.visible {
            self.visible = true;
            self.last_rendered = Some(progress);
            return ProgressAction::Show(progress);
        }

        if let Some(last) = self.last_rendered {
            if progress >= last + STEP {
                self.last_rendered = Some(progress);
                return ProgressAction::Update(progress);
            }
        }

        ProgressAction::None
    }

    /// Mark formatting finished.
    pub fn finish(&mut self) -> ProgressAction {
        if self.finished {
            return ProgressAction::None;
        }
        self.finished = true;
        if self.visible {
            self.visible = false;
            ProgressAction::Hide
        } else {
            ProgressAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_formatting_under_500ms_never_shows() {
        let mut fp = FormattingProgress::new();
        let t0 = Instant::now();

        fp.start(t0);
        assert!(fp.is_active());
        assert!(!fp.is_visible());

        // Updates arriving under 500ms must not trigger Show or Update
        assert_eq!(
            fp.update(10, t0 + Duration::from_millis(50)),
            ProgressAction::None
        );
        assert_eq!(
            fp.update(30, t0 + Duration::from_millis(150)),
            ProgressAction::None
        );
        assert_eq!(
            fp.update(60, t0 + Duration::from_millis(300)),
            ProgressAction::None
        );
        assert_eq!(
            fp.update(90, t0 + Duration::from_millis(450)),
            ProgressAction::None
        );

        // Formatting finishes at 480ms (< 500ms): never shown, so returns None
        assert_eq!(fp.finish(), ProgressAction::None);
        assert!(!fp.is_active());
        assert!(!fp.is_visible());
    }

    #[test]
    fn cache_hit_immediate_finish_never_shows() {
        let mut fp = FormattingProgress::new();
        let t0 = Instant::now();

        fp.start(t0);
        // Cache hit reaches 100 at 25ms
        assert_eq!(
            fp.update(100, t0 + Duration::from_millis(25)),
            ProgressAction::None
        );
        assert!(!fp.is_active());
        assert!(!fp.is_visible());
    }

    #[test]
    fn slow_formatting_shows_after_500ms_and_refreshes_per_10_percent() {
        let mut fp = FormattingProgress::new();
        let t0 = Instant::now();

        fp.start(t0);

        // Under 500ms: no display
        assert_eq!(
            fp.update(15, t0 + Duration::from_millis(200)),
            ProgressAction::None
        );
        assert_eq!(
            fp.update(25, t0 + Duration::from_millis(499)),
            ProgressAction::None
        );

        // Exactly / slightly past 500ms: shows with current progress
        assert_eq!(
            fp.update(30, t0 + Duration::from_millis(500)),
            ProgressAction::Show(30)
        );
        assert!(fp.is_visible());
        assert_eq!(fp.last_rendered_percent(), Some(30));

        // Progress change < 10% does NOT refresh:
        // 31%: +1% -> None
        assert_eq!(
            fp.update(31, t0 + Duration::from_millis(600)),
            ProgressAction::None
        );
        // 35%: +5% -> None
        assert_eq!(
            fp.update(35, t0 + Duration::from_millis(700)),
            ProgressAction::None
        );
        // 39%: +9% -> None
        assert_eq!(
            fp.update(39, t0 + Duration::from_millis(800)),
            ProgressAction::None
        );

        // Progress change >= 10% (30 -> 40): updates
        assert_eq!(
            fp.update(40, t0 + Duration::from_millis(900)),
            ProgressAction::Update(40)
        );
        assert_eq!(fp.last_rendered_percent(), Some(40));

        // 45%: +5% -> None
        assert_eq!(
            fp.update(45, t0 + Duration::from_millis(1000)),
            ProgressAction::None
        );

        // Next 10% jump: 40 -> 52 (+12% >= 10%): updates
        assert_eq!(
            fp.update(52, t0 + Duration::from_millis(1200)),
            ProgressAction::Update(52)
        );
        assert_eq!(fp.last_rendered_percent(), Some(52));

        // Next 10% jump: 52 -> 62 (+10%): updates
        assert_eq!(
            fp.update(62, t0 + Duration::from_millis(1400)),
            ProgressAction::Update(62)
        );

        // Next jump: 62 -> 80 (+18%): updates
        assert_eq!(
            fp.update(80, t0 + Duration::from_millis(1600)),
            ProgressAction::Update(80)
        );

        // Finish while visible: returns Hide
        assert_eq!(fp.finish(), ProgressAction::Hide);
        assert!(!fp.is_active());
        assert!(!fp.is_visible());

        // Subsequent finish calls are idempotent
        assert_eq!(fp.finish(), ProgressAction::None);
    }

    #[test]
    fn reaching_100_percent_during_update_hides_if_visible() {
        let mut fp = FormattingProgress::new();
        let t0 = Instant::now();

        fp.start(t0);
        assert_eq!(
            fp.update(40, t0 + Duration::from_millis(600)),
            ProgressAction::Show(40)
        );

        // Progress reaching 100 via update
        assert_eq!(
            fp.update(100, t0 + Duration::from_millis(1500)),
            ProgressAction::Hide
        );
        assert!(!fp.is_active());
        assert!(!fp.is_visible());
    }

    #[test]
    fn reset_clears_state() {
        let mut fp = FormattingProgress::new();
        let t0 = Instant::now();

        fp.start(t0);
        assert_eq!(
            fp.update(40, t0 + Duration::from_millis(600)),
            ProgressAction::Show(40)
        );
        assert!(fp.is_visible());

        fp.reset();
        assert!(!fp.is_active());
        assert!(!fp.is_visible());
        assert_eq!(fp.last_rendered_percent(), None);
    }
}
