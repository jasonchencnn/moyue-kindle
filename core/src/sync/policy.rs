//! Synchronization policies, reading position resolution, and TXT filtering.

use std::collections::HashSet;

use crate::sync::records::{DbBookConfig, DbBookNote};

/// Navigation resolution for an incoming remote book position (contract §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PositionResolution {
    /// Direct crengine XPointer navigation.
    NavigateXPointer(String),
    /// Only CFI location exists without an XPointer. Client must stay put (no jumping by progress fraction).
    NeedsManualPosition,
    /// Neither XPointer nor CFI location is available.
    Unresolvable,
}

/// Resolves reading position from a book config according to contract §5.2.
///
/// Priority:
/// 1. `xpointer` if present and non-empty.
/// 2. `location` (CFI) returns `NeedsManualPosition`.
/// 3. Otherwise `Unresolvable`.
///
/// Progress percentage/ratio is NEVER used for jumping, because different layout engines
/// produce incompatible page numbers (#5980).
pub fn resolve_reading_position(config: &DbBookConfig) -> PositionResolution {
    if let Some(ref xp) = config.xpointer {
        let trimmed = xp.trim();
        if !trimmed.is_empty() {
            return PositionResolution::NavigateXPointer(trimmed.to_string());
        }
    }

    if let Some(ref loc) = config.location {
        let trimmed = loc.trim();
        if !trimmed.is_empty() {
            return PositionResolution::NeedsManualPosition;
        }
    }

    PositionResolution::Unresolvable
}

/// Decide whether incoming remote progress should be accepted.
///
/// Forward-only rule: remote state is applied only if `remote_updated_at_ms > local_updated_at_ms`.
pub fn should_apply_remote_progress(local_updated_at_ms: i64, remote_updated_at_ms: i64) -> bool {
    remote_updated_at_ms > local_updated_at_ms
}

/// What a pulled book config may do to the open reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressPlan {
    /// Leave the local position alone.
    Ignore,
    /// Ask the reader whether to move, with the resolved target.
    Offer(PositionResolution),
}

/// Decide whether a pulled config may change what the reader shows.
///
/// Only the book that is open may be moved, and even then only after the
/// user answers a prompt (contract §5.2). A full or manual sync must never
/// write `rec.progress`: doing so moves the position without a question,
/// and a CFI-only remote position has no crengine equivalent to jump to.
/// TXT books keep their position local-only (contract §8).
pub fn plan_progress_update(
    local_updated_at_ms: i64,
    config: &DbBookConfig,
    book_open: bool,
    is_txt: bool,
) -> ProgressPlan {
    if !book_open || is_txt {
        return ProgressPlan::Ignore;
    }
    let remote_ms = config
        .updated_at
        .as_deref()
        .and_then(|ts| crate::sync::parse_iso8601(ts).ok())
        .unwrap_or(0);
    if !should_apply_remote_progress(local_updated_at_ms, remote_ms) {
        return ProgressPlan::Ignore;
    }
    match resolve_reading_position(config) {
        PositionResolution::Unresolvable => ProgressPlan::Ignore,
        resolution => ProgressPlan::Offer(resolution),
    }
}

/// Check if a book format is TXT (case-insensitive).
pub fn is_txt_format(format: &str) -> bool {
    format.trim().eq_ignore_ascii_case("txt")
}

/// Filter outgoing push data for TXT books (contract §8):
/// - In `configs`, remove `xpointer`, `location`, and `progress`.
/// - In `notes`, completely drop notes belonging to TXT books.
pub fn filter_push_payload(
    txt_book_hashes: &HashSet<String>,
    mut configs: Vec<DbBookConfig>,
    notes: Vec<DbBookNote>,
) -> (Vec<DbBookConfig>, Vec<DbBookNote>) {
    for config in &mut configs {
        if txt_book_hashes.contains(&config.book_hash) {
            config.xpointer = None;
            config.location = None;
            config.progress = None;
        }
    }

    let filtered_notes: Vec<DbBookNote> = notes
        .into_iter()
        .filter(|n| !txt_book_hashes.contains(&n.book_hash))
        .collect();

    (configs, filtered_notes)
}

/// Filter incoming pulled config for TXT books (contract §8):
/// - Strips `xpointer` and `location` positions.
pub fn filter_pulled_config_for_txt(mut config: DbBookConfig) -> DbBookConfig {
    config.xpointer = None;
    config.location = None;
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xpointer_takes_priority_over_cfi() {
        let config = DbBookConfig {
            book_hash: "hash01".to_string(),
            xpointer: Some("/body/DocFragment[1]/p[2].0".to_string()),
            location: Some("epubcfi(/6/2)".to_string()),
            ..Default::default()
        };

        let res = resolve_reading_position(&config);
        assert_eq!(
            res,
            PositionResolution::NavigateXPointer("/body/DocFragment[1]/p[2].0".to_string())
        );
    }

    #[test]
    fn cfi_only_returns_needs_manual_position_never_jumps() {
        let config = DbBookConfig {
            book_hash: "hash01".to_string(),
            xpointer: None,
            location: Some("epubcfi(/6/4)".to_string()),
            progress: Some("[50, 100]".to_string()),
            ..Default::default()
        };

        let res = resolve_reading_position(&config);
        assert_eq!(res, PositionResolution::NeedsManualPosition);
    }

    #[test]
    fn empty_position_returns_unresolvable() {
        let config = DbBookConfig {
            book_hash: "hash01".to_string(),
            xpointer: Some("   ".to_string()),
            location: None,
            ..Default::default()
        };

        let res = resolve_reading_position(&config);
        assert_eq!(res, PositionResolution::Unresolvable);
    }

    #[test]
    fn forward_only_progress_policy() {
        assert!(should_apply_remote_progress(1000, 2000));
        assert!(!should_apply_remote_progress(2000, 1000));
        assert!(!should_apply_remote_progress(1000, 1000));
    }

    #[test]
    fn txt_filtering_on_push() {
        let mut txt_hashes = HashSet::new();
        txt_hashes.insert("txt-hash-01".to_string());

        let configs = vec![
            DbBookConfig {
                book_hash: "txt-hash-01".to_string(),
                xpointer: Some("/txt/xp".to_string()),
                location: Some("cfi".to_string()),
                progress: Some("[10, 100]".to_string()),
                ..Default::default()
            },
            DbBookConfig {
                book_hash: "epub-hash-02".to_string(),
                xpointer: Some("/epub/xp".to_string()),
                location: None,
                progress: Some("[20, 200]".to_string()),
                ..Default::default()
            },
        ];

        let notes = vec![
            DbBookNote {
                book_hash: "txt-hash-01".to_string(),
                id: "note-txt".to_string(),
                ..Default::default()
            },
            DbBookNote {
                book_hash: "epub-hash-02".to_string(),
                id: "note-epub".to_string(),
                ..Default::default()
            },
        ];

        let (filtered_configs, filtered_notes) = filter_push_payload(&txt_hashes, configs, notes);

        assert_eq!(filtered_configs.len(), 2);
        // TXT config has positions and progress stripped
        assert_eq!(filtered_configs[0].xpointer, None);
        assert_eq!(filtered_configs[0].location, None);
        assert_eq!(filtered_configs[0].progress, None);

        // EPUB config untouched
        assert_eq!(filtered_configs[1].xpointer.as_deref(), Some("/epub/xp"));

        // TXT note is completely omitted, only EPUB note remains
        assert_eq!(filtered_notes.len(), 1);
        assert_eq!(filtered_notes[0].id, "note-epub");
    }

    #[test]
    fn txt_filtering_on_pull() {
        let config = DbBookConfig {
            book_hash: "txt-hash-01".to_string(),
            xpointer: Some("/some/xp".to_string()),
            location: Some("cfi".to_string()),
            progress: Some("[5, 50]".to_string()),
            ..Default::default()
        };

        let sanitized = filter_pulled_config_for_txt(config);
        assert_eq!(sanitized.xpointer, None);
        assert_eq!(sanitized.location, None);
        assert_eq!(sanitized.progress.as_deref(), Some("[5, 50]"));
    }

    #[test]
    fn manual_sync_never_moves_the_reading_position() {
        // Remote has a CFI and a progress pair, and is newer.
        let config = DbBookConfig {
            book_hash: "hash01".to_string(),
            xpointer: None,
            location: Some("epubcfi(/6/4)".to_string()),
            progress: Some("[50, 100]".to_string()),
            updated_at: Some(crate::sync::format_iso8601(2_000_000)),
            ..Default::default()
        };
        let plan = plan_progress_update(1_000, &config, false, false);
        assert_eq!(plan, ProgressPlan::Ignore);

        // Not even an XPointer moves a book that is not open.
        let config = DbBookConfig {
            xpointer: Some("/body/DocFragment[1]/p[1].0".to_string()),
            updated_at: Some(crate::sync::format_iso8601(2_000_000)),
            ..config
        };
        assert_eq!(
            plan_progress_update(1_000, &config, false, false),
            ProgressPlan::Ignore
        );
    }

    #[test]
    fn open_book_offers_xpointer_but_not_cfi_or_progress() {
        let config = DbBookConfig {
            book_hash: "hash01".to_string(),
            xpointer: Some("/body/DocFragment[1]/p[1].0".to_string()),
            updated_at: Some(crate::sync::format_iso8601(2_000_000)),
            ..Default::default()
        };
        assert_eq!(
            plan_progress_update(1_000, &config, true, false),
            ProgressPlan::Offer(PositionResolution::NavigateXPointer(
                "/body/DocFragment[1]/p[1].0".to_string()
            ))
        );

        let cfi_only = DbBookConfig {
            xpointer: None,
            location: Some("epubcfi(/6/4)".to_string()),
            progress: Some("[50, 100]".to_string()),
            ..config.clone()
        };
        assert_eq!(
            plan_progress_update(1_000, &cfi_only, true, false),
            ProgressPlan::Offer(PositionResolution::NeedsManualPosition)
        );
    }

    #[test]
    fn txt_book_never_offers_a_remote_position() {
        let config = DbBookConfig {
            book_hash: "txt-01".to_string(),
            xpointer: Some("/body/DocFragment[1]/p[1].0".to_string()),
            updated_at: Some(crate::sync::format_iso8601(2_000_000)),
            ..Default::default()
        };
        assert_eq!(
            plan_progress_update(0, &config, true, true),
            ProgressPlan::Ignore
        );
    }
}
