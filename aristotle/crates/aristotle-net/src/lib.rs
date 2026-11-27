// SPDX-License-Identifier: AGPL-3.0-or-later
//! Aristotle's network client.
//!
//! - [`http`]: a blocking HTTP client over `ureq` with timeouts, size
//!   limits, redirects checked against the device's allowlist ([`policy`])
//!   and Basic or Digest auth ([`auth`]).
//! - [`kosync`]: KOReader's progress sync protocol.
//! - [`jianguoyun`]: the Jianguoyun (Nutstore) WebDAV channel: the transport
//!   `inkread_core::jianguoyun` runs on, and the streaming book download.
//! - [`opds`]: OPDS 1.2 catalogs, and [`download`] to save their books.
//! - [`ota`]: the `MOYUE-OTA-1` release manifest and the verified download
//!   of an update package.
//! - [`transfer`]: the small server that takes books over the air, and the
//!   rules that decide where a file that arrives may be written.
//!
//! Pure Rust by default, plain HTTP only. The `tls` feature adds HTTPS
//! through rustls and ring, which compiles C (see `Cargo.toml`). Nothing
//! here runs on its own: the app calls it from worker threads, and only
//! for servers and catalogs the reader has set up (`docs/library.md`,
//! "Progress sync" and "Catalogs"). The one exception is [`transfer`],
//! which runs its own thread and listens on the device's network while
//! its page is open.

pub mod auth;
pub mod cloud;
mod dial;
pub mod download;
pub mod endpoint;
pub mod http;
pub mod jianguoyun;
pub mod kosync;
pub mod lan_input;
pub mod localtime;
pub mod opds;
pub mod ota;
pub mod policy;
pub mod readest;
pub mod store;
pub mod transfer;
pub mod translate;
pub mod tts;
pub mod url;

#[cfg(test)]
mod testserver;

pub use auth::Credentials;
pub use cloud::download_book;
pub use http::{Client, Error, Request, Timeouts};
pub use jianguoyun::JianguoyunTransport;
pub use policy::Policy;
pub use readest::ReadestTransport;
pub use translate::{Config, Source, Translation, Translator};
pub use url::Url;

/// Whether this build speaks HTTPS.
pub const HTTPS: bool = cfg!(feature = "tls");
