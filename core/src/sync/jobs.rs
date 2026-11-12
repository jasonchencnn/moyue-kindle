//! Manual per-book synchronization jobs.
//!
//! A reader can ask for the open book's cloud state to be pulled or pushed at
//! any time. Two things must hold for a device whose network is unreliable and
//! whose reader can switch books mid-request:
//!
//! - At most one pull and one push per book run at a time, so a double tap does
//!   not send the same request twice (each carries a token refresh and a whole
//!   book's notes).
//! - A worker that finishes after the reader moved on is ignored: its result
//!   belongs to another book, and merging or reporting it there would be wrong.

/// What a worker was asked to do for one book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// Fetch the book's cloud config and notes.
    Pull,
    /// Send the book's progress, notes and tombstones.
    Push,
}

/// A started job: what it does, for which book, and the token that identifies
/// this particular run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookJob {
    pub kind: JobKind,
    pub book: String,
    pub token: u64,
}

/// The manual jobs in flight, one slot per kind.
#[derive(Clone, Debug, Default)]
pub struct BookJobs {
    next_token: u64,
    pull: Option<BookJob>,
    push: Option<BookJob>,
}

impl BookJobs {
    /// An empty set of jobs.
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&mut self, kind: JobKind) -> &mut Option<BookJob> {
        match kind {
            JobKind::Pull => &mut self.pull,
            JobKind::Push => &mut self.push,
        }
    }

    /// The job of `kind` now running for `book`, if any.
    pub fn running(&self, kind: JobKind, book: &str) -> Option<&BookJob> {
        let slot = match kind {
            JobKind::Pull => &self.pull,
            JobKind::Push => &self.push,
        };
        slot.as_ref().filter(|j| j.book == book)
    }

    /// Start a job, unless one of the same kind is already running for `book`.
    ///
    /// A job for a *different* book still occupies the slot: the reader moved
    /// on, and its completion will be dropped by [`BookJobs::finish`]. Starting
    /// the new job replaces it.
    pub fn begin(&mut self, kind: JobKind, book: &str) -> Option<BookJob> {
        if self.running(kind, book).is_some() {
            return None;
        }
        self.next_token = self.next_token.wrapping_add(1);
        let job = BookJob {
            kind,
            book: book.to_string(),
            token: self.next_token,
        };
        *self.slot(kind) = Some(job.clone());
        Some(job)
    }

    /// Clear `job` if it is still the current one.
    ///
    /// Returns false for a completion that belongs to a job the reader has
    /// already replaced (a book switch, or a second request): the caller must
    /// ignore its result.
    pub fn finish(&mut self, job: &BookJob) -> bool {
        let slot = self.slot(job.kind);
        match slot {
            Some(current) if current.token == job.token => {
                *slot = None;
                true
            }
            _ => false,
        }
    }

    /// Drop the jobs of a book the reader left behind.
    ///
    /// Their tokens stay used: a completion that arrives afterwards is stale
    /// whether or not the slot was taken by another book.
    pub fn forget_book(&mut self, book: &str) {
        for slot in [&mut self.pull, &mut self.push] {
            if slot.as_ref().is_some_and(|j| j.book == book) {
                *slot = None;
            }
        }
    }

    /// Whether any manual job is in flight.
    pub fn busy(&self) -> bool {
        self.pull.is_some() || self.push.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_tap_of_the_same_button_is_refused() {
        let mut jobs = BookJobs::new();
        let first = jobs.begin(JobKind::Pull, "book-a").expect("first pull");
        assert!(jobs.begin(JobKind::Pull, "book-a").is_none());
        // The other kind is independent: a pull and a push may overlap.
        assert!(jobs.begin(JobKind::Push, "book-a").is_some());
        assert!(jobs.finish(&first));
        assert!(jobs.begin(JobKind::Pull, "book-a").is_some());
    }

    #[test]
    fn a_completion_after_a_book_switch_is_stale() {
        let mut jobs = BookJobs::new();
        let old = jobs.begin(JobKind::Push, "book-a").expect("push a");
        // The reader opens another book and pushes it.
        let new = jobs.begin(JobKind::Push, "book-b").expect("push b");
        assert_ne!(old.token, new.token);
        assert!(!jobs.finish(&old), "the old book's result must be dropped");
        assert!(jobs.finish(&new));
    }

    #[test]
    fn a_completion_after_leaving_the_book_is_stale() {
        let mut jobs = BookJobs::new();
        let job = jobs.begin(JobKind::Pull, "book-a").expect("pull a");
        jobs.forget_book("book-a");
        assert!(!jobs.busy());
        assert!(!jobs.finish(&job));
        // A later pull for the same book gets a fresh token and lands.
        let again = jobs.begin(JobKind::Pull, "book-a").expect("pull again");
        assert_ne!(again.token, job.token);
        assert!(!jobs.finish(&job));
        assert!(jobs.finish(&again));
    }

    #[test]
    fn finishing_twice_uses_up_the_job() {
        let mut jobs = BookJobs::new();
        let job = jobs.begin(JobKind::Pull, "book-a").expect("pull");
        assert!(jobs.finish(&job));
        assert!(!jobs.finish(&job));
        assert!(!jobs.busy());
    }
}
