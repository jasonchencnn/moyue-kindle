//! Platform-independent core: book identity, locators, notes and merge rules.
//! Wire formats stay compatible with Readest / KOReader (see docs/readest-sync-notes.md).

pub mod cloud;
pub mod hash;
pub mod jianguoyun;
pub mod library;
pub mod merge;
pub mod model;
pub mod progress;
pub mod sync;
pub mod weread;

pub use cloud::{
    derive_file_key, derive_file_key_r2, derive_file_key_s3, detect_meta_hash_matches,
    download_and_verify, download_with_ticket_renewal, ensure_valid_ticket, fetch_cloud_books,
    fetch_cloud_books_paged, fetch_cloud_books_with_user, filter_and_sort_cloud_books,
    format_to_extension, is_book_file_extension, list_book_files, make_safe_filename,
    request_download_ticket, resolve_book_file_key, select_book_file, select_book_file_key,
    verify_download_hash, CloudBook, CloudError, DownloadOptions, DownloadTicket, StorageBackend,
    StorageFileRecord,
};
