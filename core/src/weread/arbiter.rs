//! Position arbitration between the local book and the WeRead cloud (§14.4 step 3).
//!
//! Three coordinates decide who moved: the local position, the cloud position,
//! and the pair both had at the last alignment. That is a three-way merge, so
//! the answer does not depend on clocks the two sides never share:
//!
//! - local == remote → [`Verdict::Aligned`]: nothing to write.
//! - only local moved → [`Verdict::LocalNewer`]: the close/sleep write is allowed.
//! - only remote moved → [`Verdict::RemoteNewer`]: adopt the cloud, never write.
//! - both moved → [`Verdict::Conflict`]: write fence, never write.
//!
//! [`WriteFence`] turns a verdict into at most one write: once armed it allows
//! one attempt, and an unconfirmed attempt is never blindly resent (the
//! contract forbids resending a write that may have been committed). A later
//! verdict may re-arm the fence only after the cloud was re-read and the
//! unconfirmed position was verified as not landed.

/// One WeRead position, without the `precision` labels of `position.rs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PositionStamp {
    /// `chapterUid`.
    pub chapter_uid: i64,
    /// `chapterIdx`.
    pub chapter_idx: i64,
    /// Character offset inside the chapter.
    pub chapter_offset: i64,
}

impl PositionStamp {
    /// A coordinate triple.
    pub fn new(chapter_uid: i64, chapter_idx: i64, chapter_offset: i64) -> Self {
        Self {
            chapter_uid,
            chapter_idx,
            chapter_offset,
        }
    }
}

/// The last pair both sides agreed on: the merge base.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncBaseline {
    /// The local position at the alignment.
    pub local: PositionStamp,
    /// The cloud position at the alignment.
    pub remote: PositionStamp,
}

impl SyncBaseline {
    /// An alignment record.
    pub fn new(local: PositionStamp, remote: PositionStamp) -> Self {
        Self { local, remote }
    }
}

/// The four states of §14.4 step 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The local book moved and the cloud did not.
    LocalNewer,
    /// The cloud moved and the local book did not.
    RemoteNewer,
    /// Both sides name the same position.
    Aligned,
    /// Both sides moved, or a first sync cannot order them.
    Conflict,
}

impl Verdict {
    /// Whether §14.4 allows the close/sleep write in this state.
    pub fn may_write(self) -> bool {
        matches!(self, Self::LocalNewer)
    }
}

/// Compare the local and cloud positions with the last alignment.
///
/// A first sync without a baseline that finds two different positions is a
/// [`Verdict::Conflict`]: neither side can be called newer, and guessing would
/// overwrite a position the reader may not have seen.
pub fn arbitrate(
    local: PositionStamp,
    remote: PositionStamp,
    baseline: Option<SyncBaseline>,
) -> Verdict {
    if local == remote {
        return Verdict::Aligned;
    }
    let Some(baseline) = baseline else {
        return Verdict::Conflict;
    };
    let local_moved = local != baseline.local;
    let remote_moved = remote != baseline.remote;
    match (local_moved, remote_moved) {
        (true, false) => Verdict::LocalNewer,
        (false, true) => Verdict::RemoteNewer,
        (true, true) => Verdict::Conflict,
        (false, false) => Verdict::Conflict,
    }
}

/// What a fence can do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceState {
    /// No write is allowed (remote won, aligned, conflict, or no verdict yet).
    Closed,
    /// A local-newer verdict armed one write.
    Armed,
    /// The one write is in flight; a second attempt is forbidden.
    Sent,
    /// The server returned `succ:1`.
    Confirmed,
    /// The server did not confirm; the write must never be resent.
    Unconfirmed,
    /// The Web cookies expired; the reader must scan again.
    Expired,
}

/// The write fence of §14.4 step 3.
///
/// It permits at most one write per verdict and never blindly resends an
/// unconfirmed write; re-arming requires a fresh verdict after re-reading
/// the cloud (the contract's "no automatic retry" rule expressed as a state
/// machine).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteFence {
    state: FenceState,
    verdict: Verdict,
    dirty: bool,
}

impl WriteFence {
    /// A fence for `verdict`; only [`Verdict::LocalNewer`] with a dirty book
    /// starts armed.
    pub fn new(verdict: Verdict, dirty: bool) -> Self {
        let state = if verdict == Verdict::LocalNewer && dirty {
            FenceState::Armed
        } else {
            FenceState::Closed
        };
        Self {
            state,
            verdict,
            dirty,
        }
    }

    /// Re-decide with a fresh verdict (a new close/sleep event).
    pub fn reset(&mut self, verdict: Verdict, dirty: bool) -> &mut Self {
        *self = Self::new(verdict, dirty);
        self
    }

    /// The current state.
    pub fn state(&self) -> FenceState {
        self.state
    }

    /// The verdict the fence was built from.
    pub fn verdict(&self) -> Verdict {
        self.verdict
    }

    /// Whether the caller may send the write now.
    pub fn allows_write(&self) -> bool {
        self.state == FenceState::Armed
    }

    /// Consume the permission; `false` when no write is allowed.
    pub fn begin_write(&mut self) -> bool {
        if self.state != FenceState::Armed {
            return false;
        }
        self.state = FenceState::Sent;
        true
    }

    /// The server answered `succ:1`: the dirty flag is cleared for good.
    pub fn confirmed(&mut self) {
        if self.state == FenceState::Sent {
            self.state = FenceState::Confirmed;
            self.dirty = false;
        }
    }

    /// No receipt, `succ:0`, a timeout or a dropped connection: never resend.
    pub fn unconfirmed(&mut self) {
        if self.state == FenceState::Sent {
            self.state = FenceState::Unconfirmed;
        }
    }

    /// The Web cookies are dead: no write until a new scan.
    pub fn expired(&mut self) {
        self.state = FenceState::Expired;
    }

    /// Whether the local book still differs from the cloud.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(uid: i64, idx: i64, offset: i64) -> PositionStamp {
        PositionStamp::new(uid, idx, offset)
    }

    #[test]
    fn a_position_that_both_sides_name_is_aligned() {
        let local = stamp(7, 2, 100);
        let baseline = SyncBaseline::new(stamp(6, 1, 0), stamp(6, 1, 0));
        assert_eq!(arbitrate(local, local, Some(baseline)), Verdict::Aligned);
        assert_eq!(arbitrate(local, local, None), Verdict::Aligned);
        assert!(!Verdict::Aligned.may_write());
    }

    #[test]
    fn only_a_local_move_is_local_newer() {
        // AC-04: LOCAL_NEWER is the one state that may write.
        let baseline = SyncBaseline::new(stamp(7, 2, 100), stamp(7, 2, 100));
        let verdict = arbitrate(stamp(7, 2, 240), stamp(7, 2, 100), Some(baseline));
        assert_eq!(verdict, Verdict::LocalNewer);
        assert!(verdict.may_write());
    }

    #[test]
    fn only_a_remote_move_is_remote_newer() {
        let baseline = SyncBaseline::new(stamp(7, 2, 100), stamp(7, 2, 100));
        let verdict = arbitrate(stamp(7, 2, 100), stamp(7, 2, 240), Some(baseline));
        assert_eq!(verdict, Verdict::RemoteNewer);
        assert!(!verdict.may_write());
    }

    #[test]
    fn both_sides_moving_is_a_conflict() {
        let baseline = SyncBaseline::new(stamp(7, 2, 100), stamp(7, 2, 100));
        let verdict = arbitrate(stamp(7, 2, 240), stamp(8, 3, 10), Some(baseline));
        assert_eq!(verdict, Verdict::Conflict);
        assert!(!verdict.may_write());
    }

    #[test]
    fn a_first_sync_without_a_baseline_never_guesses() {
        assert_eq!(
            arbitrate(stamp(7, 2, 100), stamp(7, 2, 240), None),
            Verdict::Conflict
        );
    }

    #[test]
    fn the_fence_allows_exactly_one_write() {
        let mut fence = WriteFence::new(Verdict::LocalNewer, true);
        assert!(fence.allows_write());
        assert!(fence.begin_write());
        assert!(!fence.begin_write());
        assert!(!fence.allows_write());
        fence.unconfirmed();
        assert_eq!(fence.state(), FenceState::Unconfirmed);
        assert!(!fence.begin_write());
        // A new close event re-arms only with a fresh local-newer verdict.
        fence.reset(Verdict::LocalNewer, true);
        assert!(fence.allows_write());
        assert!(fence.begin_write());
        fence.confirmed();
        assert_eq!(fence.state(), FenceState::Confirmed);
        assert!(!fence.is_dirty());
    }

    #[test]
    fn remote_aligned_and_conflict_never_arm_the_fence() {
        for verdict in [Verdict::RemoteNewer, Verdict::Aligned, Verdict::Conflict] {
            let fence = WriteFence::new(verdict, true);
            assert!(!fence.allows_write(), "{verdict:?}");
            assert_eq!(fence.state(), FenceState::Closed);
        }
        // A clean local-newer verdict has nothing to write either.
        assert!(!WriteFence::new(Verdict::LocalNewer, false).allows_write());
    }

    #[test]
    fn expired_credentials_close_the_fence_forever() {
        let mut fence = WriteFence::new(Verdict::LocalNewer, true);
        fence.expired();
        assert_eq!(fence.state(), FenceState::Expired);
        assert!(!fence.allows_write());
        assert!(!fence.begin_write());
    }
}
