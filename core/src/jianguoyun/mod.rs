//! Jianguoyun (Nutstore) WebDAV channel: browsing, per-book sync records and LWW merges.
//!
//! The wire contract is `docs/sync-contract.md` §15 (v1). HTTP is injected through
//! [`crate::sync::transport`], so this module has no network stack, no global state and no
//! credentials in `Debug` output. `mock` is available to integration tests and to builds with the
//! `mock` feature.

pub mod client;
pub mod merge;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod model;
pub mod xml;

pub use client::{
    parse_retry_after, JianguoyunClient, PROPFIND_CONTENT_TYPE, SYNC_CONTENT_TYPE, USER_AGENT,
};
pub use merge::{decide_progress, merge_book_sync, merge_notes, merge_progress, ProgressDecision};
#[cfg(any(test, feature = "mock"))]
pub use mock::{MockRequest, MockServer};
pub use model::{
    is_ebook_extension, validate_book_hash, JianguoyunBookSync, JianguoyunConfig, JianguoyunError,
    JianguoyunErrorKind, JianguoyunNote, JianguoyunProgress, NoteKind, RemoteEntry,
    DEFAULT_WEBDAV_BASE, EBOOK_EXTENSIONS, MAX_BOOK_HASH_LEN, SYNC_DIR_NAME, SYNC_SCHEMA_VERSION,
    SYNC_SUBDIR,
};
pub use xml::{parse_propfind_response, PROPFIND_BODY};
