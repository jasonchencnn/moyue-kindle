//! Last-writer-wins merge rules for the Jianguoyun sync records (contract §15.4).
//!
//! Progress is a single LWW scalar ordered by `(updatedAtMs, fraction)`; notes are an element
//! set keyed by `id` where the effective timestamp is `max(updatedAtMs, deletedAtMs)` and a
//! tombstone wins an exact tie, so a deletion propagates instead of being resurrected.

use std::collections::HashMap;

use crate::jianguoyun::model::{JianguoyunBookSync, JianguoyunNote, JianguoyunProgress};

/// What a device should do with the reading position after comparing both copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressDecision {
    /// Both copies carry the same position; nothing has to be transferred.
    InSync,
    /// The remote position is newer; apply it locally.
    PullRemote,
    /// The local position is newer; upload it.
    PushLocal,
}

/// Decide which side owns the reading position.
///
/// A newer `updated_at_ms` wins; on an exact millisecond tie the larger `fraction` wins, and a
/// full tie resolves to the remote copy, which is [`ProgressDecision::InSync`] because neither
/// side has to move.
pub fn decide_progress(
    local: Option<&JianguoyunProgress>,
    remote: Option<&JianguoyunProgress>,
) -> ProgressDecision {
    match (local, remote) {
        (None, None) => ProgressDecision::InSync,
        (None, Some(_)) => ProgressDecision::PullRemote,
        (Some(_), None) => ProgressDecision::PushLocal,
        (Some(local), Some(remote)) => {
            if remote.is_newer_than(local) {
                ProgressDecision::PullRemote
            } else if local.is_newer_than(remote) {
                ProgressDecision::PushLocal
            } else {
                ProgressDecision::InSync
            }
        }
    }
}

/// Pick the winning reading position, keeping the remote copy on an exact tie.
pub fn merge_progress(
    local: Option<&JianguoyunProgress>,
    remote: Option<&JianguoyunProgress>,
) -> Option<JianguoyunProgress> {
    match (local, remote) {
        (None, None) => None,
        (Some(local), None) => Some(local.clone()),
        (None, Some(remote)) => Some(remote.clone()),
        (Some(local), Some(remote)) => {
            if local.is_newer_than(remote) {
                Some(local.clone())
            } else {
                Some(remote.clone())
            }
        }
    }
}

/// Merge note sets by `id`, keeping tombstones so deletions reach every device.
///
/// The effective timestamp is `max(updated_at_ms, deleted_at_ms.unwrap_or(0))`. A newer
/// timestamp wins; on an exact tie a tombstone beats a live note, and two notes with the same
/// deletion state prefer the remote copy (matching progress tie-breaking so two devices converge
/// without endless re-pushes). The result is sorted by `id`.
pub fn merge_notes(local: &[JianguoyunNote], remote: &[JianguoyunNote]) -> Vec<JianguoyunNote> {
    let mut merged: HashMap<String, JianguoyunNote> =
        HashMap::with_capacity(local.len() + remote.len());
    for note in local {
        merged.insert(note.id.clone(), note.clone());
    }
    for note in remote {
        let take_remote = match merged.get(&note.id) {
            None => true,
            Some(existing) => {
                let existing_stamp = existing.effective_timestamp_ms();
                let remote_stamp = note.effective_timestamp_ms();
                remote_stamp > existing_stamp
                    || (remote_stamp == existing_stamp
                        && (note.is_deleted() || !existing.is_deleted()))
            }
        };
        if take_remote {
            merged.insert(note.id.clone(), note.clone());
        }
    }
    let mut notes: Vec<JianguoyunNote> = merged.into_values().collect();
    notes.sort_by(|left, right| left.id.cmp(&right.id));
    notes
}

/// Merge two copies of one book record.
///
/// The reading position decides which side owns `title`/`author`/`remotePath` (remote on a tie);
/// a field that the winning side left empty falls back to the other side. `book_hash` and
/// `version` never regress, and notes are merged independently of the position.
pub fn merge_book_sync(
    local: &JianguoyunBookSync,
    remote: &JianguoyunBookSync,
) -> JianguoyunBookSync {
    let decision = decide_progress(local.progress.as_ref(), remote.progress.as_ref());
    let (preferred, fallback) = if decision == ProgressDecision::PushLocal {
        (local, remote)
    } else {
        (remote, local)
    };
    JianguoyunBookSync {
        version: local.version.max(remote.version),
        book_hash: if preferred.book_hash.trim().is_empty() {
            fallback.book_hash.clone()
        } else {
            preferred.book_hash.clone()
        },
        title: prefer_text(&preferred.title, &fallback.title),
        author: prefer_text(&preferred.author, &fallback.author),
        remote_path: prefer_text(&preferred.remote_path, &fallback.remote_path),
        progress: merge_progress(local.progress.as_ref(), remote.progress.as_ref()),
        notes: merge_notes(&local.notes, &remote.notes),
    }
}

fn prefer_text(preferred: &str, fallback: &str) -> String {
    if preferred.trim().is_empty() {
        fallback.to_string()
    } else {
        preferred.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jianguoyun::model::{JianguoyunBookSync, NoteKind};

    fn progress(fraction: f64, updated_at_ms: i64, device: &str) -> JianguoyunProgress {
        JianguoyunProgress {
            xpointer: Some("/body/DocFragment[1]/body/p[1]/text().0".to_string()),
            cfi: None,
            fraction,
            page: Some(10),
            total_pages: Some(100),
            chapter_title: Some("第一章".to_string()),
            updated_at_ms,
            device_id: device.to_string(),
        }
    }

    fn note(id: &str, updated_at_ms: i64, deleted_at_ms: Option<i64>) -> JianguoyunNote {
        JianguoyunNote {
            id: id.to_string(),
            kind: NoteKind::Highlight,
            xpointer0: Some(format!("{id}-start")),
            xpointer1: Some(format!("{id}-end")),
            text: Some(format!("text-{id}")),
            note: None,
            color: Some("yellow".to_string()),
            created_at_ms: 1,
            updated_at_ms,
            deleted_at_ms,
        }
    }

    fn record(
        hash: &str,
        progress: Option<JianguoyunProgress>,
        notes: Vec<JianguoyunNote>,
    ) -> JianguoyunBookSync {
        JianguoyunBookSync {
            version: 1,
            book_hash: hash.to_string(),
            title: "三体".to_string(),
            author: "刘慈欣".to_string(),
            remote_path: "/Books/三体.epub".to_string(),
            progress,
            notes,
        }
    }

    #[test]
    fn decision_handles_missing_sides() {
        let local = progress(0.4, 1_000, "A");
        assert_eq!(decide_progress(None, None), ProgressDecision::InSync);
        assert_eq!(
            decide_progress(None, Some(&local)),
            ProgressDecision::PullRemote
        );
        assert_eq!(
            decide_progress(Some(&local), None),
            ProgressDecision::PushLocal
        );
    }

    #[test]
    fn decision_prefers_newer_timestamp_then_fraction() {
        let local = progress(0.4, 2_000, "A");
        let remote = progress(0.9, 1_000, "B");
        assert_eq!(
            decide_progress(Some(&local), Some(&remote)),
            ProgressDecision::PushLocal
        );
        assert_eq!(
            decide_progress(Some(&remote), Some(&local)),
            ProgressDecision::PullRemote
        );

        let tied = progress(0.6, 2_000, "B");
        assert_eq!(
            decide_progress(Some(&local), Some(&tied)),
            ProgressDecision::PullRemote
        );
        assert_eq!(
            decide_progress(Some(&tied), Some(&local)),
            ProgressDecision::PushLocal
        );

        let same = local.clone();
        assert_eq!(
            decide_progress(Some(&local), Some(&same)),
            ProgressDecision::InSync
        );
    }

    #[test]
    fn merge_progress_keeps_remote_on_tie() {
        let local = progress(0.5, 1_000, "A");
        let remote = progress(0.5, 1_000, "B");
        let merged = merge_progress(Some(&local), Some(&remote)).unwrap();
        assert_eq!(merged.device_id, "B");

        let newer = progress(0.8, 2_000, "A");
        assert_eq!(
            merge_progress(Some(&newer), Some(&remote))
                .unwrap()
                .device_id,
            "A"
        );
        assert_eq!(merge_progress(Some(&local), None).unwrap().device_id, "A");
        assert!(merge_progress(None, None).is_none());
    }

    #[test]
    fn notes_merge_by_id_and_sort_deterministically() {
        let local = vec![note("b", 10, None), note("a", 10, None)];
        let remote = vec![note("c", 5, None)];
        let merged = merge_notes(&local, &remote);
        let ids: Vec<&str> = merged.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn newer_note_wins_in_both_directions() {
        let local = vec![note("a", 20, None)];
        let remote = vec![note("a", 10, None)];
        assert_eq!(merge_notes(&local, &remote)[0].updated_at_ms, 20);
        assert_eq!(merge_notes(&remote, &local)[0].updated_at_ms, 20);
    }

    #[test]
    fn tombstone_wins_an_exact_tie() {
        let live = vec![note("a", 30, None)];
        let tombstone = vec![note("a", 30, Some(30))];
        assert!(merge_notes(&live, &tombstone)[0].is_deleted());
        assert!(merge_notes(&tombstone, &live)[0].is_deleted());
    }

    #[test]
    fn live_note_exact_tie_prefers_remote_and_converges() {
        let mut local = note("a", 30, None);
        local.text = Some("本地文本".to_string());
        let mut remote = note("a", 30, None);
        remote.text = Some("远端文本".to_string());
        let merged = merge_notes(&[local], std::slice::from_ref(&remote));
        assert_eq!(merged, vec![remote]);
    }

    #[test]
    fn newer_live_note_resurrects_an_older_tombstone() {
        let tombstone = vec![note("a", 10, Some(10))];
        let live = vec![note("a", 20, None)];
        assert!(!merge_notes(&tombstone, &live)[0].is_deleted());
        assert!(!merge_notes(&live, &tombstone)[0].is_deleted());
    }

    #[test]
    fn effective_timestamp_accounts_for_deleted_at() {
        let old_tombstone = vec![note("a", 10, Some(40))];
        let recent_live = vec![note("a", 20, None)];
        assert!(merge_notes(&recent_live, &old_tombstone)[0].is_deleted());
        assert!(merge_notes(&old_tombstone, &recent_live)[0].is_deleted());
    }

    #[test]
    fn delete_propagates_across_both_devices() {
        let device_a = vec![note("a", 10, None), note("b", 10, None)];
        let device_b = vec![note("b", 10, None), note("a", 15, Some(15))];
        let merged_a = merge_notes(&device_a, &device_b);
        assert!(merged_a
            .iter()
            .any(|entry| entry.id == "a" && entry.is_deleted()));
        assert!(merged_a
            .iter()
            .any(|entry| entry.id == "b" && !entry.is_deleted()));

        let merged_b = merge_notes(&device_b, &merged_a);
        assert!(merged_b
            .iter()
            .any(|entry| entry.id == "a" && entry.is_deleted()));
    }

    #[test]
    fn book_merge_pulls_newer_progress_and_keeps_local_only_notes() {
        let local = record(
            "0123456789abcdef0123456789abcdef",
            Some(progress(0.2, 1_000, "KINDLE")),
            vec![note("local-only", 5, None)],
        );
        let mut remote = record(
            "0123456789abcdef0123456789abcdef",
            Some(progress(0.7, 2_000, "PHONE")),
            vec![note("remote-only", 6, None)],
        );
        remote.title = String::new();
        remote.author = "刘慈欣".to_string();

        let merged = merge_book_sync(&local, &remote);
        assert_eq!(merged.progress.as_ref().unwrap().device_id, "PHONE");
        assert_eq!(merged.title, "三体");
        assert_eq!(merged.notes.len(), 2);
        assert_eq!(merged.version, 1);
    }

    #[test]
    fn book_merge_of_identical_records_is_idempotent() {
        let local = record(
            "0123456789abcdef0123456789abcdef",
            Some(progress(0.5, 1_000, "A")),
            vec![note("a", 1, None)],
        );
        let remote = local.clone();
        assert_eq!(merge_book_sync(&local, &remote), remote);
    }

    #[test]
    fn book_merge_keeps_local_metadata_when_local_progress_wins() {
        let local = record(
            "0123456789abcdef0123456789abcdef",
            Some(progress(0.9, 9_000, "A")),
            Vec::new(),
        );
        let mut remote = record(
            "0123456789abcdef0123456789abcdef",
            Some(progress(0.1, 1_000, "B")),
            Vec::new(),
        );
        remote.title = "旧标题".to_string();
        let merged = merge_book_sync(&local, &remote);
        assert_eq!(merged.title, "三体");
        assert_eq!(merged.progress.as_ref().unwrap().device_id, "A");
    }

    #[test]
    fn book_merge_never_downgrades_version_or_hash() {
        let mut local = record("", None, Vec::new());
        local.version = 2;
        let mut remote = record("0123456789abcdef0123456789abcdef", None, Vec::new());
        remote.version = 1;
        let merged = merge_book_sync(&local, &remote);
        assert_eq!(merged.version, 2);
        assert_eq!(merged.book_hash, "0123456789abcdef0123456789abcdef");
    }
}
