// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bounded Web sign-in state machine (§14.1 step 3).
//!
//! It never polls on its own: the main loop asks [`LoginMachine::tick`] for at
//! most one request. A deadline, a transport-failure cap and an OTP-attempt cap
//! make an endless loop impossible.

use std::time::{Duration, Instant};

use aristotle_i18n::{tr, Msg};
use aristotle_render::plato_core::anyhow::Error;
use aristotle_ui::screens::weread_login as ui_login;
use aristotle_ui::tree::Refresh;

use inkread_core::weread::auth::percent_encode;
use inkread_core::weread::{CookieJar, WeReadError, WebLoginPoll, WebLoginSuccess};

use crate::{App, Flow};

/// A Web QR code and its confirmations are followed for this long.
pub const LOGIN_DEADLINE: Duration = Duration::from_secs(300);
/// Pause between two `getLoginInfo` polls.
const POLL_INTERVAL: Duration = Duration::from_millis(800);
/// Consecutive transport failures before the sign-in gives up.
pub(super) const MAX_POLL_FAILURES: u32 = 5;
/// Wrong OTP entries before the sign-in gives up.
const MAX_OTP_ATTEMPTS: u32 = 5;

/// The visible states of the Web sign-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebLoginPhase {
    /// The skills page and the `uid` are being fetched.
    #[default]
    Fetching,
    /// The QR code is on screen.
    Waiting,
    /// The phone asks for the four-digit code.
    NeedOtp,
    /// The last code was wrong.
    OtpMismatch,
    /// The QR code or the OTP window expired.
    Expired,
    /// A transport or server failure ended the attempt.
    Failed,
    /// The cookies are in the jar.
    Confirmed,
}

/// One `getLoginInfo` request the main loop should run on a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PollRequest {
    pub uid: String,
    pub otp: String,
}

/// The bounded sign-in state machine.
pub(super) struct LoginMachine {
    pub generation: u64,
    pub phase: WebLoginPhase,
    uid: Option<String>,
    pub(super) jar: CookieJar,
    otp: String,
    otp_attempts: u32,
    failures: u32,
    started: Instant,
    next_poll: Instant,
    pub(super) in_flight: bool,
}

impl std::fmt::Debug for LoginMachine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginMachine")
            .field("generation", &self.generation)
            .field("phase", &self.phase)
            .field("uid", &self.uid.as_ref().map(|_| "[REDACTED]"))
            .field("jar", &self.jar)
            .field("otp_len", &self.otp.len())
            .field("failures", &self.failures)
            .field("in_flight", &self.in_flight)
            .finish()
    }
}

impl LoginMachine {
    pub(super) fn new(generation: u64, now: Instant) -> Self {
        Self {
            generation,
            phase: WebLoginPhase::Fetching,
            uid: None,
            jar: CookieJar::new(),
            otp: String::new(),
            otp_attempts: 0,
            failures: 0,
            started: now,
            next_poll: now,
            in_flight: false,
        }
    }

    pub(super) fn confirm_url(&self) -> Option<String> {
        self.uid.as_deref().map(|uid| {
            format!(
                "https://weread.qq.com/web/confirm?uid={}",
                percent_encode(uid)
            )
        })
    }

    fn deadline_passed(&self, now: Instant) -> bool {
        now.duration_since(self.started) >= LOGIN_DEADLINE
    }

    fn fail_if_expired(&mut self, now: Instant) -> bool {
        if !matches!(
            self.phase,
            WebLoginPhase::Confirmed | WebLoginPhase::Expired | WebLoginPhase::Failed
        ) && self.deadline_passed(now)
        {
            self.phase = WebLoginPhase::Expired;
            self.in_flight = false;
            return true;
        }
        false
    }

    pub(super) fn on_fetched(&mut self, uid: String, jar: CookieJar, now: Instant) {
        self.uid = Some(uid);
        self.jar = jar;
        self.phase = WebLoginPhase::Waiting;
        self.next_poll = now;
        self.in_flight = false;
    }

    pub(super) fn on_fetch_failed(&mut self, error: &WeReadError) {
        self.phase = match error {
            WeReadError::WebAuthExpired | WeReadError::AuthExpired => WebLoginPhase::Expired,
            _ => WebLoginPhase::Failed,
        };
        self.in_flight = false;
    }

    pub(super) fn on_poll(
        &mut self,
        poll: WebLoginPoll,
        jar: CookieJar,
        now: Instant,
    ) -> Option<WebLoginSuccess> {
        self.in_flight = false;
        self.jar = jar;
        self.failures = 0;
        match poll {
            WebLoginPoll::Confirmed(success) => {
                self.phase = WebLoginPhase::Confirmed;
                return Some(success);
            }
            WebLoginPoll::NeedOtp => {
                self.phase = WebLoginPhase::NeedOtp;
                self.next_poll = now + POLL_INTERVAL;
            }
            WebLoginPoll::OtpMismatch => {
                self.otp.clear();
                self.otp_attempts = self.otp_attempts.saturating_add(1);
                self.phase = if self.otp_attempts >= MAX_OTP_ATTEMPTS {
                    WebLoginPhase::Failed
                } else {
                    WebLoginPhase::OtpMismatch
                };
                self.next_poll = now + POLL_INTERVAL;
            }
            WebLoginPoll::Declined | WebLoginPoll::Expired => {
                self.phase = WebLoginPhase::Expired;
            }
            WebLoginPoll::Continue => {
                self.phase = if self.deadline_passed(now) {
                    WebLoginPhase::Expired
                } else {
                    WebLoginPhase::Waiting
                };
                self.next_poll = now + POLL_INTERVAL;
            }
        }
        None
    }

    pub(super) fn on_poll_failed(&mut self, error: &WeReadError, now: Instant) {
        self.in_flight = false;
        match error {
            WeReadError::WebAuthExpired
            | WeReadError::AuthExpired
            | WeReadError::ManualVerification(_) => self.phase = WebLoginPhase::Expired,
            WeReadError::RateLimited(_) => self.phase = WebLoginPhase::Failed,
            _ => {
                self.failures = self.failures.saturating_add(1);
                if self.failures >= MAX_POLL_FAILURES || self.deadline_passed(now) {
                    self.phase = WebLoginPhase::Failed;
                } else {
                    let backoff = Duration::from_secs(1u64 << self.failures.min(4));
                    self.next_poll = now + backoff.min(Duration::from_secs(15));
                }
            }
        }
    }

    /// Append one digit of the OTP; `false` when the phase does not take one.
    pub(super) fn push_digit(&mut self, digit: char) -> bool {
        if !matches!(
            self.phase,
            WebLoginPhase::NeedOtp | WebLoginPhase::OtpMismatch
        ) || !digit.is_ascii_digit()
            || self.otp.len() >= 4
        {
            return false;
        }
        self.otp.push(digit);
        true
    }

    pub(super) fn delete_digit(&mut self) -> bool {
        self.otp.pop().is_some()
    }

    /// Submit the four digits; `false` when they are incomplete.
    pub(super) fn submit_otp(&mut self, digits: &str, now: Instant) -> bool {
        if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        self.otp = digits.to_string();
        self.phase = WebLoginPhase::Waiting;
        self.next_poll = now;
        true
    }

    pub(super) fn otp(&self) -> &str {
        &self.otp
    }

    fn active(&self) -> bool {
        matches!(
            self.phase,
            WebLoginPhase::Fetching
                | WebLoginPhase::Waiting
                | WebLoginPhase::NeedOtp
                | WebLoginPhase::OtpMismatch
        )
    }

    /// The request the main loop should run now, if any.
    pub(super) fn tick(&mut self, now: Instant) -> Option<PollRequest> {
        if !self.active() {
            return None;
        }
        // The deadline is checked before `in_flight`: a worker that never
        // answers must still end the attempt instead of hanging the screen.
        if self.fail_if_expired(now) {
            return None;
        }
        if self.in_flight {
            return None;
        }
        // The OTP step waits for the reader; no poll burns a request meanwhile.
        if matches!(
            self.phase,
            WebLoginPhase::NeedOtp | WebLoginPhase::OtpMismatch
        ) {
            return None;
        }
        if now < self.next_poll {
            return None;
        }
        let uid = self.uid.clone()?;
        self.in_flight = true;
        Some(PollRequest {
            uid,
            otp: self.otp.clone(),
        })
    }
}

impl App {
    /// The Web stage of the sign-in screen: OTP digits, confirm, new code and
    /// leaving. The OTP stays in the screen state; the machine only gets the
    /// submitted four digits.
    pub(crate) fn on_weread_web_login_event(
        &mut self,
        id: aristotle_ui::tree::ActionId,
    ) -> Result<Flow, Error> {
        match id {
            ui_login::action::BACK => {
                self.weread.cancel_login();
                self.weread.web.cancel_login();
                if self.weread.signed_in {
                    self.show_weread_shelf(Refresh::Full, 0)?;
                } else {
                    self.show_library(Refresh::Full)?;
                }
                return Ok(Flow::Continue);
            }
            ui_login::action::REFRESH => {
                self.start_weread_web_login()?;
                return Ok(Flow::Continue);
            }
            id if (ui_login::action::WEB_DIGIT..ui_login::action::WEB_DIGIT + 10).contains(&id) => {
                let digit = (b'0' + (id - ui_login::action::WEB_DIGIT) as u8) as char;
                if self.weread.login.push_otp(digit) {
                    self.weread.web.push_otp_digit(digit);
                    self.show_weread_login(Refresh::Auto)?;
                }
                return Ok(Flow::Continue);
            }
            ui_login::action::WEB_DELETE => {
                if self.weread.login.delete_otp() {
                    self.weread.web.delete_otp_digit();
                    self.show_weread_login(Refresh::Auto)?;
                }
                return Ok(Flow::Continue);
            }
            ui_login::action::WEB_CONFIRM => {
                let otp = self.weread.login.otp.clone();
                if otp.len() == 4 {
                    self.weread.web.submit_otp(&otp, std::time::Instant::now());
                    self.weread.login.set_web_phase(
                        self.weread.login_generation,
                        ui_login::WereadWebPhase::Waiting,
                        &otp,
                    );
                } else {
                    self.weread.login.message = Some(tr(Msg::WereadWebOtpIncomplete).to_string());
                }
                self.show_weread_login(Refresh::Auto)?;
                return Ok(Flow::Continue);
            }
            _ => {}
        }

        Ok(Flow::Continue)
    }
}

pub(super) fn ui_phase(phase: WebLoginPhase) -> ui_login::WereadWebPhase {
    match phase {
        WebLoginPhase::Fetching => ui_login::WereadWebPhase::Fetching,
        WebLoginPhase::Waiting => ui_login::WereadWebPhase::Waiting,
        WebLoginPhase::NeedOtp => ui_login::WereadWebPhase::NeedOtp,
        WebLoginPhase::OtpMismatch => ui_login::WereadWebPhase::OtpMismatch,
        WebLoginPhase::Expired => ui_login::WereadWebPhase::Expired,
        WebLoginPhase::Failed => ui_login::WereadWebPhase::Failed,
        WebLoginPhase::Confirmed => ui_login::WereadWebPhase::Confirmed,
    }
}
