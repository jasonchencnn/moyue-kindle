// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure state machine for USB mass storage drive mode transitions.

use std::time::Duration;

/// The current USB mass storage connection state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UsbMode {
    #[default]
    Disconnected,
    DriveMode,
}

/// An incoming USB or userstore event from the system daemon (`volumd` / `powerd`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbEvent {
    /// Early warning that userstore is about to be unmounted (`userstoreIsLikelyToUnMount`).
    DriveModeRequested,
    /// USB drive mode is active and userstore is unmounted (`driveModeStateChanged 1` or `userstoreUnAvailable`).
    DriveModeStarted,
    /// USB drive mode has ended and userstore is remounted (`driveModeStateChanged 0` or `userstoreAvailable`).
    DriveModeEnded,
}

/// The action the application must perform in response to a USB state transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbAction {
    /// No state change or redundant event; no action required.
    None,
    /// Save reading progress, speed, and stats immediately, and stop all writes to `/mnt/us`.
    SaveAndFreeze,
    /// Unload blanket drawing modules, reset touch gestures, full-screen redraw, and resume reading.
    RestoreAndResume,
}

/// Computes the next USB mode and required application action given the current mode and an event.
///
/// This is a deterministic pure function designed for thorough unit testing.
pub fn handle_usb_transition(current: UsbMode, event: UsbEvent) -> (UsbMode, UsbAction) {
    match (current, event) {
        (UsbMode::Disconnected, UsbEvent::DriveModeRequested | UsbEvent::DriveModeStarted) => {
            (UsbMode::DriveMode, UsbAction::SaveAndFreeze)
        }
        (UsbMode::DriveMode, UsbEvent::DriveModeEnded) => {
            (UsbMode::Disconnected, UsbAction::RestoreAndResume)
        }
        (UsbMode::DriveMode, UsbEvent::DriveModeRequested | UsbEvent::DriveModeStarted) => {
            // Already in drive mode: duplicate or sequential notification (e.g. Requested followed by Started)
            (UsbMode::DriveMode, UsbAction::None)
        }
        (UsbMode::Disconnected, UsbEvent::DriveModeEnded) => {
            // Already disconnected: redundant end/available notification
            (UsbMode::Disconnected, UsbAction::None)
        }
    }
}

/// How often volumd is polled while the userstore is in drive mode.
pub const DRIVE_MODE_INTERVAL: Duration = Duration::from_secs(1);

/// How often volumd is polled in normal use: often enough to notice a
/// missed powerd event, seldom enough to cost nothing.
pub const IDLE_INTERVAL: Duration = Duration::from_secs(30);

/// What one poll of volumd reported; `None` where the query failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// `driveModeState`: `0` inactive, `1` active; `None` on a failed query.
    pub drive_mode: Option<i32>,
    /// `userstoreIsAvailable`; `None` on a failed query.
    pub userstore_available: Option<bool>,
}

/// How often to poll volumd in `mode`.
pub fn poll_interval(mode: UsbMode) -> Duration {
    match mode {
        UsbMode::DriveMode => DRIVE_MODE_INTERVAL,
        UsbMode::Disconnected => IDLE_INTERVAL,
    }
}

/// The event that reconciles `current` with one poll of volumd, if any.
///
/// A failed query changes nothing: either property alone is evidence
/// enough to enter drive mode, while leaving it needs both to say so.
pub fn reconcile(current: UsbMode, status: Status) -> Option<UsbEvent> {
    match current {
        UsbMode::DriveMode => match (status.drive_mode, status.userstore_available) {
            (Some(0), Some(true)) => Some(UsbEvent::DriveModeEnded),
            _ => None,
        },
        UsbMode::Disconnected => match (status.drive_mode, status.userstore_available) {
            (Some(1), _) => Some(UsbEvent::DriveModeStarted),
            (_, Some(false)) => Some(UsbEvent::DriveModeStarted),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_mode_is_disconnected() {
        assert_eq!(UsbMode::default(), UsbMode::Disconnected);
    }

    #[test]
    fn requested_transitions_from_disconnected_to_drive_mode() {
        let (next, action) =
            handle_usb_transition(UsbMode::Disconnected, UsbEvent::DriveModeRequested);
        assert_eq!(next, UsbMode::DriveMode);
        assert_eq!(action, UsbAction::SaveAndFreeze);
    }

    #[test]
    fn started_transitions_from_disconnected_to_drive_mode() {
        let (next, action) =
            handle_usb_transition(UsbMode::Disconnected, UsbEvent::DriveModeStarted);
        assert_eq!(next, UsbMode::DriveMode);
        assert_eq!(action, UsbAction::SaveAndFreeze);
    }

    #[test]
    fn started_after_requested_is_idempotent() {
        let (mode1, action1) =
            handle_usb_transition(UsbMode::Disconnected, UsbEvent::DriveModeRequested);
        assert_eq!(mode1, UsbMode::DriveMode);
        assert_eq!(action1, UsbAction::SaveAndFreeze);

        let (mode2, action2) = handle_usb_transition(mode1, UsbEvent::DriveModeStarted);
        assert_eq!(mode2, UsbMode::DriveMode);
        assert_eq!(action2, UsbAction::None);
    }

    #[test]
    fn ended_transitions_from_drive_mode_to_disconnected() {
        let (next, action) = handle_usb_transition(UsbMode::DriveMode, UsbEvent::DriveModeEnded);
        assert_eq!(next, UsbMode::Disconnected);
        assert_eq!(action, UsbAction::RestoreAndResume);
    }

    #[test]
    fn duplicate_ended_in_disconnected_is_noop() {
        let (next, action) = handle_usb_transition(UsbMode::Disconnected, UsbEvent::DriveModeEnded);
        assert_eq!(next, UsbMode::Disconnected);
        assert_eq!(action, UsbAction::None);
    }

    #[test]
    fn five_plug_unplug_cycles_preserve_invariants() {
        let mut mode = UsbMode::Disconnected;

        for i in 1..=5 {
            // Step 1: Early warning event
            let (m1, a1) = handle_usb_transition(mode, UsbEvent::DriveModeRequested);
            assert_eq!(m1, UsbMode::DriveMode, "cycle {i} requested mode");
            assert_eq!(
                a1,
                UsbAction::SaveAndFreeze,
                "cycle {i} requested must save"
            );

            // Step 2: Confirmation that drive mode started
            let (m2, a2) = handle_usb_transition(m1, UsbEvent::DriveModeStarted);
            assert_eq!(m2, UsbMode::DriveMode, "cycle {i} started mode");
            assert_eq!(a2, UsbAction::None, "cycle {i} redundant start is noop");

            // Step 3: Drive mode ended (cable unplugged / drive ejected)
            let (m3, a3) = handle_usb_transition(m2, UsbEvent::DriveModeEnded);
            assert_eq!(m3, UsbMode::Disconnected, "cycle {i} ended mode");
            assert_eq!(
                a3,
                UsbAction::RestoreAndResume,
                "cycle {i} ended must restore"
            );

            // Step 4: Redundant userstoreAvailable event
            let (m4, a4) = handle_usb_transition(m3, UsbEvent::DriveModeEnded);
            assert_eq!(m4, UsbMode::Disconnected, "cycle {i} duplicate end mode");
            assert_eq!(a4, UsbAction::None, "cycle {i} duplicate end is noop");

            mode = m4;
        }

        assert_eq!(mode, UsbMode::Disconnected);
    }

    fn status(drive_mode: Option<i32>, userstore_available: Option<bool>) -> Status {
        Status {
            drive_mode,
            userstore_available,
        }
    }

    #[test]
    fn a_known_state_reconciles_in_both_directions() {
        assert_eq!(
            reconcile(UsbMode::Disconnected, status(Some(1), Some(true))),
            Some(UsbEvent::DriveModeStarted)
        );
        assert_eq!(
            reconcile(UsbMode::Disconnected, status(Some(0), Some(false))),
            Some(UsbEvent::DriveModeStarted)
        );
        assert_eq!(
            reconcile(UsbMode::DriveMode, status(Some(0), Some(true))),
            Some(UsbEvent::DriveModeEnded)
        );
    }

    #[test]
    fn a_state_that_agrees_or_a_failed_query_changes_nothing() {
        assert_eq!(
            reconcile(UsbMode::Disconnected, status(Some(0), Some(true))),
            None
        );
        assert_eq!(reconcile(UsbMode::DriveMode, status(Some(1), None)), None);
        assert_eq!(reconcile(UsbMode::DriveMode, status(None, None)), None);
        assert_eq!(reconcile(UsbMode::Disconnected, status(None, None)), None);
        // One property saying "drive mode" and the other failing is not
        // enough to leave it.
        assert_eq!(reconcile(UsbMode::DriveMode, status(Some(0), None)), None);
    }

    #[test]
    fn drive_mode_polls_once_a_second_and_normal_use_every_thirty() {
        assert_eq!(poll_interval(UsbMode::DriveMode), Duration::from_secs(1));
        assert_eq!(poll_interval(UsbMode::Disconnected), IDLE_INTERVAL);
        assert!(IDLE_INTERVAL >= Duration::from_secs(30));
    }
}
