//! Data model mirroring Readest `BookConfig` / `BookNote` (camelCase JSON).

use serde::{Deserialize, Serialize};

/// A position in a book, stored in several formats so any engine can resolve it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Locator {
    /// EPUB CFI (Readest / foliate-js).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfi: Option<String>,
    /// crengine XPointer, e.g. `/body/DocFragment[11]/body/div/p[3]/text().0` (KOReader).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpointer: Option<String>,
    /// Engine-neutral fallback: fraction 0..=1 through the book.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteType {
    Bookmark,
    Annotation,
    Excerpt,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookNote {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: NoteType,
    #[serde(default)]
    pub cfi: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpointer0: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpointer1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default)]
    pub note: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
}

/// Local annotation representation used on device (highlights, bookmarks, notes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: NoteType,
    /// Exact position as (start_xpointer, end_xpointer).
    /// For bookmarks, the second element is empty or None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
}

/// Strip an optional `"xpointer:"` prefix and validate that the result is a
/// real crengine XPointer (starts with `'/'`).
///
/// Non-XPointer locators (`epub:`, `kindle:`, `page:`, `fraction:`, `txt2:`,
/// or empty strings) return an empty string so callers never push invalid
/// locators as Readest XPointers.
pub fn clean_xpointer(s: &str) -> String {
    let trimmed = s.trim();
    let candidate = trimmed
        .strip_prefix("xpointer:")
        .map(str::trim)
        .unwrap_or(trimmed);
    if candidate.starts_with('/') {
        candidate.to_string()
    } else {
        String::new()
    }
}

impl Annotation {
    /// Convert local `Annotation` into wire/contract `BookNote`.
    pub fn to_book_note(&self, default_id: &str) -> BookNote {
        let (xpointer0, xpointer1) = match &self.position {
            Some((start, end)) => {
                let s = clean_xpointer(start);
                let e = clean_xpointer(end);
                let s_opt = if s.is_empty() { None } else { Some(s) };
                let e_opt = if e.is_empty() || self.kind == NoteType::Bookmark {
                    None
                } else {
                    Some(e)
                };
                (s_opt, e_opt)
            }
            None => (None, None),
        };

        BookNote {
            id: self.id.clone().unwrap_or_else(|| default_id.to_string()),
            kind: self.kind.clone(),
            cfi: String::new(),
            xpointer0,
            xpointer1,
            text: self.text.clone(),
            color: self.color.clone(),
            style: self.style.clone(),
            note: self.note.clone().unwrap_or_default(),
            created_at: self.created_at.unwrap_or(0),
            updated_at: self.updated_at.unwrap_or(0),
            deleted_at: self.deleted_at,
        }
    }

    /// Convert `BookNote` into local `Annotation`.
    ///
    /// If position cannot be located (neither xpointer0 nor valid xpointer range),
    /// position is set to `None` ("无法定位"), and no page-fraction fallback is performed.
    pub fn from_book_note(bn: &BookNote) -> Self {
        let position = match (&bn.xpointer0, &bn.xpointer1) {
            (Some(s), Some(e)) if !s.is_empty() && !e.is_empty() => Some((s.clone(), e.clone())),
            (Some(s), None) if !s.is_empty() => Some((s.clone(), String::new())),
            (Some(s), Some(e)) if !s.is_empty() && e.is_empty() => Some((s.clone(), String::new())),
            _ => None,
        };

        Self {
            id: Some(bn.id.clone()),
            kind: bn.kind.clone(),
            position,
            color: bn.color.clone(),
            style: bn.style.clone(),
            text: bn.text.clone(),
            note: if bn.note.is_empty() {
                None
            } else {
                Some(bn.note.clone())
            },
            created_at: if bn.created_at > 0 {
                Some(bn.created_at)
            } else {
                None
            },
            updated_at: if bn.updated_at > 0 {
                Some(bn.updated_at)
            } else {
                None
            },
            deleted_at: bn.deleted_at,
        }
    }

    /// Convert local `Annotation` directly to contract `DbBookNote`.
    pub fn to_db_book_note(
        &self,
        book_hash: &str,
        default_id: &str,
    ) -> crate::sync::records::DbBookNote {
        let bn = self.to_book_note(default_id);
        crate::sync::records::DbBookNote::from_model(book_hash, &bn)
    }

    /// Convert `DbBookNote` into local `Annotation`.
    pub fn from_db_book_note(dbn: &crate::sync::records::DbBookNote) -> Self {
        let bn = dbn.to_model();
        Self::from_book_note(&bn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_and_book_note_roundtrip() {
        let ann = Annotation {
            id: Some("note-123".to_string()),
            kind: NoteType::Annotation,
            position: Some((
                "xpointer:/body/DocFragment[2]/body/p[3]/text().5".to_string(),
                "xpointer:/body/DocFragment[2]/body/p[3]/text().25".to_string(),
            )),
            color: Some("yellow".to_string()),
            style: Some("highlight".to_string()),
            text: Some("Important highlight text".to_string()),
            note: Some("My personal thought".to_string()),
            created_at: Some(1762340000000),
            updated_at: Some(1762340100000),
            deleted_at: None,
        };

        let bn = ann.to_book_note("default-id");
        assert_eq!(bn.id, "note-123");
        assert_eq!(bn.kind, NoteType::Annotation);
        assert_eq!(
            bn.xpointer0.as_deref(),
            Some("/body/DocFragment[2]/body/p[3]/text().5")
        );
        assert_eq!(
            bn.xpointer1.as_deref(),
            Some("/body/DocFragment[2]/body/p[3]/text().25")
        );
        assert_eq!(bn.color.as_deref(), Some("yellow"));
        assert_eq!(bn.style.as_deref(), Some("highlight"));
        assert_eq!(bn.text.as_deref(), Some("Important highlight text"));
        assert_eq!(bn.note, "My personal thought");
        assert_eq!(bn.created_at, 1762340000000);
        assert_eq!(bn.updated_at, 1762340100000);
        assert_eq!(bn.deleted_at, None);

        let roundtrip = Annotation::from_book_note(&bn);
        assert_eq!(roundtrip.id.as_deref(), Some("note-123"));
        assert_eq!(roundtrip.kind, NoteType::Annotation);
        assert_eq!(
            roundtrip.position,
            Some((
                "/body/DocFragment[2]/body/p[3]/text().5".to_string(),
                "/body/DocFragment[2]/body/p[3]/text().25".to_string()
            ))
        );
        assert_eq!(roundtrip.color.as_deref(), Some("yellow"));
        assert_eq!(roundtrip.text.as_deref(), Some("Important highlight text"));
        assert_eq!(roundtrip.note.as_deref(), Some("My personal thought"));
    }

    #[test]
    fn bookmark_and_tombstone_mapping() {
        let bm = Annotation {
            id: None,
            kind: NoteType::Bookmark,
            position: Some((
                "/body/DocFragment[5]/body/h1[1]/text().0".to_string(),
                String::new(),
            )),
            color: None,
            style: None,
            text: None,
            note: None,
            created_at: Some(1000),
            updated_at: Some(2000),
            deleted_at: Some(2000), // Tombstone
        };

        let bn = bm.to_book_note("generated-bm-id");
        assert_eq!(bn.id, "generated-bm-id");
        assert_eq!(bn.kind, NoteType::Bookmark);
        assert_eq!(
            bn.xpointer0.as_deref(),
            Some("/body/DocFragment[5]/body/h1[1]/text().0")
        );
        assert_eq!(bn.xpointer1, None);
        assert_eq!(bn.deleted_at, Some(2000));

        let back = Annotation::from_book_note(&bn);
        assert_eq!(back.id.as_deref(), Some("generated-bm-id"));
        assert_eq!(back.deleted_at, Some(2000));
        assert_eq!(
            back.position,
            Some((
                "/body/DocFragment[5]/body/h1[1]/text().0".to_string(),
                String::new()
            ))
        );
    }

    #[test]
    fn cfi_only_position_is_unresolvable_none() {
        let bn = BookNote {
            id: "cfi-only".to_string(),
            kind: NoteType::Annotation,
            cfi: "epubcfi(/6/4[chap01]!/4/2/10,/1:0,/1:15)".to_string(),
            xpointer0: None,
            xpointer1: None,
            text: Some("Phone highlight".to_string()),
            color: Some("green".to_string()),
            style: None,
            note: String::new(),
            created_at: 1000,
            updated_at: 1000,
            deleted_at: None,
        };

        let ann = Annotation::from_book_note(&bn);
        // Position cannot be located from CFI alone, skips without fallback
        assert_eq!(ann.position, None);
        assert_eq!(ann.text.as_deref(), Some("Phone highlight"));
    }

    #[test]
    fn db_book_note_roundtrip() {
        let ann = Annotation {
            id: Some("note-db".to_string()),
            kind: NoteType::Annotation,
            position: Some((
                "/body/DocFragment[1]/p[1].0".to_string(),
                "/body/DocFragment[1]/p[1].10".to_string(),
            )),
            color: Some("blue".to_string()),
            style: Some("underline".to_string()),
            text: Some("Underlined text".to_string()),
            note: Some("A note".to_string()),
            created_at: Some(1762340000000),
            updated_at: Some(1762340200000),
            deleted_at: None,
        };

        let db_note = ann.to_db_book_note("hash-abc", "fallback-id");
        assert_eq!(db_note.book_hash, "hash-abc");
        assert_eq!(db_note.id, "note-db");
        assert_eq!(db_note.color.as_deref(), Some("blue"));
        assert_eq!(db_note.style.as_deref(), Some("underline"));

        let converted_back = Annotation::from_db_book_note(&db_note);
        assert_eq!(converted_back.id.as_deref(), Some("note-db"));
        assert_eq!(converted_back.color.as_deref(), Some("blue"));
        assert_eq!(converted_back.position, ann.position);
    }

    #[test]
    fn clean_xpointer_rejects_non_crengine_locators() {
        assert_eq!(
            clean_xpointer("xpointer:/body/DocFragment[1]/p[2].0"),
            "/body/DocFragment[1]/p[2].0"
        );
        assert_eq!(
            clean_xpointer("/body/DocFragment[1]/p[2].0"),
            "/body/DocFragment[1]/p[2].0"
        );
        assert_eq!(clean_xpointer("epub:3:120"), "");
        assert_eq!(clean_xpointer("kindle:4500"), "");
        assert_eq!(clean_xpointer("page:12"), "");
        assert_eq!(clean_xpointer("fraction:0.42"), "");
        assert_eq!(
            clean_xpointer("txt2:xpointer:/body/DocFragment[1]/p[1].0"),
            ""
        );
        assert_eq!(clean_xpointer("xpointer:epub:3:120"), "");
        assert_eq!(clean_xpointer("   "), "");

        let ann = Annotation {
            id: Some("plato-hl".to_string()),
            kind: NoteType::Annotation,
            position: Some(("epub:3:120".to_string(), "epub:3:180".to_string())),
            color: Some("yellow".to_string()),
            style: Some("highlight".to_string()),
            text: Some("Plato highlight".to_string()),
            note: None,
            created_at: None,
            updated_at: None,
            deleted_at: None,
        };
        let bn = ann.to_book_note("fallback");
        assert_eq!(bn.xpointer0, None);
        assert_eq!(bn.xpointer1, None);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_hash: Option<String>,
    /// `[current, total]`, 1-based.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xpointer: Option<String>,
    #[serde(default)]
    pub booknotes: Vec<BookNote>,
    #[serde(default)]
    pub updated_at: i64,
}
