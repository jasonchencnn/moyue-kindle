// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readest account authentication and two-way progress / note synchronization.
//!
//! Follows `docs/sync-contract.md`:
//! - Auth: GoTrue email + password login (tokens never logged or exposed in Debug).
//! - Reading progress: only forward-progress is accepted; crengine XPointers
//!   are used for positioning. Incoming CFI-only positions show an informational
//!   notice without jumping (contract §4/§6).
//! - Annotations: bidirectional mapping between local [`Annotation`] and
//!   [`DbBookNote`] through [`LocalNote`]; every annotation has a stable id, and
//!   merges use `core::sync::merge_notes` (LWW with tombstone propagation).
//! - Formats: TXT progress/location/XPointer and notes are filtered out (contract §8).

use std::collections::HashSet;

use aristotle_i18n::{tf, tr, Msg};
use aristotle_library::index::{Annotation, AnnotationKind, BookRecord};
use aristotle_library::Progress;
use aristotle_net::store::{Accounts, ReadestEndpointField, ReadestSettings};
use aristotle_net::ReadestTransport;
use aristotle_render::plato_core::anyhow::Error;
use aristotle_ui::screens::readest_edit::{self as ui_readest_edit, ReadestEditOutcome};
use aristotle_ui::screens::readest_login::{
    self as ui_readest_login, ReadestLoginOutcome, ReadestLoginState,
};
use aristotle_ui::screens::readest_status::{self as ui_readest_status, ReadestStatusView};
use aristotle_ui::tree::{Refresh, Tree};
use aristotle_ui::ui::UiEvent;

use inkread_core::model::{clean_xpointer, NoteType};
use inkread_core::sync::local::{
    merge_pulled_notes, tombstone_to_db_book_note, LocalNote, LocalTombstone,
};
use inkread_core::sync::policy::{plan_progress_update, PositionResolution, ProgressPlan};
use inkread_core::sync::records::{
    format_progress_pair, DbBookConfig, DbBookNote, SyncPushRequest,
};
use inkread_core::sync::time::format_iso8601;
use inkread_core::sync::{
    BookJob, BookJobs, JobKind, Session, SyncClient, SyncConfig, SyncPullResponse,
};

use crate::device::Facts;
use crate::net::{Done, Net};
use crate::reading::Overlay;
use crate::{perf, App, Flow, Screen};

/// Which part of a full synchronization round failed, for the status screen.
///
/// The three pulls and the push are separate requests against different
/// records; saying which one stopped first is what tells a reader whether the
/// progress, the highlights or the library did not make it (`SYNC-111`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStage {
    Library,
    Progress,
    Notes,
    Push,
}

impl SyncStage {
    /// The stage's name in the reader's language.
    pub fn label(self) -> &'static str {
        match self {
            Self::Library => tr(Msg::ReadestStageLibrary),
            Self::Progress => tr(Msg::ReadestStageProgress),
            Self::Notes => tr(Msg::ReadestStageNotes),
            Self::Push => tr(Msg::ReadestStagePush),
        }
    }
}

/// The newest server timestamp of each record type a round pulled.
///
/// `None` means the round pulled nothing of that type, so the stored cursor
/// must stay where it was: jumping to "now" would skip whatever the server
/// stamped while the round was running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PulledCursors {
    pub books: Option<i64>,
    pub configs: Option<i64>,
    pub notes: Option<i64>,
}

/// Application-level state for Readest synchronization.
#[derive(Clone, Debug, Default)]
pub struct ReadestState {
    pub last_sync_time: Option<String>,
    pub pending_count: usize,
    pub is_syncing: bool,
    pub message: Option<String>,
    pub offer: Option<(String, usize, String)>,
    pub next_token: u64,
    pub opened_book: Option<String>,
    pub opened_token: u64,
    /// The local progress timestamp (`ms`) before this open touched the book.
    pub opened_before_ms: i64,
    /// The local locator when the book opened, to detect whether the reader turned a page.
    pub opened_locator: Option<aristotle_library::Locator>,
    /// The manual pull / push of the open book in flight, if any (`SYNC-111`).
    pub jobs: BookJobs,
    /// One line about the last manual pull or push, shown in the reader menu.
    pub manual_message: Option<String>,
    /// A current-book response waiting for background layout or wake.
    pub pending_pull: Option<(String, SyncPullResponse)>,
    /// Whether `pending_pull` came from an explicit manual pull in the reader menu.
    pub pending_pull_manual: bool,
}

impl ReadestState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last_sync_display(&self) -> Option<String> {
        self.last_sync_time.clone()
    }
}

/// The endpoint editor screen and the setting it writes.
pub(crate) struct ReadestEditScreen {
    pub field: crate::net::TextField,
    pub state: ui_readest_edit::ReadestEditState,
}

/// Convert a local library [`Annotation`] into the platform-neutral local note
/// model that carries its stable id across pushes and pulls.
pub fn local_note_from_annotation(a: &Annotation) -> LocalNote {
    LocalNote {
        id: a.id.clone(),
        kind: match a.kind {
            AnnotationKind::Bookmark => NoteType::Bookmark,
            AnnotationKind::Highlight | AnnotationKind::Note => NoteType::Annotation,
        },
        position: a.position.clone(),
        text: a.text.clone(),
        note: a.note.clone(),
        color: a.color.clone(),
        style: a.style.clone(),
        page: a.page.clone(),
        location: a.location,
        chapter: a.chapter.clone(),
        source: a.source.clone(),
        // The library writes local wall-clock seconds; `LocalNote` and the
        // wire record count milliseconds, as `annotation_from_local_note`
        // and every other reader of `created_at` assume.
        created_at: a
            .created
            .as_deref()
            .and_then(crate::marks::parse_local_time)
            .map_or(0, |secs| secs * 1000),
        updated_at: a
            .modified
            .as_deref()
            .or(a.created.as_deref())
            .and_then(crate::marks::parse_local_time)
            .map_or(0, |secs| secs * 1000),
    }
}

/// Convert a merged local note back into the library's annotation.
pub fn annotation_from_local_note(n: LocalNote) -> Annotation {
    Annotation {
        id: n.id,
        kind: match n.kind {
            NoteType::Bookmark => AnnotationKind::Bookmark,
            _ if n.text.is_none() && n.note.is_some() => AnnotationKind::Note,
            _ => AnnotationKind::Highlight,
        },
        text: n.text,
        page: n.page,
        location: n.location,
        position: n.position,
        created: (n.created_at > 0).then(|| crate::marks::local_time(n.created_at / 1000)),
        modified: (n.updated_at > n.created_at.max(0))
            .then(|| crate::marks::local_time(n.updated_at / 1000)),
        color: n.color,
        style: n.style,
        source: n.source,
        chapter: n.chapter,
        note: n.note,
    }
}

/// Convert local library [`Annotation`] into cloud [`DbBookNote`].
pub fn annotation_to_db_book_note(book_hash: &str, a: &Annotation) -> DbBookNote {
    local_note_from_annotation(a).to_db_book_note(book_hash)
}

/// Merge pulled notes into a book record in place.
///
/// Notes another importer wrote keep their page, location, chapter and source;
/// only notes that came from Readest are replaced.
pub fn merge_remote_notes(rec: &mut BookRecord, remote: &[DbBookNote]) -> bool {
    let local: Vec<LocalNote> = rec
        .annotations
        .iter()
        .map(local_note_from_annotation)
        .collect();
    let tombstones: Vec<LocalTombstone> = rec
        .annotation_tombstones
        .iter()
        .map(|t| LocalTombstone {
            id: t.id.clone(),
            deleted_at: t.deleted_at,
        })
        .collect();
    let before: Vec<(String, Option<String>)> = rec
        .annotations
        .iter()
        .map(|a| (a.id.clone(), a.modified.clone()))
        .collect();
    let merged = merge_pulled_notes(&rec.id, &local, &tombstones, remote);
    let after: Vec<(String, Option<String>)> = merged
        .notes
        .iter()
        .map(|n| {
            (
                n.id.clone(),
                (n.updated_at > n.created_at)
                    .then(|| crate::marks::local_time(n.updated_at / 1000)),
            )
        })
        .collect();
    let changed = before != after;
    rec.annotations = merged
        .notes
        .into_iter()
        .map(annotation_from_local_note)
        .collect();
    rec.annotation_tombstones = merged
        .tombstones
        .into_iter()
        .map(|t| aristotle_library::index::AnnotationTombstone {
            id: t.id,
            deleted_at: t.deleted_at,
        })
        .collect();
    changed
}

/// Convert local library [`Progress`] into cloud [`DbBookConfig`].
pub fn progress_to_db_book_config(
    book_hash: &str,
    p: &Progress,
    pages: Option<(u32, u32)>,
) -> DbBookConfig {
    let xpointer = match &p.locator {
        aristotle_library::Locator::XPointer { xpointer } => {
            let clean = clean_xpointer(xpointer);
            if clean.is_empty() {
                None
            } else {
                Some(clean)
            }
        }
        _ => None,
    };
    let updated_at = format_iso8601(p.updated * 1000);
    DbBookConfig {
        user_id: None,
        book_hash: book_hash.to_string(),
        meta_hash: None,
        location: None,
        xpointer,
        progress: format_progress_pair(pages),
        rsvp_position: None,
        search_config: None,
        view_settings: None,
        created_at: Some(updated_at.clone()),
        updated_at: Some(updated_at),
        deleted_at: None,
    }
}

/// The connection settings a login request uses: the endpoints from the
/// settings screen, not the defaults.
fn login_config(accounts: &Accounts) -> SyncConfig {
    accounts.readest_config()
}

impl App {
    /// Navigate to Readest login screen.
    pub(crate) fn show_readest_login(&mut self, refresh: Refresh) -> Result<(), Error> {
        // A fresh `scoped` device has no allowlist entry yet; the login
        // request itself must be allowed, not just the ones after it.
        self.net
            .ensure_allow_entries(self.net.accounts.readest_allow_entries());
        let state = ReadestLoginState::new();
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let mut tree = Tree::new();
        let root =
            ui_readest_login::build_readest_login_screen(&mut tree, &self.styles, &state, &status);
        tree.set_root(root);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::ReadestLogin(Box::new(state));
        self.render_ui()
    }

    /// Navigate to Readest status screen.
    pub(crate) fn show_readest_status(&mut self, refresh: Refresh) -> Result<(), Error> {
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let readest = self.net.accounts.readest.as_ref();
        let email = readest.map(|r| r.email.as_str());
        let config = self.net.accounts.readest_config();
        let api_base = &config.api_base;
        let supabase_url = &config.supabase_url;
        let last_sync = readest
            .and_then(|r| r.last_sync_time)
            .map(|ms| crate::marks::local_time(ms / 1000))
            .or_else(|| self.readest.last_sync_display());
        let pending_count = readest.map(|r| r.pending_pushes()).unwrap_or(0);
        self.readest.pending_count = pending_count;

        let view = ReadestStatusView {
            status,
            email,
            last_sync: last_sync.as_deref(),
            pending_count,
            is_syncing: self.readest.is_syncing,
            message: self.readest.message.as_deref(),
            auto_push: self
                .net
                .accounts
                .readest
                .as_ref()
                .is_some_and(|s| s.auto_push_enabled()),
            api_base,
            supabase_url,
            anon_key_masked: "••••••••",
            show_restore_default: !self.net.accounts.readest_endpoints.is_empty(),
        };
        let mut tree = Tree::new();
        let root = ui_readest_status::build_readest_status_screen(&mut tree, &self.styles, &view);
        tree.set_root(root);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::ReadestStatus;
        self.render_ui()
    }

    /// Silently refresh the Readest access token in the background on startup.
    ///
    /// Because `access_token` is intentionally kept out of `accounts.json`,
    /// a cold launch would otherwise pay an extra auth TLS handshake + token
    /// refresh round-trip on the user's first tap. Failures are ignored so
    /// offline startup stays completely silent; any rotated refresh token is
    /// persisted on arrival via [`Done::ReadestPrewarmed`].
    pub(crate) fn readest_prewarm(&mut self) {
        if !self.prefs.enable_readest || self.net.accounts.readest.is_none() {
            return;
        }
        self.net.reconcile_allow_entries();
        let policy = Net::policy();
        let config = self.net.accounts.readest_config();
        let store = self.net.readest_session.clone();
        let epoch = store.epoch();
        self.net.spawn("readest-prewarm", move |_| {
            let client =
                SyncClient::with_session_store(ReadestTransport::new(policy), config, store);
            let _ = client.ensure_token();
            Done::ReadestPrewarmed { store: epoch }
        });
    }

    /// Triggered when a book session opens: pull remote book configuration.
    pub(crate) fn readest_book_opened(&mut self) {
        let Screen::Reader(r) = &self.screen else {
            return;
        };
        let id = r.session.id.clone();
        if !self.prefs.enable_readest || self.is_weread_book(&id) {
            return;
        }
        if self.readest.opened_book.as_deref() == Some(&id) {
            return;
        }
        if self.net.accounts.readest.is_none() {
            return;
        }
        self.readest.next_token = self.readest.next_token.wrapping_add(1);
        let token = self.readest.next_token;
        let current_progress = self.lib.book(&id).and_then(|b| b.progress.as_ref());
        let before_open_ms = self
            .sync
            .before_open
            .as_ref()
            .filter(|(b, _)| b == &id)
            .and_then(|(_, p)| p.as_ref().map(|p| p.updated * 1000))
            .or_else(|| current_progress.map(|p| p.updated * 1000))
            .unwrap_or(0);
        self.readest.opened_book = Some(id.clone());
        self.readest.opened_token = token;
        self.readest.opened_before_ms = before_open_ms;
        self.readest.opened_locator = current_progress.map(|p| p.locator.clone());
        self.readest.offer = None;
        self.readest.manual_message = None;
        self.readest.pending_pull = None;
        self.readest.pending_pull_manual = false;

        // The hosts the settings name must be allowed before the first request
        // of this session, not only after the login screen was visited.
        self.net.reconcile_allow_entries();
        self.retry_pending_automatic();

        let policy = Net::policy();
        let config = self.net.accounts.readest_config();
        let store = self.net.readest_session.clone();
        let epoch = store.epoch();
        let book_hash = id;

        self.net.spawn("readest-pull", move |_| {
            let transport = ReadestTransport::new(policy);
            let client = SyncClient::with_session_store(transport, config, store);
            let response = client
                .pull_internal(None, 0, Some(50), Some(&book_hash))
                .map_err(|e| {
                    eprintln!(
                        "aristotle: readest: open pull failed: {}",
                        redacted_sync_error(&e)
                    );
                    e.kind()
                });
            Done::ReadestPulled {
                book: book_hash,
                token,
                store: epoch,
                response,
            }
        });
    }

    /// Handles arrival of remote book delta.
    pub(crate) fn on_readest_pulled(
        &mut self,
        book: String,
        token: u64,
        store: u64,
        response: Result<SyncPullResponse, inkread_core::sync::SyncErrorKind>,
    ) -> Result<(), Error> {
        if self.readest.opened_book.as_deref() != Some(&book) || self.readest.opened_token != token
        {
            return Ok(());
        }
        if !self.net.is_current_store(store) {
            return Ok(());
        }
        self.net.persist_session();
        let resp = match response {
            Ok(r) => r,
            Err(kind) => {
                perf!("readest: pull failed: {kind:?}");
                self.readest.message = Some(readest_error_message(kind));
                return Ok(());
            }
        };
        self.apply_readest_pull(&book, resp, false)
    }

    /// Merge a pulled delta into the library and offer its position.
    ///
    /// Shared by the automatic pull on open and the manual pull of the open
    /// book (`SYNC-111`); a position is only ever *offered*, never applied
    /// without the reader's consent (contract §5.2).
    fn apply_readest_pull(
        &mut self,
        book: &str,
        resp: SyncPullResponse,
        manual: bool,
    ) -> Result<(), Error> {
        if self.asleep.is_some()
            || matches!(&self.screen, Screen::Reader(r) if r.session.id == book && !r.session.has_book())
        {
            self.readest.pending_pull = Some((book.to_string(), resp));
            self.readest.pending_pull_manual = manual;
            return Ok(());
        }
        let book = book.to_string();
        let is_txt = self
            .lib
            .book(&book)
            .map(|b| matches!(b.format, aristotle_library::Format::Txt))
            .unwrap_or(false);

        // Merge notes if any. TXT notes stay local (contract §8).
        let mut notes_saved = false;
        if !is_txt && !resp.notes.is_empty() {
            if let Some(rec) = self.lib.book_mut(&book) {
                if merge_remote_notes(rec, &resp.notes) {
                    notes_saved = true;
                    perf!(
                        "readest: merged {} pulled notes for {book}",
                        resp.notes.len()
                    );
                }
            }
            self.reload_marks();
        }

        // Check remote reading position
        let remote_config = resp.configs.into_iter().find(|c| c.book_hash == book);
        if (notes_saved || remote_config.is_some()) && !self.is_weread_book(&book) {
            self.cloud.record_channel_hash(self.lib.data_dir(), &book);
        }
        let Some(config) = remote_config else {
            if notes_saved {
                self.saver.save(&self.lib);
            }
            return Ok(());
        };

        // Keep the cloud shelf entry aligned with the pulled config's progress.
        self.update_cloud_book_from_config(&config);

        let local_ms = if manual {
            // Explicit manual pull in the reader menu always offers a remote
            // position that differs from the current page.
            0
        } else {
            let cur = self.lib.book(&book).and_then(|b| b.progress.as_ref());
            if self.readest.opened_book.as_deref() == Some(&book)
                && cur.map(|p| &p.locator) == self.readest.opened_locator.as_ref()
            {
                self.readest.opened_before_ms
            } else {
                cur.map(|p| p.updated * 1000).unwrap_or(0)
            }
        };

        if notes_saved {
            self.saver.save(&self.lib);
        }

        // The book is open: offer the remote position, never jump silently.
        let plan = if manual && !is_txt {
            match inkread_core::sync::policy::resolve_reading_position(&config) {
                PositionResolution::Unresolvable => ProgressPlan::Ignore,
                res => ProgressPlan::Offer(res),
            }
        } else {
            plan_progress_update(local_ms, &config, true, is_txt)
        };
        match plan {
            ProgressPlan::Ignore => return Ok(()),
            ProgressPlan::Offer(PositionResolution::NavigateXPointer(xp)) => {
                let Screen::Reader(r) = &mut self.screen else {
                    return Ok(());
                };
                let page_opt = r.session.book().page_of_xpointer(&xp);
                if let Some(page) = page_opt {
                    if page != r.session.current {
                        let chapter = r.session.book().chapter_title(page).unwrap_or_default();
                        let percent_ratio = config
                            .to_model()
                            .progress
                            .filter(|(_, total)| *total > 0)
                            .map(|(cur, total)| cur as f64 / total as f64)
                            .unwrap_or_else(|| r.session.book().fraction(page) as f64);
                        let percent =
                            format!("{}%", (percent_ratio.clamp(0.0, 1.0) * 100.0).floor());
                        let text = if chapter.is_empty() {
                            tf!(Msg::ReadestJumpPrompt, page = page + 1)
                        } else {
                            tf!(
                                Msg::ReaderSyncBodyChapter,
                                device = "Readest",
                                percent = percent,
                                chapter = chapter
                            )
                        };
                        self.readest.offer = Some((book, page, text));
                        self.show_readest_sync_offer()?;
                    }
                }
            }
            ProgressPlan::Offer(PositionResolution::NeedsManualPosition) => {
                let Screen::Reader(r) = &self.screen else {
                    return Ok(());
                };
                let text = tr(Msg::ReadestCfiOnlyHint).to_string();
                self.readest.offer = Some((book, r.session.current, text));
                self.show_readest_sync_offer()?;
            }
            ProgressPlan::Offer(PositionResolution::Unresolvable) => {}
        }
        Ok(())
    }

    /// Applies the deferred current-book response once its engine is available.
    pub(crate) fn apply_pending_readest_pull(&mut self) -> Result<(), Error> {
        if self.asleep.is_some() {
            return Ok(());
        }
        let (Some((book, _)), Screen::Reader(reader)) = (&self.readest.pending_pull, &self.screen)
        else {
            return Ok(());
        };
        if reader.session.id != *book {
            self.readest.pending_pull = None;
            self.readest.pending_pull_manual = false;
            return Ok(());
        }
        if !reader.session.has_book() {
            return Ok(());
        }
        if let Some((book, response)) = self.readest.pending_pull.take() {
            let manual = std::mem::take(&mut self.readest.pending_pull_manual);
            self.apply_readest_pull(&book, response, manual)?;
        }
        Ok(())
    }

    /// Push current book state on close, sleep, or quit.
    ///
    /// `closing` forgets the open book; `wait` runs on this thread with a
    /// short timeout (sleep, quit), so a dead network cannot hold the device.
    /// This is the automatic path: it does nothing when the reader turned
    /// "push on close" off (`SYNC-111`).
    /// Whether a book belongs to the WeRead channel and must stay isolated from
    /// Readest sync (neither pulled from nor pushed to Readest).
    pub(crate) fn is_weread_book(&self, book_id: &str) -> bool {
        if self.weread.downloads.book_id_of_hash(book_id).is_some() {
            return true;
        }
        self.lib.book(book_id).is_some_and(|rec| {
            crate::shelf::source_from_path(&rec.file.path)
                == aristotle_ui::screens::library::BookSource::WeRead
        })
    }

    /// Push the open book if automatic push is enabled.
    pub(crate) fn readest_push(&mut self, closing: bool, wait: bool) {
        if !self.prefs.enable_readest || !self.readest_auto_push_enabled() {
            if closing {
                self.readest.opened_book = None;
            }
            return;
        }
        let Some(book_id) = self.readest.opened_book.clone() else {
            return;
        };
        if closing {
            self.readest.opened_book = None;
        }
        if self.is_weread_book(&book_id) {
            return;
        }
        // Inline sleep/exit pushes must not wait behind a worker's refresh.
        if wait && self.net.busy() {
            if let Some(settings) = self.net.accounts.readest.as_mut() {
                settings.enqueue_push(&book_id);
                self.readest.pending_count = settings.pending_pushes();
            }
            self.net.persist_session();
            return;
        }
        self.readest_push_book(book_id, wait, None);
    }

    /// Whether closing, sleeping or quitting pushes the open book.
    pub(crate) fn readest_auto_push_enabled(&self) -> bool {
        self.net
            .accounts
            .readest
            .as_ref()
            .is_some_and(|s| s.auto_push_enabled())
    }

    /// Retry the books whose push failed earlier. Runs when a book opens and
    /// when the user syncs, which is when the network is known to work.
    ///
    /// The retry is an automatic push, so it follows the same switch; a manual
    /// push or a full sync still sends everything.
    pub(crate) fn retry_pending_automatic(&mut self) {
        if !self.readest_auto_push_enabled() {
            return;
        }
        let Some(settings) = self.net.accounts.readest.as_ref() else {
            return;
        };
        let pending = settings.pending_queue.clone();
        for book in pending {
            self.readest_push_book(book, false, None);
        }
    }

    /// Push one book's row, config, notes and tombstones.
    ///
    /// `job` marks a manual push from the reader menu: its completion is
    /// reported in the menu and a result for a book the reader has left is
    /// dropped.
    fn readest_push_book(&mut self, book_id: String, inline: bool, job: Option<BookJob>) {
        if !self.prefs.enable_readest || self.net.accounts.readest.is_none() {
            return;
        }
        if self.is_weread_book(&book_id) {
            if let Some(r) = self.net.accounts.readest.as_mut() {
                r.dequeue_push(&book_id);
                self.net.save();
            }
            return;
        }
        let Some(rec) = self.lib.book(&book_id) else {
            // The book is gone: its queued push would retry forever.
            if let Some(r) = self.net.accounts.readest.as_mut() {
                r.dequeue_push(&book_id);
                self.net.save();
            }
            return;
        };
        let format = rec.format;
        let is_txt = matches!(format, aristotle_library::Format::Txt);

        // Contract §3.2: the page pair is `[current, total]`; when the book is
        // not currently open on screen, derive a page pair from its known cloud
        // total or a 100-page percentage scale so `books.progress` piggyback works.
        let cloud_total = self
            .cloud
            .books
            .iter()
            .find(|b| b.book_hash == book_id)
            .and_then(|b| b.progress.map(|(_, total)| total))
            .filter(|&total| total > 0);
        let pages = match &self.screen {
            Screen::Reader(r) if r.session.id == book_id => r.session.page_number(),
            _ => None,
        }
        .or_else(|| {
            rec.progress.as_ref().and_then(|p| {
                if p.fraction > 0.0 {
                    let total = cloud_total.unwrap_or(100);
                    let cur = ((p.fraction.clamp(0.0, 1.0) * total as f32).round() as u32)
                        .clamp(1, total);
                    Some((cur, total))
                } else {
                    None
                }
            })
        });
        let config_opt = if is_txt {
            None
        } else {
            rec.progress
                .as_ref()
                .map(|p| progress_to_db_book_config(&book_id, p, pages))
        };

        let notes: Vec<DbBookNote> = if is_txt {
            Vec::new()
        } else {
            let mut notes: Vec<DbBookNote> = rec
                .annotations
                .iter()
                .map(|a| annotation_to_db_book_note(&book_id, a))
                .collect();
            notes.extend(rec.annotation_tombstones.iter().map(|t| {
                tombstone_to_db_book_note(
                    &book_id,
                    &LocalTombstone {
                        id: t.id.clone(),
                        deleted_at: t.deleted_at,
                    },
                )
            }));
            notes
        };

        let mut configs = Vec::new();
        if let Some(c) = config_opt {
            configs.push(c);
        }

        // Push only `configs` and `notes` (matching readest.koplugin and
        // useProgressSync.ts): pushing a bare `DbBook` with `uploaded_at: None`
        // and `updated_at: None` would cause `transformBookToDB` on the Readest
        // server to stamp `updated_at = Date.now()` and wipe `uploaded_at` and
        // `progress` on the `books` row. Instead, the server's `configs`
        // piggyback updates `books.progress` and `books.updated_at` safely.
        let push_req = SyncPushRequest {
            books: Vec::new(),
            configs,
            notes,
            stat_books: Vec::new(),
            stat_pages: Vec::new(),
        };

        let mut txt_hashes = HashSet::new();
        if is_txt {
            txt_hashes.insert(book_id.clone());
        }

        let policy = Net::policy();
        let sync_config = self.net.accounts.readest_config();
        let store = self.net.readest_session.clone();
        let epoch = store.epoch();
        // The record as it is now: it is what the payload carries, and what
        // the answer may acknowledge (SYNC-111).
        let submitted = rec.clone();
        let book_hash = book_id;

        let run_push = move || {
            let transport = if inline {
                ReadestTransport::with_timeout(policy, 5)
            } else {
                ReadestTransport::new(policy)
            };
            let client = SyncClient::with_session_store(transport, sync_config, store);
            let res = client.push(push_req, Some(&txt_hashes)).map(|_| ());
            if let Err(ref e) = res {
                eprintln!(
                    "aristotle: readest: push failed (inline={inline}): {}",
                    redacted_sync_error(e)
                );
            }
            Done::ReadestPushed {
                book: book_hash,
                job,
                store: epoch,
                submitted: Some(submitted),
                result: res.map_err(|e| e.kind()),
            }
        };

        if inline {
            let done = run_push();
            if let Done::ReadestPushed {
                book,
                job,
                store,
                submitted,
                result,
            } = done
            {
                let _ = self.on_readest_pushed(book, job, store, submitted, result);
            }
        } else {
            self.net.spawn("readest-push", move |_| run_push());
        }
    }

    /// Handles arrival of push outcome.
    pub(crate) fn on_readest_pushed(
        &mut self,
        book: String,
        job: Option<BookJob>,
        store: u64,
        submitted: Option<BookRecord>,
        result: Result<(), inkread_core::sync::SyncErrorKind>,
    ) -> Result<(), Error> {
        if !self.net.is_current_store(store) {
            return Ok(());
        }
        self.net.persist_session();
        // A manual push whose book the reader has left, or that a later tap
        // replaced, is stale: its result belongs to nothing on screen.
        if let Some(job) = &job {
            if !self.readest.jobs.finish(job) {
                perf!("readest: dropped a stale push result for {}", job.book);
                return Ok(());
            }
        }
        let mut queue_changed = false;
        match result {
            Ok(()) => {
                // Only what the push carried is acknowledged: a deletion made
                // while it was in flight, a note added after it and a newer
                // position stay, and the book stays on the pending list.
                let ack = submitted
                    .as_ref()
                    .map(|rec| self.lib.acknowledge_push(&book, rec));
                let pending = ack.is_some_and(|a| a.pending);
                if let Some(r) = self.net.accounts.readest.as_mut() {
                    let before = r.pending_queue.clone();
                    if pending {
                        r.enqueue_push(&book);
                    } else {
                        r.dequeue_push(&book);
                    }
                    queue_changed = r.pending_queue != before;
                }
                if queue_changed {
                    self.net.save();
                }
                if ack.is_some_and(|a| a.acknowledged) {
                    self.saver.save(&self.lib);
                }
                if let Some(p) = self
                    .lib
                    .book(&book)
                    .and_then(|rec| rec.progress.as_ref())
                    .or_else(|| submitted.as_ref().and_then(|rec| rec.progress.as_ref()))
                    .cloned()
                {
                    self.update_cloud_book_from_local_progress(&book, &p);
                }
                if job.is_some() {
                    self.readest.manual_message = Some(tr(Msg::ReaderSyncPushDone).to_string());
                }
            }
            Err(kind) => {
                perf!("readest: push error: {kind:?}");
                let message = readest_error_message(kind);
                self.readest.message = Some(message.clone());
                // Failed pushes are retried on the next open, sync or wake,
                // unless the reader turned the automatic push off.
                if let Some(r) = self.net.accounts.readest.as_mut() {
                    let before = r.pending_queue.clone();
                    r.enqueue_push(&book);
                    queue_changed = r.pending_queue != before;
                }
                if queue_changed {
                    self.net.save();
                }
                if job.is_some() {
                    self.readest.manual_message = Some(tf!(Msg::ReaderSyncFailed, error = message));
                }
            }
        }
        self.readest.pending_count = self
            .net
            .accounts
            .readest
            .as_ref()
            .map(|r| r.pending_pushes())
            .unwrap_or(0);
        if job.is_some() {
            self.refresh_reader_sync_status()?;
        }
        Ok(())
    }

    /// Trigger full two-way synchronization of all local books, configs, and notes.
    pub(crate) fn readest_sync_all(&mut self) {
        if !self.prefs.enable_readest {
            return;
        }
        if self.net.accounts.readest.is_none() {
            self.readest.message = Some(tr(Msg::ReadestAccountNotLoggedIn).to_string());
            return;
        }
        // Two taps on "Sync now" must not start two rounds: they would share
        // one refresh token (the app does not store the access token, so each
        // round refreshes) and send every book, config and note twice.
        if self.readest.is_syncing {
            perf!("readest: a full sync is already running");
            return;
        }
        self.readest.is_syncing = true;
        self.readest.message = Some(tr(Msg::ReadestSyncing).to_string());
        self.net.reconcile_allow_entries();
        let Some(settings) = self.net.accounts.readest.as_ref() else {
            return;
        };

        let policy = Net::policy();
        let sync_config = self.net.accounts.readest_config();
        let store = self.net.readest_session.clone();
        let epoch = store.epoch();
        // One cursor per record type (contract §1.2): the server filters books
        // by `synced_at` but configs and notes by `updated_at`/`deleted_at`.
        let since_books = settings.cursor(aristotle_net::store::CursorKind::Books);
        let since_configs = settings.cursor(aristotle_net::store::CursorKind::Configs);
        let since_notes = settings.cursor(aristotle_net::store::CursorKind::Notes);

        // Snapshot only non-WeRead books so WeRead books stay strictly isolated
        // from Readest cloud sync.
        let records: Vec<BookRecord> = self
            .lib
            .index()
            .books
            .iter()
            .filter(|rec| !self.is_weread_book(&rec.id))
            .cloned()
            .collect();
        // The same snapshot goes back with the answer: it is what the server
        // received, and only it may be acknowledged (SYNC-111).

        self.net.spawn("readest-sync-all", move |_| {
            let mut local_configs = Vec::new();
            let mut local_notes = Vec::new();
            let mut txt_hashes = HashSet::new();
            for rec in &records {
                let is_txt = matches!(rec.format, aristotle_library::Format::Txt);
                if is_txt {
                    txt_hashes.insert(rec.id.clone());
                }
                if !is_txt {
                    if let Some(p) = &rec.progress {
                        let pages = if p.fraction > 0.0 {
                            let cur =
                                ((p.fraction.clamp(0.0, 1.0) * 100.0).round() as u32).clamp(1, 100);
                            Some((cur, 100))
                        } else {
                            None
                        };
                        local_configs.push(progress_to_db_book_config(&rec.id, p, pages));
                    }
                    local_notes.extend(
                        rec.annotations
                            .iter()
                            .map(|a| annotation_to_db_book_note(&rec.id, a)),
                    );
                    local_notes.extend(rec.annotation_tombstones.iter().map(|t| {
                        tombstone_to_db_book_note(
                            &rec.id,
                            &LocalTombstone {
                                id: t.id.clone(),
                                deleted_at: t.deleted_at,
                            },
                        )
                    }));
                }
            }

            let submitted = records;
            let transport = ReadestTransport::with_timeout(policy, 30);
            // The session is the account's, shared with the other requests:
            // the store keeps the live access token, the settings file only
            // the refresh token.
            let client = SyncClient::with_session_store(transport, sync_config, store);

            // Fast path: when all three cursors match (e.g. initial sync), a
            // single untyped `GET /api/sync?since=<s>` (with no `limit`, so
            // the server loops internally and returns complete results per
            // contract §1.1) fetches books, configs and notes concurrently on
            // the server in 1 RTT over a single warm Keep-Alive socket.
            // When cursors differ, refresh the token once and fan out the 3
            // independent typed GETs in parallel.
            let pull_res: Result<
                (
                    Vec<inkread_core::sync::DbBook>,
                    Vec<DbBookConfig>,
                    Vec<DbBookNote>,
                    PulledCursors,
                ),
                (SyncStage, inkread_core::sync::SyncError),
            > = if since_books == since_configs && since_configs == since_notes {
                match client.pull_internal(None, since_books, None, None) {
                    Ok(resp) => {
                        let cursors = PulledCursors {
                            books: resp.books_cursor(),
                            configs: resp.configs_cursor(),
                            notes: resp.notes_cursor(),
                        };
                        Ok((resp.books, resp.configs, resp.notes, cursors))
                    }
                    Err(e) => Err((SyncStage::Library, e)),
                }
            } else if let Err(e) = client.ensure_token() {
                Err((SyncStage::Library, e))
            } else {
                std::thread::scope(|scope| {
                    let books_h =
                        scope.spawn(|| client.pull_all_books_with_cursor(since_books, 100));
                    let configs_h = scope
                        .spawn(|| client.pull_internal(Some("configs"), since_configs, None, None));
                    let notes_h = scope
                        .spawn(|| client.pull_internal(Some("notes"), since_notes, None, None));

                    let books_res = books_h.join().unwrap_or_else(|_| {
                        Err(inkread_core::sync::SyncError::Transport(
                            "books worker panicked".into(),
                        ))
                    });
                    let configs_res = configs_h.join().unwrap_or_else(|_| {
                        Err(inkread_core::sync::SyncError::Transport(
                            "configs worker panicked".into(),
                        ))
                    });
                    let notes_res = notes_h.join().unwrap_or_else(|_| {
                        Err(inkread_core::sync::SyncError::Transport(
                            "notes worker panicked".into(),
                        ))
                    });

                    let (pulled_books, books_cursor) =
                        books_res.map_err(|e| (SyncStage::Library, e))?;
                    let configs = configs_res.map_err(|e| (SyncStage::Progress, e))?;
                    let notes = notes_res.map_err(|e| (SyncStage::Notes, e))?;
                    let cursors = PulledCursors {
                        books: books_cursor,
                        configs: configs.configs_cursor(),
                        notes: notes.notes_cursor(),
                    };
                    Ok((pulled_books, configs.configs, notes.notes, cursors))
                })
            };

            let (pulled_books, pulled_configs, pulled_notes, cursors) = match pull_res {
                Ok(tuple) => tuple,
                Err((stage, e)) => {
                    eprintln!(
                        "aristotle: readest: full sync failed at {stage:?}: {}",
                        redacted_sync_error(&e)
                    );
                    return Done::ReadestSyncAll {
                        store: epoch,
                        submitted: Vec::new(),
                        now_ms: client.now_ms(),
                        pulled_configs: Vec::new(),
                        pulled_notes: Vec::new(),
                        stage: Some(stage),
                        cursors: PulledCursors::default(),
                        result: Err(e.kind()),
                    };
                }
            };
            let pull_count = pulled_books.len() + pulled_configs.len() + pulled_notes.len();

            // Skip POST /api/sync when there are no local configs or notes to
            // upload, saving a full round-trip on pull-only syncs.
            let push_count = if local_configs.is_empty() && local_notes.is_empty() {
                0
            } else {
                let push_req = SyncPushRequest {
                    books: Vec::new(),
                    configs: local_configs,
                    notes: local_notes,
                    stat_books: Vec::new(),
                    stat_pages: Vec::new(),
                };

                let stage = SyncStage::Push;
                match client.push(push_req, Some(&txt_hashes)) {
                    Ok(p) => p.books.len() + p.configs.len() + p.notes.len(),
                    Err(e) => {
                        eprintln!(
                            "aristotle: readest: full sync failed at {stage:?}: {}",
                            redacted_sync_error(&e)
                        );
                        return Done::ReadestSyncAll {
                            store: epoch,
                            submitted: Vec::new(),
                            now_ms: client.now_ms(),
                            pulled_configs,
                            pulled_notes,
                            stage: Some(stage),
                            cursors: PulledCursors::default(),
                            result: Err(e.kind()),
                        };
                    }
                }
            };

            Done::ReadestSyncAll {
                store: epoch,
                submitted,
                now_ms: client.now_ms(),
                pulled_configs,
                pulled_notes,
                stage: None,
                cursors,
                result: Ok((pull_count, push_count)),
            }
        });
    }

    /// Handles arrival of full synchronization outcome.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_readest_sync_all(
        &mut self,
        store: u64,
        submitted: Vec<BookRecord>,
        now_ms: i64,
        pulled_configs: Vec<DbBookConfig>,
        pulled_notes: Vec<DbBookNote>,
        stage: Option<SyncStage>,
        cursors: PulledCursors,
        result: Result<(usize, usize), inkread_core::sync::SyncErrorKind>,
    ) -> Result<(), Error> {
        if !self.net.is_current_store(store) {
            return Ok(());
        }
        self.readest.is_syncing = false;
        // The store holds the account's session, and it is the only place a
        // new token is read from: a worker that finished with an older copy
        // cannot write it over the newer one. The refresh token rotates, so
        // a round that failed after the refresh must not lose it either.
        self.net.persist_session();
        match result {
            Ok((_pull_n, _push_n)) => {
                // Only what the round carried is acknowledged. This runs
                // before the pulled notes are merged: those notes are on the
                // server already, a change of the reader's own is not.
                let mut fully_sent: Vec<String> = Vec::new();
                let mut still_pending: Vec<String> = Vec::new();
                for rec in &submitted {
                    let ack = self.lib.acknowledge_push(&rec.id, rec);
                    if ack.pending {
                        still_pending.push(rec.id.clone());
                    } else {
                        fully_sent.push(rec.id.clone());
                    }
                }
                if let Some(r) = self.net.accounts.readest.as_mut() {
                    r.last_sync_time = Some(now_ms);
                    // The books that went up whole leave the pending list; the
                    // ones that changed while the round ran keep theirs.
                    r.pending_queue.retain(|book| !fully_sent.contains(book));
                    for book in &still_pending {
                        r.enqueue_push(book);
                    }
                    // The cursors are the server's own timestamps; a type that
                    // pulled nothing keeps the cursor it had.
                    r.advance_cursor(aristotle_net::store::CursorKind::Books, cursors.books);
                    r.advance_cursor(aristotle_net::store::CursorKind::Configs, cursors.configs);
                    r.advance_cursor(aristotle_net::store::CursorKind::Notes, cursors.notes);
                }
                self.net.save();
                self.readest.last_sync_time = Some(format_iso8601(now_ms));
                self.readest.message = Some(tr(Msg::ReadestSyncSuccess).to_string());

                // Update cloud shelf progress and seed unread local copies from
                // pulled configs without silently overwriting active local reading
                // positions (contract §5.2).
                for cfg in pulled_configs {
                    self.update_cloud_book_from_config(&cfg);
                    self.seed_unread_local_book_from_config(&cfg);
                }

                // Group pulled notes by book_hash and merge
                let mut notes_by_book: std::collections::HashMap<String, Vec<DbBookNote>> =
                    std::collections::HashMap::new();
                for note in pulled_notes {
                    notes_by_book
                        .entry(note.book_hash.clone())
                        .or_default()
                        .push(note);
                }
                for (book_hash, rem_notes) in notes_by_book {
                    if let Some(rec) = self.lib.book_mut(&book_hash) {
                        merge_remote_notes(rec, &rem_notes);
                    }
                }
                // The tombstones that went up were forgotten above; the ones
                // that were made since stay.
                self.saver.save(&self.lib);
            }
            Err(kind) => {
                // Say which step stopped, with the localized error only: the
                // server's text and any address details stay off the screen.
                let message = readest_error_message(kind);
                let text = match stage {
                    Some(stage) => tf!(
                        Msg::ReadestSyncFailedStage,
                        error = message,
                        stage = stage.label()
                    ),
                    None => message,
                };
                perf!(
                    "readest: full sync failed at {:?}: {kind:?}",
                    stage.unwrap_or(SyncStage::Push)
                );
                self.readest.message = Some(text);
            }
        }
        if matches!(self.screen, Screen::ReadestStatus) {
            self.show_readest_status(Refresh::Auto)?;
        } else if matches!(self.screen, Screen::Mine) {
            self.show_mine(Refresh::Auto)?;
        }
        Ok(())
    }

    /// Pull the open book's cloud progress and highlights, by hand (`SYNC-111`).
    ///
    /// Only this book, only a pull: it never pushes what is local and never
    /// stamps a reading position by itself. A remote position is offered to the
    /// reader, as the automatic pull on open does (contract §5.2).
    pub(crate) fn readest_pull_book_now(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &self.screen else {
            return Ok(());
        };
        let book = r.session.id.clone();
        if self.is_weread_book(&book) {
            self.readest.manual_message = Some(tr(Msg::ReaderSyncWereadOnly).to_string());
            return self.refresh_reader_sync_status();
        }
        if self.net.accounts.readest.is_none() {
            self.readest.manual_message = Some(tr(Msg::ReadestAccountNotLoggedIn).to_string());
            return self.refresh_reader_sync_status();
        }
        // Contract §8: TXT positions and notes stay local.
        let is_txt = self
            .lib
            .book(&book)
            .map(|b| matches!(b.format, aristotle_library::Format::Txt))
            .unwrap_or(false);
        if is_txt {
            self.readest.manual_message = Some(tr(Msg::ReaderSyncTxt).to_string());
            return self.refresh_reader_sync_status();
        }
        let Some(job) = self.readest.jobs.begin(JobKind::Pull, &book) else {
            // A pull of this book is already running: one is enough.
            return Ok(());
        };
        self.net.reconcile_allow_entries();
        self.readest.manual_message = Some(tr(Msg::ReaderSyncPullRunning).to_string());

        let policy = Net::policy();
        let config = self.net.accounts.readest_config();
        let store = self.net.readest_session.clone();
        let epoch = store.epoch();
        self.net.spawn("readest-pull-book", move |_| {
            let client =
                SyncClient::with_session_store(ReadestTransport::new(policy), config, store);
            // Single untyped GET fetches both `configs` and `notes` for this
            // book in parallel on the server (contract §1.1), cutting manual
            // pull latency in half compared to two sequential GETs. `limit=1`
            // bounds the ignored `books` slice while configs/notes remain full.
            let pulled = client
                .pull_internal(None, 0, Some(1), Some(&book))
                .map(|resp| (resp.configs, resp.notes));
            let (pulled, result) = match pulled {
                Ok(pair) => (Some(pair), Ok(())),
                Err(e) => {
                    eprintln!(
                        "aristotle: readest: manual pull failed: {}",
                        redacted_sync_error(&e)
                    );
                    (None, Err(e.kind()))
                }
            };
            Done::ReadestBookSync {
                job,
                store: epoch,
                pulled,
                result,
            }
        });
        self.refresh_reader_sync_status()
    }

    /// Push the open book's progress, highlights and tombstones, by hand.
    ///
    /// This is the manual path: it works with the automatic push switched off,
    /// and it sends only this book.
    pub(crate) fn readest_push_book_now(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &self.screen else {
            return Ok(());
        };
        let book = r.session.id.clone();
        if self.is_weread_book(&book) {
            self.readest.manual_message = Some(tr(Msg::ReaderSyncWereadOnly).to_string());
            return self.refresh_reader_sync_status();
        }
        if self.net.accounts.readest.is_none() {
            self.readest.manual_message = Some(tr(Msg::ReadestAccountNotLoggedIn).to_string());
            return self.refresh_reader_sync_status();
        }
        let is_txt = self
            .lib
            .book(&book)
            .map(|b| matches!(b.format, aristotle_library::Format::Txt))
            .unwrap_or(false);
        if is_txt {
            self.readest.manual_message = Some(tr(Msg::ReaderSyncTxt).to_string());
            return self.refresh_reader_sync_status();
        }
        let Some(job) = self.readest.jobs.begin(JobKind::Push, &book) else {
            return Ok(());
        };
        // A manual push sends what the reader has on screen now: the current
        // page is part of the payload, so it must be the one just read.
        if let Screen::Reader(r) = &self.screen {
            let now = aristotle_library::library::now();
            self.lib
                .set_progress(&r.session.id, r.session.progress(now));
        }
        self.net.reconcile_allow_entries();
        self.readest.manual_message = Some(tr(Msg::ReaderSyncPushRunning).to_string());
        self.readest_push_book(book, false, Some(job));
        self.refresh_reader_sync_status()
    }

    /// Handles the completion of a manual pull of the open book.
    pub(crate) fn on_readest_book_sync(
        &mut self,
        job: BookJob,
        store: u64,
        pulled: Option<(Vec<DbBookConfig>, Vec<DbBookNote>)>,
        result: Result<(), inkread_core::sync::SyncErrorKind>,
    ) -> Result<(), Error> {
        if !self.net.is_current_store(store) {
            return Ok(());
        }
        self.net.persist_session();
        // The token says whether this result still belongs to the book on
        // screen: a switch mid-request makes it stale.
        if !self.readest.jobs.finish(&job) {
            perf!(
                "readest: dropped a stale {:?} result for {}",
                job.kind,
                job.book
            );
            return Ok(());
        }
        let open = matches!(&self.screen, Screen::Reader(r) if r.session.id == job.book);
        if !open {
            self.readest.jobs.forget_book(&job.book);
            return self.refresh_reader_sync_status();
        }
        match result {
            Ok(()) => {
                if let Some((configs, notes)) = pulled {
                    perf!(
                        "readest: manual pull of {}: {} configs, {} notes",
                        job.book,
                        configs.len(),
                        notes.len()
                    );
                    self.apply_readest_pull(
                        &job.book,
                        SyncPullResponse {
                            configs,
                            notes,
                            ..Default::default()
                        },
                        true,
                    )?;
                }
                let offered = self.readest.offer.is_some()
                    || matches!(&self.screen, Screen::Reader(r) if matches!(r.overlay, Overlay::Sync { .. }));
                self.readest.manual_message = Some(if offered {
                    tr(Msg::ReaderSyncPullDone).to_string()
                } else {
                    tr(Msg::ReaderSyncSame).to_string()
                });
            }
            Err(kind) => {
                perf!("readest: manual pull failed: {kind:?}");
                self.readest.manual_message = Some(tf!(
                    Msg::ReaderSyncFailed,
                    error = readest_error_message(kind)
                ));
            }
        }
        self.refresh_reader_sync_status()
    }

    /// Offer reading position jump popup dialog in reader.
    pub(crate) fn show_readest_sync_offer(&mut self) -> Result<(), Error> {
        if self.asleep.is_some() {
            return Ok(());
        }
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let Some((book, _, _)) = &self.readest.offer else {
            return Ok(());
        };
        if *book != r.session.id {
            self.readest.offer = None;
            return Ok(());
        }
        if r.menu_open || !r.overlay.is_none() || r.gestures.holding() {
            return Ok(());
        }
        let Some((_, target, text)) = self.readest.offer.take() else {
            return Ok(());
        };
        r.overlay = Overlay::Sync { target, text };
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// Event handler for Readest login screen.
    pub(crate) fn on_readest_login_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let Screen::ReadestLogin(state) = &mut self.screen else {
            return Ok(Flow::Continue);
        };
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        match state.handle_action(id) {
            ReadestLoginOutcome::Cancel => {
                self.show_mine(Refresh::Full)?;
            }
            ReadestLoginOutcome::OpenSettings => {
                self.show_readest_status(Refresh::Full)?;
            }
            ReadestLoginOutcome::Submit { email, password } => {
                let policy = Net::policy();
                let config = login_config(&self.net.accounts);
                self.net.spawn("readest-login", move |_| {
                    let transport = ReadestTransport::new(policy);
                    let client = SyncClient::new(transport, config);
                    let res = client.login(&email, &password).map_err(|e| e.kind());
                    Done::ReadestLogin { result: res }
                });
                let facts = Facts::status(&self.battery);
                let status = facts.status_line();
                let mut tree = Tree::new();
                let root = ui_readest_login::build_readest_login_screen(
                    &mut tree,
                    &self.styles,
                    state,
                    &status,
                );
                tree.set_root(root);
                self.ui.set_tree_with(tree, Refresh::Auto);
                self.ui.layout();
                self.render_ui()?;
            }
            ReadestLoginOutcome::Changed => {
                let facts = Facts::status(&self.battery);
                let status = facts.status_line();
                let mut tree = Tree::new();
                let root = ui_readest_login::build_readest_login_screen(
                    &mut tree,
                    &self.styles,
                    state,
                    &status,
                );
                tree.set_root(root);
                self.ui.set_tree_with(tree, Refresh::Auto);
                self.ui.layout();
                self.render_ui()?;
            }
            ReadestLoginOutcome::None => {}
        }
        Ok(Flow::Continue)
    }

    /// Handles completion of login request worker.
    pub(crate) fn on_readest_login(
        &mut self,
        result: Result<Session, inkread_core::sync::SyncErrorKind>,
    ) -> Result<(), Error> {
        match result {
            Ok(session) => {
                let before = self.net.accounts.allow_entries();
                let email = if let Screen::ReadestLogin(st) = &self.screen {
                    st.email.clone()
                } else {
                    String::new()
                };
                let old = self.net.accounts.readest.clone();
                self.net.accounts.readest =
                    Some(ReadestSettings::after_login(&email, &session, old.as_ref()));
                // The requests of this account share one session: the one the
                // sign-in produced. A new store for the new account: the
                // results of the requests of the old one must not land here.
                self.net.readest_session =
                    inkread_core::sync::SessionStore::with_session(Some(session.clone()));
                self.cloud = crate::cloud::CloudState::default();
                self.readest.pending_pull = None;
                self.net.save();
                self.net.update_allowlist(&before);
                // The account may carry a custom endpoint the file has never
                // seen; the first sync must not be refused by the firewall.
                self.net.reconcile_allow_entries();
                self.readest.message = None;
                self.show_mine(Refresh::Full)?;
            }
            Err(kind) => {
                if let Screen::ReadestLogin(st) = &mut self.screen {
                    st.is_logging_in = false;
                    st.error_message = Some(readest_error_message(kind));
                    let facts = Facts::status(&self.battery);
                    let status = facts.status_line();
                    let mut tree = Tree::new();
                    let root = ui_readest_login::build_readest_login_screen(
                        &mut tree,
                        &self.styles,
                        st,
                        &status,
                    );
                    tree.set_root(root);
                    self.ui.set_tree_with(tree, Refresh::Auto);
                    self.ui.layout();
                    self.render_ui()?;
                }
            }
        }
        Ok(())
    }

    /// Whether Readest owns progress for this book.
    ///
    /// When both sync services are on, Readest wins for reflowable books: it
    /// has the richer positions. TXT keeps using KOSync, because Readest
    /// does not sync TXT positions at all (contract §8).
    pub(crate) fn readest_priority(&self, book_id: &str) -> bool {
        self.net.accounts.readest.is_some()
            && self
                .lib
                .book(book_id)
                .map(|b| !matches!(b.format, aristotle_library::Format::Txt))
                .unwrap_or(false)
    }

    /// Open the editor for one Readest endpoint.
    pub(crate) fn show_readest_edit(
        &mut self,
        field: crate::net::TextField,
        refresh: Refresh,
    ) -> Result<(), Error> {
        use crate::net::TextField as F;
        // Unset endpoints prefill the official values, so the editor starts
        // from what the screen shows instead of an empty line.
        let config = self.net.accounts.readest_config();
        let endpoints = &self.net.accounts.readest_endpoints;
        let (title, initial, secret) = match field {
            F::ReadestApiBase => (
                tr(Msg::ReadestApiBase).to_string(),
                endpoints.api_base.clone().unwrap_or(config.api_base),
                false,
            ),
            F::ReadestSupabaseUrl => (
                tr(Msg::ReadestSupabaseUrl).to_string(),
                endpoints
                    .supabase_url
                    .clone()
                    .unwrap_or(config.supabase_url),
                false,
            ),
            F::ReadestAnonKey => (
                tr(Msg::ReadestSupabaseAnonKey).to_string(),
                endpoints
                    .supabase_anon_key
                    .clone()
                    .unwrap_or(config.supabase_anon_key),
                true,
            ),
            _ => return Ok(()),
        };
        let state = ui_readest_edit::ReadestEditState::new(title, &initial, secret);
        self.render_readest_edit(ReadestEditScreen { field, state }, refresh)
    }

    fn render_readest_edit(
        &mut self,
        edit: ReadestEditScreen,
        refresh: Refresh,
    ) -> Result<(), Error> {
        let facts = Facts::status(&self.battery);
        let status = facts.status_line();
        let mut tree = Tree::new();
        let root = ui_readest_edit::build_readest_edit_screen(
            &mut tree,
            &self.styles,
            &edit.state,
            &status,
        );
        tree.set_root(root);
        self.ui.set_tree_with(tree, refresh);
        self.ui.layout();
        self.screen = Screen::ReadestEdit(Box::new(edit));
        self.render_ui()
    }

    /// Event handler for the endpoint editor.
    pub(crate) fn on_readest_edit_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        let mut edit = match std::mem::replace(&mut self.screen, Screen::Mine) {
            Screen::ReadestEdit(e) => *e,
            other => {
                self.screen = other;
                return Ok(Flow::Continue);
            }
        };
        match edit.state.handle_action(id) {
            ReadestEditOutcome::Cancel => self.show_readest_status(Refresh::Full)?,
            ReadestEditOutcome::Save(value) => {
                self.on_readest_endpoint_text(edit.field, &value)?;
            }
            ReadestEditOutcome::Changed => {
                self.render_readest_edit(edit, Refresh::Auto)?;
            }
            ReadestEditOutcome::None => {
                self.screen = Screen::ReadestEdit(Box::new(edit));
            }
        }
        Ok(Flow::Continue)
    }

    /// Store typed endpoint text. Empty restores the default.
    pub(crate) fn on_readest_endpoint_text(
        &mut self,
        text_field: crate::net::TextField,
        value: &str,
    ) -> Result<(), Error> {
        use crate::net::TextField as F;
        let (field, title, secret) = match text_field {
            F::ReadestApiBase => (
                ReadestEndpointField::ApiBase,
                tr(Msg::ReadestApiBase).to_string(),
                false,
            ),
            F::ReadestSupabaseUrl => (
                ReadestEndpointField::SupabaseUrl,
                tr(Msg::ReadestSupabaseUrl).to_string(),
                false,
            ),
            F::ReadestAnonKey => (
                ReadestEndpointField::AnonKey,
                tr(Msg::ReadestSupabaseAnonKey).to_string(),
                true,
            ),
            _ => return Ok(()),
        };
        let before = self.net.accounts.allow_entries();
        if self
            .net
            .accounts
            .set_readest_endpoint(field, value)
            .is_err()
        {
            let mut state = ui_readest_edit::ReadestEditState::new(title, value.trim(), secret);
            state.error = Some(tr(Msg::ReadestErrInvalidUrl).to_string());
            return self.render_readest_edit(
                ReadestEditScreen {
                    field: text_field,
                    state,
                },
                Refresh::Auto,
            );
        }
        self.net.save();
        self.net.update_allowlist(&before);
        self.show_readest_status(Refresh::Full)
    }

    /// Event handler for Readest status screen.
    pub(crate) fn on_readest_status_event(&mut self, ev: UiEvent) -> Result<Flow, Error> {
        use ui_readest_status::action as a;
        let UiEvent::Action { id, .. } = ev else {
            return Ok(Flow::Continue);
        };
        match id {
            a::BACK => {
                self.show_mine(Refresh::Full)?;
            }
            a::SYNC_NOW => {
                self.readest_sync_all();
                self.show_readest_status(Refresh::Auto)?;
            }
            a::LOGOUT => {
                let before = self.net.accounts.allow_entries();
                self.net.accounts.readest = None;
                // The shared session goes with the account: a request in
                // flight must not keep using it, and its result must not be
                // taken for the account that signs in next.
                self.net.readest_session = inkread_core::sync::SessionStore::new();
                self.cloud = crate::cloud::CloudState::default();
                self.readest.pending_pull = None;
                self.net.save();
                self.net.update_allowlist(&before);
                // Results of requests already in flight must not land in the
                // next account.
                self.readest.opened_book = None;
                self.readest.next_token = self.readest.next_token.wrapping_add(1);
                self.readest.jobs = BookJobs::new();
                self.readest.manual_message = None;
                self.readest.is_syncing = false;
                self.show_mine(Refresh::Full)?;
            }
            a::AUTO_PUSH => {
                if let Some(r) = self.net.accounts.readest.as_mut() {
                    let on = !r.auto_push_enabled();
                    r.set_auto_push(on);
                }
                self.net.save();
                self.show_readest_status(Refresh::Auto)?;
            }
            a::SIGN_IN => {
                self.show_readest_login(Refresh::Full)?;
            }
            a::EDIT_API_BASE => {
                self.show_readest_edit(crate::net::TextField::ReadestApiBase, Refresh::Full)?;
            }
            a::EDIT_SUPABASE_URL => {
                self.show_readest_edit(crate::net::TextField::ReadestSupabaseUrl, Refresh::Full)?;
            }
            a::EDIT_ANON_KEY => {
                self.show_readest_edit(crate::net::TextField::ReadestAnonKey, Refresh::Full)?;
            }
            a::RESTORE_DEFAULT => {
                let before = self.net.accounts.allow_entries();
                self.net.accounts.reset_readest_endpoints();
                self.net.save();
                self.net.update_allowlist(&before);
                self.show_readest_status(Refresh::Full)?;
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }
}

/// Redacted summary of a `SyncError` for stderr diagnostics: includes the
/// error kind (plus HTTP status code or brief transport/limit class), while
/// never logging response bodies, tokens, URLs, or emails.
pub(crate) fn redacted_sync_error(e: &inkread_core::sync::SyncError) -> String {
    use inkread_core::sync::SyncError as E;
    match e {
        E::HttpStatus(code, _) => format!("{:?}(HTTP {code})", e.kind()),
        E::TooLarge(limit) => format!("{:?}({limit}B)", e.kind()),
        E::Transport(detail) => {
            let brief = detail.split('\n').next().unwrap_or("");
            let brief: String = brief.chars().take(80).collect();
            format!("{:?}({brief})", e.kind())
        }
        _ => format!("{:?}", e.kind()),
    }
}

/// The message for a synchronization failure, without any server text.
fn readest_error_message(kind: inkread_core::sync::SyncErrorKind) -> String {
    use inkread_core::sync::SyncErrorKind as K;
    match kind {
        K::AuthFailed => tr(Msg::ReadestErrLoginFailed).to_string(),
        K::AuthExpired => tr(Msg::ReadestErrAuthExpired).to_string(),
        K::Network => tr(Msg::ReadestErrNetwork).to_string(),
        K::Server | K::Invalid => tr(Msg::ReadestErrServer).to_string(),
        K::Blocked => tr(Msg::ReadestErrBlocked).to_string(),
        K::TooLarge => tr(Msg::ReadestErrTooLarge).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation() -> Annotation {
        Annotation {
            id: "note-1".to_string(),
            kind: AnnotationKind::Highlight,
            text: Some("Test highlight text".to_string()),
            page: Some("12".to_string()),
            location: Some((10, 11)),
            position: Some((
                "/body/DocFragment[3]/body/p[2]/text().5".to_string(),
                "/body/DocFragment[3]/body/p[2]/text().25".to_string(),
            )),
            created: Some("2026-11-09 12:00:00".to_string()),
            modified: Some("2026-11-09 12:05:00".to_string()),
            color: Some("yellow".to_string()),
            style: Some("highlight".to_string()),
            source: "aristotle".to_string(),
            chapter: Some("Chapter 1".to_string()),
            note: Some("My thoughtful note".to_string()),
        }
    }

    #[test]
    fn annotation_mapping_bidirectional_roundtrip() {
        let original = annotation();

        let db_note = annotation_to_db_book_note("hash-1234", &original);
        assert_eq!(db_note.book_hash, "hash-1234");
        assert_eq!(db_note.id, "note-1");
        assert_eq!(db_note.text.as_deref(), Some("Test highlight text"));
        assert_eq!(db_note.note.as_deref(), Some("My thoughtful note"));

        let restored = LocalNote::from_db_book_note(&db_note)
            .map(annotation_from_local_note)
            .expect("active note must map");
        assert_eq!(restored.kind, AnnotationKind::Highlight);
        assert_eq!(restored.text, original.text);
        assert_eq!(restored.note, original.note);
        assert_eq!(restored.position, original.position);
    }

    #[test]
    fn mapping_keeps_local_time_and_local_only_fields() {
        let original = annotation();
        let note = local_note_from_annotation(&original);
        let back = annotation_from_local_note(note);
        assert_eq!(back.created.as_deref(), Some("2026-11-09 12:00:00"));
        assert_eq!(back.modified.as_deref(), Some("2026-11-09 12:05:00"));
        assert_eq!(back.page, original.page);
        assert_eq!(back.location, original.location);
        assert_eq!(back.chapter, original.chapter);
        assert_eq!(back.source, "aristotle");
    }

    #[test]
    fn merge_keeps_non_readest_annotations() {
        let mut rec = BookRecord {
            id: "book-1".to_string(),
            file: aristotle_library::index::FileStamp {
                path: "book.epub".to_string(),
                size: 1,
                mtime: 1,
                mtime_ns: 0,
            },
            format: aristotle_library::Format::Epub,
            meta: Default::default(),
            cover: None,
            added: 0,
            last_opened: None,
            status: Default::default(),
            progress: None,
            settings: Default::default(),
            annotations: vec![annotation()],
            annotation_tombstones: Vec::new(),
        };
        let cloud = DbBookNote {
            book_hash: "book-1".to_string(),
            id: "note-1".to_string(),
            kind: "annotation".to_string(),
            text: Some("changed by the phone".to_string()),
            updated_at: Some(format_iso8601(1_900_000_000_000)),
            ..Default::default()
        };
        merge_remote_notes(&mut rec, &[cloud]);
        assert_eq!(rec.annotations.len(), 1);
        assert_eq!(rec.annotations[0].page.as_deref(), Some("12"));
        assert_eq!(rec.annotations[0].chapter.as_deref(), Some("Chapter 1"));
        assert_eq!(rec.annotations[0].source, "aristotle");
    }

    #[test]
    fn progress_to_db_book_config_conversion() {
        let progress = Progress {
            fraction: 0.42,
            locator: aristotle_library::Locator::XPointer {
                xpointer: "/body/DocFragment[4]/body/p[1]/text().0".to_string(),
            },
            updated: 1700000000,
            source: Some("aristotle".to_string()),
            page: None,
        };

        let config = progress_to_db_book_config("book-xyz", &progress, Some((42, 100)));
        assert_eq!(config.book_hash, "book-xyz");
        assert_eq!(config.progress.as_deref(), Some("[42, 100]"));
        assert_eq!(
            config.xpointer.as_deref(),
            Some("/body/DocFragment[4]/body/p[1]/text().0")
        );
        // Without a page pair the fraction is not sent at all.
        let no_pages = progress_to_db_book_config("book-xyz", &progress, None);
        assert_eq!(no_pages.progress, None);
    }
}
