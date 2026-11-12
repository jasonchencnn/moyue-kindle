//! Readest synchronization client, models, and protocol implementation.

pub mod auth;
pub mod client;
pub mod jobs;
pub mod local;
pub mod merge;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod policy;
pub mod records;
pub mod session;
pub mod time;
pub mod transport;

pub use auth::{
    build_password_login_request, build_refresh_request, login, parse_token_response, refresh,
    AuthError, Session, SyncConfig,
};
pub use client::{SyncClient, SyncError, SyncErrorKind, BATCH_SIZE, DEFAULT_EXPIRY_MARGIN_MS};
pub use jobs::{BookJob, BookJobs, JobKind};
pub use merge::{determine_row_winner, merge_book_config, merge_book_rows, merge_notes, RowWinner};
#[cfg(any(test, feature = "mock"))]
pub use mock::MockServer;
pub use policy::{
    filter_pulled_config_for_txt, filter_push_payload, is_txt_format, resolve_reading_position,
    should_apply_remote_progress, PositionResolution,
};
pub use records::{
    DbBook, DbBookConfig, DbBookNote, StatBookRecord, StatPageRecord, SyncPullResponse,
    SyncPushRequest, SyncPushResponse,
};
pub use session::{SessionStore, TokenError};
pub use time::{format_iso8601, parse_iso8601, TimeError};
pub use transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
