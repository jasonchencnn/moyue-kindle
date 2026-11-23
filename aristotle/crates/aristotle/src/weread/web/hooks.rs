// SPDX-License-Identifier: AGPL-3.0-or-later
//! The app hooks of the Web channel: the second sign-in stage, the reading
//! session, the close/sleep write and the worker poll.
//!
//! Split from `web.rs` to keep every file under the repository's 1200-line
//! limit; the state and the protocol live in the parent module.

use std::time::Instant;

use aristotle_i18n::{tr, Msg};
use aristotle_render::plato_core::anyhow::Error;
use inkread_core::weread::WeReadCoordinate;

use super::login::ui_phase;

use crate::{App, Screen};

/// Open the Web sign-in at the second stage of the login screen.
impl App {
    pub(crate) fn start_weread_web_login(&mut self) -> Result<(), Error> {
        let generation = self.weread.begin_web_login();
        self.allow_weread_hosts();
        self.weread.web.ensure_loaded(&self.net.paths.state);
        self.weread.web.start_login(
            &self.net.paths.state,
            crate::net::Net::policy(),
            generation,
            Instant::now(),
        );
        self.weread.login.begin_web(generation);
        self.show_weread_login(aristotle_ui::tree::Refresh::Full)
    }

    /// The start of a reading session: read the cloud position and record the
    /// arbitration baseline. A non-WeRead book is ignored.
    pub(crate) fn weread_web_book_open(&mut self, hash: &str) {
        let credentials = self.weread.credentials();
        self.weread.web.begin_book(
            &self.net.paths.state,
            hash,
            credentials,
            crate::net::Net::policy(),
            Instant::now(),
        );
    }

    /// After a sleep: the session ended with the sleep write, so re-open it
    /// for the reader that is still on screen.
    pub(crate) fn weread_web_resume(&mut self) {
        let Screen::Reader(reader) = &self.screen else {
            return;
        };
        let hash = reader.session.id.clone();
        self.weread_web_book_open(&hash);
    }

    /// The end of a reading session: close the session and, when the verdict
    /// allows it, write the position once.
    pub(crate) fn weread_web_book_close(&mut self) {
        let Screen::Reader(reader) = &self.screen else {
            return;
        };
        let hash = reader.session.id.clone();
        let position = reader.session.position();
        let fraction = reader
            .session
            .progress(aristotle_library::library::now())
            .fraction;
        let xpointer = match position {
            aristotle_render::book::Position::XPointer { xpointer } => Some(xpointer),
            _ => None,
        };
        if let Some(book_id) = self.weread.downloads.book_id_of_hash(&hash) {
            let pct = (fraction.clamp(0.0, 1.0) * 100.0).floor() as i64;
            if pct > 0 {
                self.weread.set_book_progress(&book_id, pct);
            }
        }
        self.weread.web.close_book(
            &self.net.paths.state,
            crate::net::Net::policy(),
            xpointer.as_deref(),
            fraction,
        );
    }

    /// The sleep/exit variant of [`App::weread_web_book_close`]: the one write
    /// finishes before the network is stopped.
    pub(crate) fn weread_web_book_close_blocking(&mut self) {
        let Screen::Reader(reader) = &self.screen else {
            return;
        };
        let hash = reader.session.id.clone();
        let position = reader.session.position();
        let fraction = reader
            .session
            .progress(aristotle_library::library::now())
            .fraction;
        let xpointer = match position {
            aristotle_render::book::Position::XPointer { xpointer } => Some(xpointer),
            _ => None,
        };
        if let Some(book_id) = self.weread.downloads.book_id_of_hash(&hash) {
            let pct = (fraction.clamp(0.0, 1.0) * 100.0).floor() as i64;
            if pct > 0 {
                self.weread.set_book_progress(&book_id, pct);
            }
        }
        self.weread.web.close_book_blocking(
            &self.net.paths.state,
            crate::net::Net::policy(),
            xpointer.as_deref(),
            fraction,
        );
    }

    /// Drain the Web workers; called from the main loop between events.
    pub(crate) fn poll_weread_web(&mut self) -> Result<(), Error> {
        let now = Instant::now();
        self.weread.web.ensure_loaded(&self.net.paths.state);
        let phase_before = self.weread.web.login_phase();
        self.weread.web.tick_login(&crate::net::Net::policy(), now);
        let report = self.weread.web.poll(now);
        let login_changed = report.login_changed || self.weread.web.login_phase() != phase_before;
        if let Some(credentials) = report.credentials {
            self.weread.set_credentials(credentials);
            self.weread.save();
        }
        if let Some((book_id, Some(progress))) = report.remote_progress {
            self.weread.set_book_progress(&book_id, progress);
        }
        if let Some(jump) = report.remote_jump {
            self.weread.web.set_pending_jump(jump);
        }
        if let Some(message) = report.message.as_ref() {
            self.weread.message = Some(message.clone());
            if matches!(self.screen, Screen::Reader(_)) {
                self.readest.manual_message = Some(message.clone());
                let _ = self.refresh_reader_sync_status();
            }
        } else if report.manual_no_change && matches!(self.screen, Screen::Reader(_)) {
            self.readest.manual_message = Some(tr(Msg::WereadPositionUnavailable).to_string());
            let _ = self.refresh_reader_sync_status();
        }
        self.apply_pending_weread_remote_jump()?;
        if matches!(self.screen, Screen::Reader(_)) {
            self.weread.web.start_heartbeat(crate::net::Net::policy());
        }
        if login_changed {
            self.sync_weread_web_login();
        }
        if report.login_confirmed {
            self.weread.finish_login();
            self.weread.message = None;
            if matches!(self.screen, Screen::WereadLogin) {
                self.show_weread_shelf(aristotle_ui::tree::Refresh::Full, 0)?;
            }
            self.refresh_weread_shelf();
        }
        Ok(())
    }

    /// Apply a pending cloud jump once the matching WeRead book is open and laid out.
    fn apply_pending_weread_remote_jump(&mut self) -> Result<(), Error> {
        let Some(jump) = self.weread.web.peek_pending_jump().cloned() else {
            return Ok(());
        };
        self.weread.downloads.ensure_loaded(&self.net.paths.state);
        let expected_hash = self.weread.downloads.hash_of(&jump.book_id);
        let matches_hash = |hash: &str| {
            expected_hash
                .as_deref()
                .is_some_and(|expected| expected.eq_ignore_ascii_case(hash))
        };
        let reader_ready = matches!(
            &self.screen,
            Screen::Reader(r) if matches_hash(&r.session.id) && r.session.has_book()
        );
        if !reader_ready {
            let still_opening = self.pending.as_ref().is_some_and(|p| matches_hash(&p.id))
                || self.opening.as_ref().is_some_and(|o| matches_hash(&o.id))
                || matches!(
                    &self.screen,
                    Screen::Reader(r) if matches_hash(&r.session.id) && !r.session.has_book()
                );
            if !still_opening {
                let _ = self.weread.web.take_pending_jump();
            }
            return Ok(());
        }

        let _ = self.weread.web.take_pending_jump();
        let positions = self
            .weread
            .downloads
            .positions_of(&jump.book_id)
            .or_else(|| {
                crate::weread::download::PositionStore::load_book(
                    &self.net.paths.state,
                    &jump.book_id,
                )
            });
        let coordinate = WeReadCoordinate {
            chapter_uid: jump.coordinate.chapter_uid,
            chapter_idx: jump.coordinate.chapter_idx,
            chapter_offset: jump.coordinate.chapter_offset,
        };
        let xpointer = positions
            .as_ref()
            .and_then(|pos| pos.map.xpointer_for(coordinate, Some(&pos.anchors)).ok())
            .map(|target| target.xpointer);
        let (current_loc, target_loc) = match &mut self.screen {
            Screen::Reader(r) => {
                let current = r.session.current;
                let target = xpointer.and_then(|xp| {
                    let pos = aristotle_render::book::Position::XPointer { xpointer: xp };
                    r.session.book().location_of(&pos)
                });
                (current, target)
            }
            _ => return Ok(()),
        };
        match target_loc {
            Some(loc) if loc != current_loc => {
                self.jump_to(loc)?;
                if let Some(progress) = jump.progress {
                    self.weread.set_book_progress(&jump.book_id, progress);
                } else if let Screen::Reader(r) = &self.screen {
                    let fraction = r
                        .session
                        .progress(aristotle_library::library::now())
                        .fraction;
                    let pct = (fraction.clamp(0.0, 1.0) * 100.0).floor() as i64;
                    if pct > 0 {
                        self.weread.set_book_progress(&jump.book_id, pct);
                    }
                }
                if jump.manual {
                    self.readest.manual_message = Some(tr(Msg::ReaderWereadPullDone).to_string());
                    let _ = self.refresh_reader_sync_status();
                }
            }
            Some(_) => {
                if jump.manual {
                    self.readest.manual_message = Some(tr(Msg::ReaderSyncSame).to_string());
                    let _ = self.refresh_reader_sync_status();
                }
            }
            None => {
                if jump.manual {
                    self.readest.manual_message =
                        Some(tr(Msg::WereadPositionUnavailable).to_string());
                    let _ = self.refresh_reader_sync_status();
                }
            }
        }
        Ok(())
    }

    /// Manually pull WeRead cloud progress and notes for the currently open book.
    pub(crate) fn weread_pull_progress_now(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &self.screen else {
            return Ok(());
        };
        let hash = r.session.id.clone();
        self.weread.ensure_loaded(&self.net.paths.state);
        if self.weread.downloads.book_id_of_hash(&hash).is_none() {
            self.readest.manual_message = Some(tr(Msg::WereadNotesNotWeread).to_string());
            return self.refresh_reader_sync_status();
        }
        let Some(credentials) = self.weread.credentials() else {
            self.readest.manual_message = Some(tr(Msg::WereadErrAuth).to_string());
            return self.refresh_reader_sync_status();
        };
        self.allow_weread_hosts();
        match self.weread.web.pull_book_manual(
            &self.net.paths.state,
            &hash,
            Some(credentials),
            crate::net::Net::policy(),
            Instant::now(),
        ) {
            Ok(()) => {
                let _ = self.weread_notes_refresh_now(super::super::notes::FetchReason::Auto);
                self.readest.manual_message = Some(tr(Msg::ReaderWereadPullRunning).to_string());
            }
            Err(message) => {
                self.readest.manual_message = Some(message);
            }
        }
        self.refresh_reader_sync_status()
    }

    /// Manually force-push the open book's local progress and pending reading
    /// time to WeRead.
    pub(crate) fn weread_push_progress_now(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &self.screen else {
            return Ok(());
        };
        let hash = r.session.id.clone();
        let position = r.session.position();
        let fraction = r
            .session
            .progress(aristotle_library::library::now())
            .fraction;
        let xpointer = match position {
            aristotle_render::book::Position::XPointer { xpointer } => Some(xpointer),
            _ => None,
        };
        self.weread.ensure_loaded(&self.net.paths.state);
        let Some(book_id) = self.weread.downloads.book_id_of_hash(&hash) else {
            self.readest.manual_message = Some(tr(Msg::WereadNotesNotWeread).to_string());
            return self.refresh_reader_sync_status();
        };
        self.allow_weread_hosts();
        match self.weread.web.force_push_book(
            &self.net.paths.state,
            crate::net::Net::policy(),
            &book_id,
            xpointer.as_deref(),
            fraction,
        ) {
            Ok(()) => {
                let pct = (fraction.clamp(0.0, 1.0) * 100.0).floor() as i64;
                if pct > 0 {
                    self.weread.set_book_progress(&book_id, pct);
                }
                self.weread_readtime_flush_now();
                self.readest.manual_message = Some(tr(Msg::ReaderWereadPushRunning).to_string());
            }
            Err(message) => {
                self.readest.manual_message = Some(message);
            }
        }
        self.refresh_reader_sync_status()
    }

    /// Copy the machine's state into the login screen without rebuilding it
    /// when the screen is not showing.
    pub(crate) fn sync_weread_web_login(&mut self) {
        if !matches!(self.screen, Screen::WereadLogin) {
            return;
        }
        let Some(phase) = self.weread.web.login_phase() else {
            return;
        };
        let generation = self.weread.login_generation;
        if let Some(url) = self.weread.web.confirm_url() {
            self.weread.login.set_web_code(generation, &url);
        }
        let otp = self.weread.web.login_otp().to_string();
        self.weread
            .login
            .set_web_phase(generation, ui_phase(phase), &otp);
        let _ = self.show_weread_login(aristotle_ui::tree::Refresh::Auto);
    }
}
