//! One account's session, shared by every request that uses it.
//!
//! Several requests can be in flight at the same time (a manual pull, a
//! manual push, the full sync, the cloud shelf), and a rotating refresh token
//! may only be spent once: a second request that refreshes with the same
//! token is refused by the server, and the device looks signed out even
//! though the account is fine.
//!
//! Every client of the same account therefore shares one [`SessionStore`]:
//! the refresh happens once, the other requests wait for its result, and a
//! request that finishes late reads the newest session instead of the one it
//! started with.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::sync::auth::{AuthError, Session};

/// Every store gets its own number: the app replaces the store when the
/// account changes, and a result of the old store must not land in the new
/// account.
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

/// Why no access token could be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// Nobody is signed in.
    NotAuthenticated,
    /// The refresh failed.
    Auth(AuthError),
}

/// The session of one account, shared by the requests that use it.
///
/// The store owns the session; the clients that use it only read it and ask
/// for a token. Cloning the store shares the same session.
#[derive(Clone, Debug)]
pub struct SessionStore {
    inner: Arc<Inner>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    /// Woken when a refresh finished, so the waiting requests look again.
    refreshed: Condvar,
    /// The identity of this store: one per account.
    epoch: u64,
}

#[derive(Debug, Default)]
struct State {
    session: Option<Session>,
    /// How often the session changed. A request that was refused with 401
    /// compares it to tell "my token is old" from "somebody else already
    /// refreshed".
    generation: u64,
    /// Set while a request is refreshing the token.
    refreshing: bool,
}

impl SessionStore {
    /// A store without a session.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                refreshed: Condvar::new(),
                epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed),
            }),
        }
    }

    /// The identity of this store.
    ///
    /// The app replaces the store when the account changes (a sign-in, a
    /// sign-out). A result of the old store belongs to the old account and
    /// must not be applied to the new one: the epoch tells them apart.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// A store that starts with `session`.
    pub fn with_session(session: Option<Session>) -> Self {
        let store = Self::new();
        store.set_session(session);
        store
    }

    /// The session now, if the account is signed in.
    pub fn current(&self) -> Option<Session> {
        self.inner.state.lock().unwrap().session.clone()
    }

    /// How often the session changed.
    ///
    /// A request remembers the value it started with: when it is refused with
    /// 401 and the value has changed since, somebody else refreshed already
    /// and their token is the newer one.
    pub fn generation(&self) -> u64 {
        self.inner.state.lock().unwrap().generation
    }

    /// Set the session, or clear it when it is `None`.
    ///
    /// The caller owns the account (a sign-in, a sign-out); the requests in
    /// flight read the new session from here.
    pub fn set_session(&self, session: Option<Session>) {
        let mut state = self.inner.state.lock().unwrap();
        state.session = session;
        state.generation += 1;
        drop(state);
        self.inner.refreshed.notify_all();
    }

    /// An access token for the account, refreshing the session when it is
    /// expired or about to expire.
    ///
    /// `refresh` performs the network call with the refresh token it is
    /// given. Only one request runs it at a time: the others wait for it and
    /// then use the token it produced, so a rotating token is spent once.
    pub fn access_token(
        &self,
        now_ms: i64,
        margin_ms: i64,
        refresh: &mut dyn FnMut(&str) -> Result<Session, AuthError>,
    ) -> Result<String, TokenError> {
        self.access_token_with_generation(now_ms, margin_ms, refresh)
            .map(|(token, _)| token)
    }

    /// An access token and the generation it belongs to.
    ///
    /// Both come from the same lock hold: a request that is refused with a
    /// 401 must know whether the token it used is still the current one, and
    /// a refresh that happened in between must not look like its own.
    pub fn access_token_with_generation(
        &self,
        now_ms: i64,
        margin_ms: i64,
        refresh: &mut dyn FnMut(&str) -> Result<Session, AuthError>,
    ) -> Result<(String, u64), TokenError> {
        let mut state = self.inner.state.lock().unwrap();
        loop {
            let Some(session) = state.session.clone() else {
                return Err(TokenError::NotAuthenticated);
            };
            if !session.is_expired(now_ms, margin_ms) {
                return Ok((session.access_token.clone(), state.generation));
            }
            if state.refreshing {
                // Somebody is refreshing: wait for it and use its result.
                state = self.inner.refreshed.wait(state).unwrap();
                continue;
            }
            state.refreshing = true;
            let generation = state.generation;
            let token = session.refresh_token.clone();
            drop(state);
            return self.run_refresh(generation, &token, refresh);
        }
    }

    /// A token that is newer than the one a request used.
    ///
    /// `since` is the [`SessionStore::generation`] the request started with.
    /// When somebody else refreshed in the meantime, their token is used: a
    /// rotating refresh token may only be spent once, and a second refresh
    /// with the old one would be refused.
    pub fn refreshed_token(
        &self,
        since: u64,
        refresh: &mut dyn FnMut(&str) -> Result<Session, AuthError>,
    ) -> Result<String, TokenError> {
        let mut state = self.inner.state.lock().unwrap();
        loop {
            if state.generation != since {
                // Somebody refreshed while the request was in flight.
                return match &state.session {
                    Some(s) => Ok(s.access_token.clone()),
                    None => Err(TokenError::NotAuthenticated),
                };
            }
            if state.refreshing {
                state = self.inner.refreshed.wait(state).unwrap();
                continue;
            }
            let Some(session) = state.session.clone() else {
                return Err(TokenError::NotAuthenticated);
            };
            state.refreshing = true;
            let generation = state.generation;
            let token = session.refresh_token.clone();
            drop(state);
            return self
                .run_refresh(generation, &token, refresh)
                .map(|(token, _)| token);
        }
    }

    /// Run the refresh and keep its result, waking the waiting requests.
    ///
    /// `generation` is the generation the refresh started from: when the
    /// account changed while the request was in flight (a sign-out, a new
    /// sign-in), the answer belongs to the account before and must not be
    /// published.
    fn run_refresh(
        &self,
        generation: u64,
        token: &str,
        refresh: &mut dyn FnMut(&str) -> Result<Session, AuthError>,
    ) -> Result<(String, u64), TokenError> {
        let result = refresh(token);
        let mut state = self.inner.state.lock().unwrap();
        state.refreshing = false;
        if state.generation != generation {
            // The account changed under the request: the answer is stale and
            // the credentials it carries must not come back.
            drop(state);
            self.inner.refreshed.notify_all();
            return Err(TokenError::NotAuthenticated);
        }
        match result {
            Ok(new_session) => {
                let access_token = new_session.access_token.clone();
                state.session = Some(new_session);
                state.generation += 1;
                let generation = state.generation;
                drop(state);
                self.inner.refreshed.notify_all();
                Ok((access_token, generation))
            }
            Err(AuthError::AuthExpired) => {
                // The account is gone: nobody may keep using the token.
                state.session = None;
                state.generation += 1;
                drop(state);
                self.inner.refreshed.notify_all();
                Err(TokenError::Auth(AuthError::AuthExpired))
            }
            Err(e) => {
                // The session stays as it was: a network failure must not
                // sign the reader out.
                drop(state);
                self.inner.refreshed.notify_all();
                Err(TokenError::Auth(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn session(access: &str, refresh: &str, expires_at: i64) -> Session {
        Session::new(access, refresh, expires_at)
    }

    #[test]
    fn a_valid_session_needs_no_refresh() {
        let store = SessionStore::with_session(Some(session("a", "r", 10_000_000)));
        let calls = AtomicUsize::new(0);
        let mut refresh = |_: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AuthError::RefreshFailed("not called".to_string()))
        };
        let token = store.access_token(0, 0, &mut refresh).expect("token");
        assert_eq!(token, "a");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_expired_session_is_refreshed_once() {
        let store = SessionStore::with_session(Some(session("", "r", 0)));
        let calls = AtomicUsize::new(0);
        let mut refresh = |token: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(token, "r");
            Ok(session("a2", "r2", 10_000_000))
        };
        let token = store.access_token(0, 60_000, &mut refresh).expect("token");
        assert_eq!(token, "a2");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // The refreshed session is the one the next request uses.
        assert_eq!(store.current().expect("session").refresh_token, "r2");
    }

    #[test]
    fn a_failed_refresh_keeps_the_session() {
        let store = SessionStore::with_session(Some(session("", "r", 0)));
        let mut refresh = |_: &str| Err(AuthError::RefreshFailed("503".to_string()));
        let err = store.access_token(0, 0, &mut refresh).expect_err("failed");
        assert_eq!(
            err,
            TokenError::Auth(AuthError::RefreshFailed("503".to_string()))
        );
        assert!(store.current().is_some(), "the session survives a failure");
    }

    #[test]
    fn an_expired_refresh_token_signs_the_reader_out() {
        let store = SessionStore::with_session(Some(session("", "r", 0)));
        let mut refresh = |_: &str| Err(AuthError::AuthExpired);
        let err = store.access_token(0, 0, &mut refresh).expect_err("failed");
        assert_eq!(err, TokenError::Auth(AuthError::AuthExpired));
        assert!(store.current().is_none());
    }

    /// The account is cleared while a refresh is in flight: the answer of
    /// that refresh belongs to the account before and must not come back.
    #[test]
    fn a_refresh_that_finishes_after_the_account_was_cleared_is_not_published() {
        let store = SessionStore::with_session(Some(session("", "r", 0)));
        let (busy_tx, busy_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let worker = store.clone();
        let handle = std::thread::spawn(move || {
            let mut refresh = |_: &str| {
                busy_tx.send(()).expect("the refresh is running");
                go_rx.recv().expect("the release");
                Ok(session("old-access", "old-refresh", 10_000_000))
            };
            worker.access_token(0, 0, &mut refresh)
        });

        busy_rx.recv().expect("the refresh is running");
        store.set_session(None);
        go_tx.send(()).expect("the release");

        let result = handle.join().expect("the thread");
        assert!(
            result.is_err(),
            "the answer of the old account must not be published"
        );
        assert!(
            store.current().is_none(),
            "the account stays cleared: {:?}",
            store.current()
        );
    }

    /// A new sign-in while a refresh is in flight: the answer of the old
    /// account must not replace the new session.
    #[test]
    fn a_refresh_that_finishes_after_a_new_sign_in_is_not_published() {
        let store = SessionStore::with_session(Some(session("", "r", 0)));
        let (busy_tx, busy_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let worker = store.clone();
        let handle = std::thread::spawn(move || {
            let mut refresh = |_: &str| {
                busy_tx.send(()).expect("the refresh is running");
                go_rx.recv().expect("the release");
                Ok(session("old-access", "old-refresh", 10_000_000))
            };
            worker.access_token(0, 0, &mut refresh)
        });

        busy_rx.recv().expect("the refresh is running");
        store.set_session(Some(session("new-access", "new-refresh", 10_000_000)));
        go_tx.send(()).expect("the release");

        let result = handle.join().expect("the thread");
        assert!(result.is_err(), "the old answer must not be published");
        let current = store.current().expect("the new session");
        assert_eq!(current.access_token, "new-access");
        assert_eq!(current.refresh_token, "new-refresh");
    }

    /// The token and the generation belong together: a request that was
    /// refused must not spend the rotating token again when somebody else
    /// refreshed in the meantime.
    #[test]
    fn the_token_and_the_generation_come_from_the_same_moment() {
        let store = SessionStore::with_session(Some(session("a", "r", 10_000_000)));
        let (token, generation) = store
            .access_token_with_generation(0, 0, &mut |_: &str| {
                Err(AuthError::RefreshFailed("not called".to_string()))
            })
            .expect("a token");
        assert_eq!(token, "a");
        assert_eq!(generation, store.generation());

        // Somebody else refreshed: the generation moved on, the token with it.
        store.set_session(Some(session("a2", "r2", 10_000_000)));
        let calls = AtomicUsize::new(0);
        let mut refresh = |_: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AuthError::RefreshFailed("not called".to_string()))
        };
        let token = store
            .refreshed_token(generation, &mut refresh)
            .expect("a token");
        assert_eq!(token, "a2");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the spent token is not used again"
        );
    }

    #[test]
    fn a_newer_session_replaces_an_older_refresh() {
        let store = SessionStore::with_session(Some(session("a", "r", 10_000_000)));
        let since = store.generation();
        // Somebody else refreshed while the request was in flight.
        store.set_session(Some(session("a2", "r2", 10_000_000)));
        let calls = AtomicUsize::new(0);
        let mut refresh = |_: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(AuthError::RefreshFailed("not called".to_string()))
        };
        let token = store.refreshed_token(since, &mut refresh).expect("token");
        assert_eq!(token, "a2");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the spent token is not used again"
        );
    }
}
