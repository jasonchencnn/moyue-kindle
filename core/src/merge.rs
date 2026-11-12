//! Merge rules matching Readest file-sync: notes are an element-set keyed by id
//! with per-note `updatedAt` and `deletedAt` tombstones; scalars are LWW on config `updatedAt`.

use crate::model::{BookConfig, BookNote};
use std::collections::HashMap;

pub fn merge_notes(local: Vec<BookNote>, remote: Vec<BookNote>) -> Vec<BookNote> {
    let mut by_id: HashMap<String, BookNote> =
        local.into_iter().map(|n| (n.id.clone(), n)).collect();
    for r in remote {
        match by_id.get(&r.id) {
            Some(l) if l.updated_at >= r.updated_at => {}
            _ => {
                by_id.insert(r.id.clone(), r);
            }
        }
    }
    let mut v: Vec<_> = by_id.into_values().collect();
    v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    v
}

pub fn merge_config(local: BookConfig, remote: BookConfig) -> BookConfig {
    let notes = merge_notes(local.booknotes.clone(), remote.booknotes.clone());
    let mut winner = if remote.updated_at > local.updated_at {
        remote
    } else {
        local
    };
    winner.booknotes = notes;
    winner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NoteType;

    fn note(id: &str, updated: i64, deleted: Option<i64>) -> BookNote {
        BookNote {
            id: id.into(),
            kind: NoteType::Annotation,
            cfi: String::new(),
            xpointer0: None,
            xpointer1: None,
            text: None,
            color: None,
            style: None,
            note: String::new(),
            created_at: 1,
            updated_at: updated,
            deleted_at: deleted,
        }
    }

    #[test]
    fn newer_note_wins_and_tombstone_propagates() {
        let m = merge_notes(
            vec![note("a", 1, None)],
            vec![note("a", 2, Some(2)), note("b", 1, None)],
        );
        assert_eq!(m.len(), 2);
        assert_eq!(m.iter().find(|n| n.id == "a").unwrap().deleted_at, Some(2));
    }
}
