// SPDX-License-Identifier: AGPL-3.0-or-later
//! State machine for handing off in-progress prewarm to book open.
//!
//! When the reader opens a book, prewarm may be running in the background at
//! `SCHED_IDLE` priority holding `OPEN_GATE` and the engine lock. Three cases arise:
//!
//! 1. **Same book (`Action::Adopt`)**: The book being opened is the one prewarm
//!    is preparing. The open takes over the task, boosts its thread priority to
//!    normal, and reuses the prepared document without a duplicate layout.
//! 2. **Different book (`Action::CancelAndBoost`)**: The reader opened another
//!    book. Prewarm is cancelled and its thread priority is boosted to normal
//!    so it quickly finishes its current step and releases `OPEN_GATE`, preventing
//!    priority inversion.
//! 3. **Prewarm finished / idle (`Action::Normal`)**: Prewarm is not running.
//!    The open proceeds normally.

/// State of the prewarm handoff coordinator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State<K, T> {
    /// No prewarm is in progress.
    Idle,
    /// Prewarm is currently running for a book identified by `key`.
    Prewarming { key: K, task: T },
    /// Prewarm finished on its own (snapshot saved or completed).
    Finished,
}

/// Action to take when opening a book.
#[derive(Debug, PartialEq, Eq)]
pub enum Action<T> {
    /// Opening the same book: adopt the in-progress prewarm task.
    Adopt(T),
    /// Opening a different book: cancel the prewarm task and boost its priority.
    CancelAndBoost(T),
    /// Prewarm is finished or idle: proceed with a normal open.
    Normal,
}

/// Coordinates prewarm and open transitions.
pub struct Handoff<K, T> {
    state: State<K, T>,
    adoptions: u32,
    cancellations: u32,
}

impl<K: PartialEq, T> Default for Handoff<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: PartialEq, T> Handoff<K, T> {
    /// Creates a new idle handoff coordinator.
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            adoptions: 0,
            cancellations: 0,
        }
    }

    /// The current state.
    pub fn state(&self) -> &State<K, T> {
        &self.state
    }

    /// Whether prewarm is currently in progress.
    pub fn is_prewarming(&self) -> bool {
        matches!(self.state, State::Prewarming { .. })
    }

    /// The key of the book currently being prewarmed, if any.
    pub fn prewarming_key(&self) -> Option<&K> {
        match &self.state {
            State::Prewarming { key, .. } => Some(key),
            _ => None,
        }
    }

    /// Reference to the running task, if prewarming.
    pub fn prewarming_task(&self) -> Option<&T> {
        match &self.state {
            State::Prewarming { task, .. } => Some(task),
            _ => None,
        }
    }

    /// Mutable reference to the running task, if prewarming.
    pub fn prewarming_task_mut(&mut self) -> Option<&mut T> {
        match &mut self.state {
            State::Prewarming { task, .. } => Some(task),
            _ => None,
        }
    }

    /// Start a prewarm task for `key`.
    pub fn start(&mut self, key: K, task: T) {
        self.state = State::Prewarming { key, task };
    }

    /// Mark the prewarm task as finished on its own.
    pub fn finish(&mut self) {
        self.state = State::Finished;
    }

    /// Reset back to idle.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    /// The reader opens a book with `key`.
    pub fn open(&mut self, key: &K) -> Action<T> {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Prewarming {
                key: running_key,
                task,
            } => {
                if running_key == *key {
                    self.adoptions += 1;
                    Action::Adopt(task)
                } else {
                    self.cancellations += 1;
                    Action::CancelAndBoost(task)
                }
            }
            State::Finished => Action::Normal,
            State::Idle => Action::Normal,
        }
    }

    /// Explicitly cancel the in-progress prewarm (e.g. going to sleep or leaving library).
    pub fn cancel(&mut self) -> Option<T> {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Prewarming { task, .. } => {
                self.cancellations += 1;
                Some(task)
            }
            _ => None,
        }
    }

    /// Number of times prewarm was adopted by open.
    pub fn adoptions(&self) -> u32 {
        self.adoptions
    }

    /// Number of times prewarm was cancelled with priority boost.
    pub fn cancellations(&self) -> u32 {
        self.cancellations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_the_same_book_adopts_prewarm_task() {
        let mut handoff: Handoff<String, i32> = Handoff::new();
        handoff.start("book-1".into(), 42);
        assert!(handoff.is_prewarming());
        assert_eq!(handoff.prewarming_key(), Some(&"book-1".to_string()));

        let action = handoff.open(&"book-1".to_string());
        assert_eq!(action, Action::Adopt(42));
        assert_eq!(handoff.adoptions(), 1);
        assert_eq!(handoff.cancellations(), 0);
        assert!(!handoff.is_prewarming());
        assert_eq!(handoff.state(), &State::Idle);
    }

    #[test]
    fn opening_another_book_cancels_and_boosts_prewarm_task() {
        let mut handoff: Handoff<String, i32> = Handoff::new();
        handoff.start("book-1".into(), 42);

        let action = handoff.open(&"book-2".to_string());
        assert_eq!(action, Action::CancelAndBoost(42));
        assert_eq!(handoff.adoptions(), 0);
        assert_eq!(handoff.cancellations(), 1);
        assert!(!handoff.is_prewarming());
        assert_eq!(handoff.state(), &State::Idle);
    }

    #[test]
    fn opening_when_prewarm_is_finished_proceeds_normally() {
        let mut handoff: Handoff<String, i32> = Handoff::new();
        handoff.start("book-1".into(), 42);
        handoff.finish();
        assert_eq!(handoff.state(), &State::Finished);
        assert!(!handoff.is_prewarming());

        let action = handoff.open(&"book-1".to_string());
        assert_eq!(action, Action::Normal);
        assert_eq!(handoff.adoptions(), 0);
        assert_eq!(handoff.cancellations(), 0);
    }

    #[test]
    fn opening_when_idle_proceeds_normally() {
        let mut handoff: Handoff<String, i32> = Handoff::new();
        assert_eq!(handoff.state(), &State::Idle);

        let action = handoff.open(&"book-1".to_string());
        assert_eq!(action, Action::Normal);
        assert_eq!(handoff.adoptions(), 0);
        assert_eq!(handoff.cancellations(), 0);
    }

    #[test]
    fn composite_key_with_style_distinguishes_typography_changes() {
        #[derive(Clone, PartialEq, Eq, Debug)]
        struct Key {
            id: String,
            font_size: u32,
        }

        let mut handoff: Handoff<Key, &'static str> = Handoff::new();
        let key_10pt = Key {
            id: "book-1".into(),
            font_size: 10,
        };
        let key_12pt = Key {
            id: "book-1".into(),
            font_size: 12,
        };

        handoff.start(key_10pt.clone(), "task-10pt");

        // Opening same book with different font size cancels & boosts old task
        let action = handoff.open(&key_12pt);
        assert_eq!(action, Action::CancelAndBoost("task-10pt"));
        assert_eq!(handoff.cancellations(), 1);
    }

    #[test]
    fn cancellation_without_open() {
        let mut handoff: Handoff<String, i32> = Handoff::new();
        handoff.start("book-1".into(), 99);

        let task = handoff.cancel();
        assert_eq!(task, Some(99));
        assert_eq!(handoff.cancellations(), 1);
        assert_eq!(handoff.state(), &State::Idle);

        // Subsequent open is normal
        assert_eq!(handoff.open(&"book-1".to_string()), Action::Normal);
    }

    #[test]
    fn multiple_cycles_of_prewarm_and_handoff() {
        let mut handoff: Handoff<String, i32> = Handoff::new();

        // Cycle 1: Prewarm finished
        handoff.start("book-1".into(), 1);
        handoff.finish();
        assert_eq!(handoff.open(&"book-1".to_string()), Action::Normal);

        // Cycle 2: Prewarm adopted
        handoff.start("book-2".into(), 2);
        assert_eq!(handoff.open(&"book-2".to_string()), Action::Adopt(2));

        // Cycle 3: Prewarm cancelled for another book
        handoff.start("book-3".into(), 3);
        assert_eq!(
            handoff.open(&"book-4".to_string()),
            Action::CancelAndBoost(3)
        );

        assert_eq!(handoff.adoptions(), 1);
        assert_eq!(handoff.cancellations(), 1);
    }
}
