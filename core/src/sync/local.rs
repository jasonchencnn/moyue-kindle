//! Local note state and how pulled Readest notes fold into it.
//!
//! The device keeps a stable id per annotation. A pull merges by that id, so
//! the same highlight does not multiply between the device and the cloud.

use crate::model::NoteType;
use crate::sync::records::DbBookNote;

/// Source tag of notes that came from the Readest cloud.
pub const READEST_SOURCE: &str = "readest";

/// A note as stored on the device.
///
/// Besides the fields Readest knows, it keeps the local-only ones (`page`,
/// `location`, `chapter`) so a pull cannot lose them.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalNote {
    pub id: String,
    pub kind: NoteType,
    pub position: Option<(String, String)>,
    pub text: Option<String>,
    pub note: Option<String>,
    pub color: Option<String>,
    pub style: Option<String>,
    pub page: Option<String>,
    pub location: Option<(u32, u32)>,
    pub chapter: Option<String>,
    /// `"readest"` for cloud notes, the importer's name otherwise.
    pub source: String,
    /// Creation time in milliseconds since the epoch (0 when unknown).
    pub created_at: i64,
    /// Last modification in milliseconds; falls back to `created_at`.
    pub updated_at: i64,
}

impl LocalNote {
    /// The effective modification time: `updated_at` or `created_at`.
    pub fn modified_at(&self) -> i64 {
        self.updated_at.max(self.created_at)
    }

    /// Whether a pull may replace this note's content.
    pub fn is_cloud_owned(&self) -> bool {
        self.source == READEST_SOURCE
    }

    /// Convert into the wire record.
    pub fn to_db_book_note(&self, book_hash: &str) -> DbBookNote {
        let note = crate::model::Annotation {
            id: Some(self.id.clone()),
            kind: self.kind.clone(),
            position: self.position.clone(),
            color: self.color.clone(),
            style: self.style.clone(),
            text: self.text.clone(),
            note: self.note.clone(),
            created_at: (self.created_at > 0).then_some(self.created_at),
            updated_at: Some(self.modified_at()),
            deleted_at: None,
        };
        note.to_db_book_note(book_hash, &self.id)
    }

    /// Convert an active wire note into a local one; `None` for a tombstone.
    pub fn from_db_book_note(note: &DbBookNote) -> Option<Self> {
        if note.deleted_at.is_some() {
            return None;
        }
        let model = crate::model::Annotation::from_db_book_note(note);
        Some(Self {
            id: model.id.unwrap_or_else(|| note.id.clone()),
            kind: model.kind,
            position: model.position,
            text: model.text,
            note: model.note,
            color: model.color,
            style: model.style,
            page: None,
            location: None,
            chapter: None,
            source: READEST_SOURCE.to_string(),
            created_at: model.created_at.unwrap_or(0),
            updated_at: model.updated_at.unwrap_or(0),
        })
    }
}

/// A removed note, kept until its tombstone reaches the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTombstone {
    pub id: String,
    pub deleted_at: i64,
}

/// What a pull did to the local note list.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteMerge {
    pub notes: Vec<LocalNote>,
    pub tombstones: Vec<LocalTombstone>,
}

/// A tombstone as a wire note (type `annotation`, both times `deleted_at`).
pub fn tombstone_to_db_book_note(book_hash: &str, tombstone: &LocalTombstone) -> DbBookNote {
    let at = crate::sync::format_iso8601(tombstone.deleted_at);
    DbBookNote {
        book_hash: book_hash.to_string(),
        id: tombstone.id.clone(),
        kind: "annotation".to_string(),
        updated_at: Some(at.clone()),
        deleted_at: Some(at),
        ..Default::default()
    }
}

/// Fold `remote` into the local list, keyed by note id.
///
/// Only the notes that belong to `book_hash` are folded in: a pull answers
/// with the rows the server chose to send, and a note of another book must
/// not land in this one.
///
/// A note new to the device is added with [`READEST_SOURCE`]; a cloud-owned
/// note is replaced by the merged wire record. A tombstone from either side
/// removes the note and stays in the result until the caller prunes it after
/// a successful push. The result has one entry per id.
pub fn merge_pulled_notes(
    book_hash: &str,
    local: &[LocalNote],
    tombstones: &[LocalTombstone],
    remote: &[DbBookNote],
) -> NoteMerge {
    use crate::sync::merge::merge_notes;
    use std::collections::HashMap;

    let mut local_wire: Vec<DbBookNote> =
        local.iter().map(|n| n.to_db_book_note(book_hash)).collect();
    local_wire.extend(
        tombstones
            .iter()
            .map(|t| tombstone_to_db_book_note(book_hash, t)),
    );
    // The notes of another book are not ours: they belong to the record of
    // that book, and merging them here would write a highlight of another
    // book into this one.
    let remote: Vec<DbBookNote> = remote
        .iter()
        .filter(|n| n.book_hash == book_hash)
        .cloned()
        .collect();
    let merged = merge_notes(&local_wire, &remote);

    let mut by_id: HashMap<&str, &LocalNote> = local.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut notes = Vec::with_capacity(merged.len());
    let mut gone = Vec::new();

    for wire in merged {
        let deleted_ms = wire
            .deleted_at
            .as_deref()
            .and_then(|ts| crate::sync::parse_iso8601(ts).ok());
        if let Some(deleted_at) = deleted_ms {
            by_id.remove(wire.id.as_str());
            gone.push(LocalTombstone {
                id: wire.id,
                deleted_at,
            });
            continue;
        }
        match by_id.remove(wire.id.as_str()) {
            Some(existing) if !existing.is_cloud_owned() => notes.push(existing.clone()),
            Some(existing) => {
                if let Some(mut cloud) = LocalNote::from_db_book_note(&wire) {
                    // Keep the local-only fields a cloud note may have gained.
                    cloud.page = existing.page.clone();
                    cloud.location = existing.location;
                    cloud.chapter = existing.chapter.clone();
                    notes.push(cloud);
                }
            }
            None => {
                if let Some(cloud) = LocalNote::from_db_book_note(&wire) {
                    notes.push(cloud);
                }
            }
        }
    }

    // A local note the merge never mentioned (should not happen) stays.
    for note in local {
        if by_id.contains_key(note.id.as_str()) {
            notes.push(note.clone());
        }
    }

    gone.sort_by(|a, b| a.id.cmp(&b.id));
    gone.dedup_by(|a, b| a.id == b.id);
    NoteMerge {
        notes,
        tombstones: gone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::client::SyncClient;
    use crate::sync::mock::MockServer;
    use crate::sync::records::SyncPushRequest;
    use crate::sync::{DbBook, SyncConfig, SyncPullResponse};

    fn note(id: &str, text: &str, updated_at: i64) -> LocalNote {
        LocalNote {
            id: id.to_string(),
            kind: NoteType::Annotation,
            position: Some((
                "/body/DocFragment[1]/p[1].0".to_string(),
                "/body/DocFragment[1]/p[1].10".to_string(),
            )),
            text: Some(text.to_string()),
            note: None,
            color: Some("yellow".to_string()),
            style: Some("highlight".to_string()),
            page: Some("12".to_string()),
            location: Some((10, 11)),
            chapter: Some("Chapter 1".to_string()),
            source: READEST_SOURCE.to_string(),
            created_at: updated_at,
            updated_at,
        }
    }

    /// One simulated device: its local notes and the server side of a pull.
    struct Device {
        local: Vec<LocalNote>,
        tombstones: Vec<LocalTombstone>,
    }

    impl Device {
        fn new() -> Self {
            Self {
                local: Vec::new(),
                tombstones: Vec::new(),
            }
        }

        fn apply(&mut self, book_hash: &str, pulled: &SyncPullResponse) {
            let merged =
                merge_pulled_notes(book_hash, &self.local, &self.tombstones, &pulled.notes);
            self.local = merged.notes;
            self.tombstones = merged.tombstones;
        }

        fn push_request(&self, book_hash: &str) -> SyncPushRequest {
            let mut notes: Vec<DbBookNote> = self
                .local
                .iter()
                .map(|n| n.to_db_book_note(book_hash))
                .collect();
            notes.extend(
                self.tombstones
                    .iter()
                    .map(|t| tombstone_to_db_book_note(book_hash, t)),
            );
            SyncPushRequest {
                notes,
                ..Default::default()
            }
        }

        fn delete(&mut self, id: &str, at: i64) {
            self.local.retain(|n| n.id != id);
            self.tombstones.retain(|t| t.id != id);
            self.tombstones.push(LocalTombstone {
                id: id.to_string(),
                deleted_at: at,
            });
        }
    }

    fn sync_round(
        book_hash: &str,
        from: &Device,
        to: &mut Device,
        client: &SyncClient<MockServer>,
        since: &mut i64,
    ) {
        client
            .push(from.push_request(book_hash), None)
            .expect("push");
        let pulled = client.pull_internal(None, *since, Some(100), None).unwrap();
        to.apply(book_hash, &pulled);
        *since = client.now_ms();
    }

    #[test]
    fn three_rounds_do_not_duplicate_notes() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let client_a = SyncClient::new(server.clone(), config.clone());
        let client_b = SyncClient::new(server.clone(), config);
        // The app's cursor is the device clock; keep the server's near it.
        server.set_clock_ms(client_a.now_ms());
        client_a.login("test@example.com", "password123").unwrap();
        client_b.login("test@example.com", "password123").unwrap();

        let book = "book-stable-id";
        let mut a = Device::new();
        a.local
            .push(note("note-1", "one highlight", 1_793_360_400_000));
        a.local
            .push(note("note-2", "another one", 1_793_360_401_000));
        let mut b = Device::new();
        let mut since = 0;

        for round in 0..3 {
            server.advance_clock_ms(1_000);
            sync_round(book, &a, &mut b, &client_b, &mut since);
            assert_eq!(a.local.len(), 2, "device A round {round}");
            assert_eq!(b.local.len(), 2, "device B round {round}");

            server.advance_clock_ms(1_000);
            sync_round(book, &b, &mut a, &client_a, &mut since);
            assert_eq!(a.local.len(), 2, "device A round {round} after push back");
            assert_eq!(b.local.len(), 2, "device B round {round} after push back");
        }
    }

    #[test]
    fn wire_roundtrip_keeps_id_color_style_and_modification_time() {
        let mut n = note("note-rt", "roundtrip", 1_793_360_400_000);
        n.updated_at = 1_793_360_470_000;
        let wire = n.to_db_book_note("book");
        assert_eq!(wire.id, "note-rt");
        assert_eq!(wire.color.as_deref(), Some("yellow"));
        assert_eq!(wire.style.as_deref(), Some("highlight"));
        assert_eq!(
            wire.updated_at.as_deref(),
            Some(crate::sync::format_iso8601(1_793_360_470_000).as_str()),
            "updated_at must be the modification time, not created_at"
        );

        let back = LocalNote::from_db_book_note(&wire).expect("active note");
        assert_eq!(back.id, "note-rt");
        assert_eq!(back.color.as_deref(), Some("yellow"));
        assert_eq!(back.style.as_deref(), Some("highlight"));
        assert!(back.is_cloud_owned());
    }

    #[test]
    fn merged_note_map_has_one_entry_per_id() {
        let now = 1_793_360_400_000;
        let local = vec![note("dup", "one", now)];
        let remote = vec![DbBookNote {
            book_hash: "book".into(),
            id: "dup".into(),
            kind: "annotation".into(),
            text: Some("one".into()),
            updated_at: Some(crate::sync::format_iso8601(now)),
            ..Default::default()
        }];
        let merged = merge_pulled_notes("book", &local, &[], &remote);
        assert_eq!(merged.notes.len(), 1);
        assert_eq!(merged.notes[0].id, "dup");
    }

    #[test]
    fn deletion_reaches_the_other_device_and_stays_deleted() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let client_a = SyncClient::new(server.clone(), config.clone());
        let client_b = SyncClient::new(server.clone(), config);
        server.set_clock_ms(client_a.now_ms());
        client_a.login("test@example.com", "password123").unwrap();
        client_b.login("test@example.com", "password123").unwrap();

        let book = "book-delete";
        let mut a = Device::new();
        a.local
            .push(note("note-keep", "keep me", 1_793_360_400_000));
        a.local
            .push(note("note-drop", "drop me", 1_793_360_401_000));
        let mut b = Device::new();
        let mut since = 0;

        sync_round(book, &a, &mut b, &client_b, &mut since);
        assert_eq!(b.local.len(), 2);

        // Kindle deletes one annotation: the tombstone goes up, B sees it.
        server.advance_clock_ms(2_000);
        a.delete("note-drop", server.clock_ms());
        sync_round(book, &a, &mut b, &client_b, &mut since);
        assert_eq!(a.local.len(), 1);
        assert_eq!(b.local.len(), 1);
        assert_eq!(b.local[0].id, "note-keep");

        // Round trips do not resurrect the deleted note.
        for _ in 0..3 {
            server.advance_clock_ms(1_000);
            sync_round(book, &b, &mut a, &client_a, &mut since);
            server.advance_clock_ms(1_000);
            sync_round(book, &a, &mut b, &client_b, &mut since);
            assert_eq!(a.local.len(), 1, "A resurrected the deleted note");
            assert_eq!(b.local.len(), 1, "B resurrected the deleted note");
        }
    }

    #[test]
    fn remote_tombstone_removes_a_local_cloud_note() {
        let now = 1_793_360_400_000;
        let local = vec![note("note-x", "to be deleted", now)];
        let remote = vec![DbBookNote {
            book_hash: "book".into(),
            id: "note-x".into(),
            kind: "annotation".into(),
            updated_at: Some(crate::sync::format_iso8601(now + 5_000)),
            deleted_at: Some(crate::sync::format_iso8601(now + 5_000)),
            ..Default::default()
        }];

        let merged = merge_pulled_notes("book", &local, &[], &remote);
        assert!(merged.notes.is_empty());
        assert_eq!(merged.tombstones.len(), 1);
        assert_eq!(merged.tombstones[0].id, "note-x");
    }

    #[test]
    fn tombstone_survives_an_unrelated_round() {
        let now = 1_793_360_400_000;
        let tombstone = LocalTombstone {
            id: "gone".into(),
            deleted_at: now,
        };
        let merged = merge_pulled_notes("book", &[], std::slice::from_ref(&tombstone), &[]);
        assert!(merged.notes.is_empty());
        assert_eq!(merged.tombstones, vec![tombstone]);
    }

    #[test]
    fn local_only_note_keeps_page_chapter_and_source() {
        let now = 1_793_360_400_000;
        let mut local = note("note-clip", "a clipping", now);
        local.source = "kindle-clippings".into();
        local.page = Some("vii".into());
        local.chapter = Some("Prologue".into());

        // The cloud has a newer, different version of the same id.
        let remote = DbBookNote {
            book_hash: "book".into(),
            id: "note-clip".into(),
            kind: "annotation".into(),
            text: Some("cloud text".into()),
            updated_at: Some(crate::sync::format_iso8601(now + 60_000)),
            ..Default::default()
        };

        let merged = merge_pulled_notes("book", &[local.clone()], &[], &[remote]);
        assert_eq!(merged.notes, vec![local]);
    }

    #[test]
    fn a_note_of_another_book_is_not_merged_in() {
        let remote = vec![
            DbBookNote {
                book_hash: "book-a".into(),
                id: "note-a".into(),
                kind: "annotation".into(),
                text: Some("mine".into()),
                ..Default::default()
            },
            DbBookNote {
                book_hash: "book-b".into(),
                id: "note-b".into(),
                kind: "annotation".into(),
                text: Some("theirs".into()),
                ..Default::default()
            },
        ];

        let merged = merge_pulled_notes("book-a", &[], &[], &remote);
        assert_eq!(
            merged.notes.len(),
            1,
            "the note of the other book must stay out of this book"
        );
        assert_eq!(merged.notes[0].id, "note-a");
    }

    /// The server answers with the notes of every book when the pull names
    /// none. The merge of one book must still keep only its own notes.
    #[test]
    fn a_pull_without_a_book_filter_does_not_mix_the_books() {
        let server = MockServer::new();
        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);
        client.login("test@example.com", "password123").unwrap();

        let request = SyncPushRequest {
            books: vec![
                DbBook {
                    book_hash: "book-a".into(),
                    title: "A".into(),
                    ..Default::default()
                },
                DbBook {
                    book_hash: "book-b".into(),
                    title: "B".into(),
                    ..Default::default()
                },
            ],
            notes: vec![
                DbBookNote {
                    book_hash: "book-a".into(),
                    id: "note-a".into(),
                    kind: "annotation".into(),
                    text: Some("mine".into()),
                    ..Default::default()
                },
                DbBookNote {
                    book_hash: "book-b".into(),
                    id: "note-b".into(),
                    kind: "annotation".into(),
                    text: Some("theirs".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        client.push(request, None).expect("push");

        let pulled = client
            .pull_internal(Some("notes"), 0, Some(100), None)
            .expect("pull");
        assert_eq!(pulled.notes.len(), 2, "the server answered with both books");

        let merged = merge_pulled_notes("book-a", &[], &[], &pulled.notes);
        assert_eq!(merged.notes.len(), 1);
        assert_eq!(merged.notes[0].id, "note-a");
    }
}
