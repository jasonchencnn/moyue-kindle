//! Client-side merge logic adhering strictly to sync-contract v2 and fixture specifications.

use std::collections::HashMap;

use crate::sync::records::{DbBook, DbBookConfig, DbBookNote};
use crate::sync::time::parse_iso8601;

/// Identifies the winner of a whole-row comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowWinner {
    /// Local (client) row won.
    Local,
    /// Remote (server) row won.
    Remote,
}

/// Helper to parse ISO-8601 timestamp string into ms epoch, defaulting to 0.
fn ts_ms(ts: Option<&str>) -> i64 {
    ts.and_then(|s| parse_iso8601(s).ok()).unwrap_or(0)
}

/// Decide whole-row winner between local and remote records using last-writer-wins with tombstone precedence.
///
/// Follows upstream `sync.ts:729-738`:
/// `client_is_newer = client_deleted > server_deleted || client_updated > server_updated`.
pub fn determine_row_winner(
    local_updated: Option<&str>,
    local_deleted: Option<&str>,
    remote_updated: Option<&str>,
    remote_deleted: Option<&str>,
) -> RowWinner {
    let local_u = ts_ms(local_updated);
    let local_d = ts_ms(local_deleted);
    let remote_u = ts_ms(remote_updated);
    let remote_d = ts_ms(remote_deleted);

    let client_is_newer = local_d > remote_d || local_u > remote_u;
    if client_is_newer {
        RowWinner::Local
    } else {
        RowWinner::Remote
    }
}

/// Merge two `DbBook` records according to whole-row and field-level LWW rules.
pub fn merge_book_rows(local: &DbBook, remote: &DbBook) -> (RowWinner, DbBook) {
    let row_win = determine_row_winner(
        local.updated_at.as_deref(),
        local.deleted_at.as_deref(),
        remote.updated_at.as_deref(),
        remote.deleted_at.as_deref(),
    );

    // Start with the base from the row winner
    let mut result = match row_win {
        RowWinner::Local => local.clone(),
        RowWinner::Remote => remote.clone(),
    };

    // 1. Reading status group: client wins on tie (>=)
    let local_status_ms = ts_ms(local.reading_status_updated_at.as_deref());
    let remote_status_ms = ts_ms(remote.reading_status_updated_at.as_deref());
    let status_winner_is_local = local_status_ms >= remote_status_ms;
    let status_source = if status_winner_is_local {
        local
    } else {
        remote
    };
    result.reading_status = status_source.reading_status.clone();
    result.reading_status_updated_at = status_source.reading_status_updated_at.clone();

    // 2. Cover group: client wins on tie (>=)
    let local_cover_ms = ts_ms(local.cover_updated_at.as_deref());
    let remote_cover_ms = ts_ms(remote.cover_updated_at.as_deref());
    let cover_winner_is_local = local_cover_ms >= remote_cover_ms;
    let cover_source = if cover_winner_is_local { local } else { remote };
    result.cover_hash = cover_source.cover_hash.clone();
    result.cover_updated_at = cover_source.cover_updated_at.clone();

    // 3. Metadata group: newer wins; ties follow row winner; empty blob guard applies
    let local_meta_ms = ts_ms(local.metadata_updated_at.as_deref());
    let remote_meta_ms = ts_ms(remote.metadata_updated_at.as_deref());
    let meta_winner_is_local = if local_meta_ms > remote_meta_ms {
        true
    } else if remote_meta_ms > local_meta_ms {
        false
    } else {
        row_win == RowWinner::Local
    };
    let (meta_winner, meta_loser) = if meta_winner_is_local {
        (local, remote)
    } else {
        (remote, local)
    };
    result.title = meta_winner.title.clone();
    result.author = meta_winner.author.clone();
    result.tags = meta_winner.tags.clone();
    result.metadata_updated_at = meta_winner.metadata_updated_at.clone();
    // Guard: null winner metadata does not overwrite existing loser metadata
    result.metadata = meta_winner
        .metadata
        .clone()
        .or_else(|| meta_loser.metadata.clone());

    // 4. Group group: newer wins; ties favor the side having a group; otherwise follows row winner
    let local_grp_ms = ts_ms(local.group_updated_at.as_deref());
    let remote_grp_ms = ts_ms(remote.group_updated_at.as_deref());
    let local_has_grp = local.group_id.is_some() || local.group_name.is_some();
    let remote_has_grp = remote.group_id.is_some() || remote.group_name.is_some();

    let grp_winner_is_local = if local_grp_ms > remote_grp_ms {
        true
    } else if remote_grp_ms > local_grp_ms {
        false
    } else if local_has_grp != remote_has_grp {
        local_has_grp
    } else {
        row_win == RowWinner::Local
    };
    let grp_source = if grp_winner_is_local { local } else { remote };
    result.group_id = grp_source.group_id.clone();
    result.group_name = grp_source.group_name.clone();
    result.group_updated_at = grp_source.group_updated_at.clone();

    (row_win, result)
}

/// Merge two `DbBookConfig` records using row-level LWW with tombstone resolution.
pub fn merge_book_config(local: &DbBookConfig, remote: &DbBookConfig) -> (RowWinner, DbBookConfig) {
    let row_win = determine_row_winner(
        local.updated_at.as_deref(),
        local.deleted_at.as_deref(),
        remote.updated_at.as_deref(),
        remote.deleted_at.as_deref(),
    );

    let winner = match row_win {
        RowWinner::Local => local.clone(),
        RowWinner::Remote => remote.clone(),
    };
    (row_win, winner)
}

/// Merge a single `DbBookNote` pair with the same ID.
fn merge_single_note(local: &DbBookNote, remote: &DbBookNote) -> DbBookNote {
    let local_u = ts_ms(local.updated_at.as_deref());
    let local_d = ts_ms(local.deleted_at.as_deref());
    let remote_u = ts_ms(remote.updated_at.as_deref());
    let remote_d = ts_ms(remote.deleted_at.as_deref());

    let local_eff = local_d.max(local_u);
    let remote_eff = remote_d.max(remote_u);

    if local_eff > remote_eff {
        local.clone()
    } else if remote_eff > local_eff {
        remote.clone()
    } else {
        // Equal timestamps: tombstone wins
        let local_has_tomb = local.deleted_at.is_some() && local_d > 0;
        let remote_has_tomb = remote.deleted_at.is_some() && remote_d > 0;

        if local_has_tomb && !remote_has_tomb {
            local.clone()
        } else {
            remote.clone()
        }
    }
}

/// Merge sets of notes from local and remote by unique ID.
pub fn merge_notes(local: &[DbBookNote], remote: &[DbBookNote]) -> Vec<DbBookNote> {
    let mut by_id: HashMap<String, DbBookNote> = HashMap::new();

    for note in local {
        by_id.insert(note.id.clone(), note.clone());
    }

    for note in remote {
        match by_id.get(&note.id) {
            Some(existing) => {
                let merged = merge_single_note(existing, note);
                by_id.insert(note.id.clone(), merged);
            }
            None => {
                by_id.insert(note.id.clone(), note.clone());
            }
        }
    }

    let mut result: Vec<DbBookNote> = by_id.into_values().collect();
    // Sort by id for stable deterministic output
    result.sort_by(|a, b| {
        let a_created = ts_ms(a.created_at.as_deref());
        let b_created = ts_ms(b.created_at.as_deref());
        a_created.cmp(&b_created).then_with(|| a.id.cmp(&b.id))
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn test_fixture_lww_basic() {
        let json_str = include_str!("../../../tests/fixtures/sync/lww_basic.json");
        let root: Value = serde_json::from_str(json_str).expect("parse lww_basic.json");

        for case in root["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let table = case["table"].as_str().unwrap();
            let expected_winner_str = case["expected"]["winner"].as_str().unwrap();
            let expected_winner = match expected_winner_str {
                "local" => RowWinner::Local,
                "remote" => RowWinner::Remote,
                other => panic!("unknown winner {other}"),
            };

            if table == "books" {
                let local: DbBook = serde_json::from_value(case["input"]["local"].clone()).unwrap();
                let remote: DbBook =
                    serde_json::from_value(case["input"]["remote"].clone()).unwrap();
                let expected_result: DbBook =
                    serde_json::from_value(case["expected"]["result"].clone()).unwrap();

                let (winner, merged) = merge_book_rows(&local, &remote);
                assert_eq!(winner, expected_winner, "case {name} winner mismatch");
                assert_eq!(merged, expected_result, "case {name} full row mismatch");
            } else if table == "book_configs" {
                let local: DbBookConfig =
                    serde_json::from_value(case["input"]["local"].clone()).unwrap();
                let remote: DbBookConfig =
                    serde_json::from_value(case["input"]["remote"].clone()).unwrap();
                let expected_result: DbBookConfig =
                    serde_json::from_value(case["expected"]["result"].clone()).unwrap();

                let (winner, merged) = merge_book_config(&local, &remote);
                assert_eq!(winner, expected_winner, "case {name} winner mismatch");
                assert_eq!(merged, expected_result, "case {name} full row mismatch");
            } else {
                panic!("unexpected table {table} in case {name}");
            }
        }
    }

    #[test]
    fn test_fixture_lww_ties() {
        let json_str = include_str!("../../../tests/fixtures/sync/lww_ties.json");
        let root: Value = serde_json::from_str(json_str).expect("parse lww_ties.json");

        for case in root["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let local: DbBook = serde_json::from_value(case["input"]["local"].clone()).unwrap();
            let remote: DbBook = serde_json::from_value(case["input"]["remote"].clone()).unwrap();
            let expected_result: DbBook =
                serde_json::from_value(case["expected"]["result"].clone()).unwrap();

            let (winner, merged) = merge_book_rows(&local, &remote);

            if let Some(w) = case["expected"].get("winner").and_then(|v| v.as_str()) {
                let exp_w = match w {
                    "local" => RowWinner::Local,
                    "remote" => RowWinner::Remote,
                    other => panic!("unknown winner {other}"),
                };
                assert_eq!(winner, exp_w, "case {name} winner mismatch");
            }
            if let Some(rw) = case["expected"].get("row_winner").and_then(|v| v.as_str()) {
                let exp_rw = match rw {
                    "local" => RowWinner::Local,
                    "remote" => RowWinner::Remote,
                    other => panic!("unknown row_winner {other}"),
                };
                assert_eq!(winner, exp_rw, "case {name} row_winner mismatch");
            }
            if let Some(sw) = case["expected"]
                .get("reading_status_winner")
                .and_then(|v| v.as_str())
            {
                let expected_status = if sw == "client" || sw == "local" {
                    &local.reading_status
                } else {
                    &remote.reading_status
                };
                assert_eq!(
                    &merged.reading_status, expected_status,
                    "case {name} reading_status_winner mismatch"
                );
            }
            if let Some(mw) = case["expected"]
                .get("metadata_winner")
                .and_then(|v| v.as_str())
            {
                let expected_meta = if mw == "client" || mw == "local" {
                    (&local.title, &local.author)
                } else {
                    (&remote.title, &remote.author)
                };
                assert_eq!(
                    (&merged.title, &merged.author),
                    expected_meta,
                    "case {name} metadata_winner mismatch"
                );
            }
            if let Some(gw) = case["expected"]
                .get("group_winner")
                .and_then(|v| v.as_str())
            {
                let expected_grp = if gw == "client" || gw == "local" {
                    (&local.group_id, &local.group_name)
                } else {
                    (&remote.group_id, &remote.group_name)
                };
                assert_eq!(
                    (&merged.group_id, &merged.group_name),
                    expected_grp,
                    "case {name} group_winner mismatch"
                );
            }

            assert_eq!(merged, expected_result, "case {name} full row mismatch");
        }
    }

    #[test]
    fn test_fixture_tombstones() {
        let json_str = include_str!("../../../tests/fixtures/sync/tombstones.json");
        let root: Value = serde_json::from_str(json_str).expect("parse tombstones.json");

        for case in root["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let table = case["table"].as_str().unwrap();

            if table == "books" {
                let local: DbBook = serde_json::from_value(case["input"]["local"].clone()).unwrap();
                let remote: DbBook =
                    serde_json::from_value(case["input"]["remote"].clone()).unwrap();
                let expected_winner_str = case["expected"]["winner"].as_str().unwrap();
                let expected_result: DbBook =
                    serde_json::from_value(case["expected"]["result"].clone()).unwrap();

                let (winner, merged) = merge_book_rows(&local, &remote);
                let expected_winner = match expected_winner_str {
                    "local" => RowWinner::Local,
                    "remote" => RowWinner::Remote,
                    other => panic!("unknown winner {other}"),
                };

                assert_eq!(winner, expected_winner, "case {name} winner mismatch");
                assert_eq!(merged, expected_result, "case {name} full row mismatch");
            } else if table == "book_notes" {
                let records: Vec<DbBookNote> =
                    serde_json::from_value(case["input"]["records"].clone()).unwrap();
                assert_eq!(records.len(), 2);
                let merged = merge_notes(&[records[0].clone()], &[records[1].clone()]);
                assert_eq!(merged.len(), 1, "case {name} should dedupe to 1 note");
                let expected_notes: Vec<DbBookNote> =
                    serde_json::from_value(case["expected"]["result"].clone()).unwrap();
                assert_eq!(merged, expected_notes, "case {name} full note mismatch");
            }
        }
    }

    #[test]
    fn test_fixture_field_level_lww() {
        let json_str = include_str!("../../../tests/fixtures/sync/field_level_lww.json");
        let root: Value = serde_json::from_str(json_str).expect("parse field_level_lww.json");

        for case in root["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let local: DbBook = serde_json::from_value(case["input"]["local"].clone()).unwrap();
            let remote: DbBook = serde_json::from_value(case["input"]["remote"].clone()).unwrap();
            let expected_result: DbBook =
                serde_json::from_value(case["expected"]["result"].clone()).unwrap();

            let (winner, merged) = merge_book_rows(&local, &remote);

            if let Some(rw) = case["expected"].get("row_winner").and_then(|v| v.as_str()) {
                let exp_rw = match rw {
                    "local" => RowWinner::Local,
                    "remote" => RowWinner::Remote,
                    other => panic!("unknown row_winner {other}"),
                };
                assert_eq!(winner, exp_rw, "case {name} row_winner mismatch");
            }
            if let Some(mw) = case["expected"]
                .get("metadata_winner")
                .and_then(|v| v.as_str())
            {
                let expected_title = if mw == "client" || mw == "local" {
                    &local.title
                } else {
                    &remote.title
                };
                assert_eq!(
                    &merged.title, expected_title,
                    "case {name} metadata_winner mismatch"
                );
            }
            if let Some(gw) = case["expected"]
                .get("group_winner")
                .and_then(|v| v.as_str())
            {
                let expected_grp = if gw == "client" || gw == "local" {
                    (&local.group_id, &local.group_name)
                } else {
                    (&remote.group_id, &remote.group_name)
                };
                assert_eq!(
                    (&merged.group_id, &merged.group_name),
                    expected_grp,
                    "case {name} group_winner mismatch"
                );
            }

            assert_eq!(merged, expected_result, "case {name} full row mismatch");
        }
    }

    #[test]
    fn test_fixture_notes_merge() {
        let json_str = include_str!("../../../tests/fixtures/sync/notes_merge.json");
        let root: Value = serde_json::from_str(json_str).expect("parse notes_merge.json");

        for case in root["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let local: Vec<DbBookNote> =
                serde_json::from_value(case["input"]["local"].clone()).unwrap();
            let remote: Vec<DbBookNote> =
                serde_json::from_value(case["input"]["remote"].clone()).unwrap();

            let merged = merge_notes(&local, &remote);

            if let Some(total) = case["expected"]["total"].as_u64() {
                assert_eq!(merged.len(), total as usize, "case {name} total mismatch");
            }
            if let Some(expected_ids) = case["expected"]["ids"].as_array() {
                let actual_ids: Vec<&str> = merged.iter().map(|n| n.id.as_str()).collect();
                for id_val in expected_ids {
                    let id = id_val.as_str().unwrap();
                    assert!(
                        actual_ids.contains(&id),
                        "case {name} missing id {id} in {actual_ids:?}"
                    );
                }
            }
            if let Some(expected_res) = case["expected"]["result"].as_array() {
                let expected_notes: Vec<DbBookNote> =
                    serde_json::from_value(serde_json::Value::Array(expected_res.clone())).unwrap();
                assert_eq!(
                    merged, expected_notes,
                    "case {name} full note list mismatch"
                );
            }
        }
    }

    #[test]
    fn book_row_without_updated_at_cannot_beat_the_server() {
        // What the Kindle sends: no updated_at, no deleted_at.
        let local = DbBook {
            book_hash: "book-1".to_string(),
            title: "Old local title".to_string(),
            ..Default::default()
        };
        // The phone renamed and then deleted the book.
        let remote = DbBook {
            book_hash: "book-1".to_string(),
            title: "Renamed on phone".to_string(),
            updated_at: Some("2026-11-05T10:00:00.000Z".to_string()),
            deleted_at: Some("2026-11-06T10:00:00.000Z".to_string()),
            ..Default::default()
        };

        let (winner, merged) = merge_book_rows(&local, &remote);
        assert_eq!(winner, RowWinner::Remote);
        assert_eq!(merged.title, "Renamed on phone");
        assert_eq!(
            merged.deleted_at.as_deref(),
            Some("2026-11-06T10:00:00.000Z"),
            "a row without timestamps must not resurrect a deleted book"
        );
    }
}
