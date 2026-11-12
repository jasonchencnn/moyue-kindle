//! Supabase GoTrue authentication and session management (contract §10.3).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};

/// Configuration for sync service endpoints and credentials.
///
/// Neither API base nor Supabase credentials are hardcoded into request logic.
#[derive(Clone, PartialEq, Eq)]
pub struct SyncConfig {
    /// Base URL for Readest sync endpoints (default: the official Readest API).
    pub api_base: String,
    /// Base URL for Supabase authentication (default: the official Readest
    /// auth proxy).
    pub supabase_url: String,
    /// Supabase anonymous API key.
    pub supabase_anon_key: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            api_base: "https://web.readest.com/api".to_string(),
            // The official client reaches GoTrue through this proxy: a direct
            // `*.supabase.co` connection is reset on some networks.
            supabase_url: "https://auth-cf.readest.com".to_string(),
            // Decoded from `NEXT_PUBLIC_DEFAULT_SUPABASE_KEY_BASE64` in upstream `apps/readest-app/.env`.
            supabase_anon_key:
                "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6InZic3l4ZnVzampxZHhranFseXNjIiwicm9sZSI6ImFub24iLCJpYXQiOjE3MzQxMjM2NzEsImV4cCI6MjA0OTY5OTY3MX0.3U5Uqaou_1SgrVe1eo9rApc0uKjqhpQdUXhvwUHmUfg"
                    .to_string(),
        }
    }
}

impl fmt::Debug for SyncConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncConfig")
            .field("api_base", &self.api_base)
            .field("supabase_url", &self.supabase_url)
            .field("supabase_anon_key", &"[REDACTED]")
            .finish()
    }
}

/// An authenticated session holding access and refresh tokens.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Absolute expiration timestamp in Unix epoch milliseconds.
    pub expires_at: i64,
}

impl Session {
    /// Create a new session.
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_at: i64,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at,
        }
    }

    /// Check if the session is expired or within `margin_ms` of expiring.
    pub fn is_expired(&self, now_ms: i64, margin_ms: i64) -> bool {
        now_ms.saturating_add(margin_ms) >= self.expires_at
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Authentication and session errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("session expired, user re-login required")]
    AuthExpired,
    #[error("authentication login failed: {0}")]
    LoginFailed(String),
    #[error("token refresh failed: {0}")]
    RefreshFailed(String),
    #[error("transport error during auth: {0}")]
    Transport(String),
    /// The device refused the authentication address (network allowlist, or
    /// a build without HTTPS): the network itself may be fine.
    #[error("auth endpoint not reachable from this device: {0}")]
    Blocked(String),
    /// The authentication response exceeded the local byte limit.
    #[error("auth response exceeded the {0}-byte limit")]
    TooLarge(u64),
    #[error("invalid auth response payload: {0}")]
    InvalidResponse(String),
}

impl From<TransportError> for AuthError {
    fn from(err: TransportError) -> Self {
        match err {
            TransportError::Blocked(detail) => Self::Blocked(detail),
            TransportError::TooLarge(limit) => Self::TooLarge(limit),
            other => Self::Transport(other.to_string()),
        }
    }
}

#[derive(Serialize)]
struct PasswordGrantBody<'a> {
    email: &'a str,
    password: &'a str,
}

#[derive(Serialize)]
struct RefreshGrantBody<'a> {
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct GoTrueTokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: Option<i64>,
}

/// Build an HTTP request for Supabase GoTrue password login.
pub fn build_password_login_request(
    config: &SyncConfig,
    email: &str,
    password: &str,
) -> Result<HttpRequest, AuthError> {
    let url = format!("{}/auth/v1/token?grant_type=password", config.supabase_url);
    let body = serde_json::to_vec(&PasswordGrantBody { email, password })
        .map_err(|e| AuthError::InvalidResponse(e.to_string()))?;

    Ok(HttpRequest::new("POST", url)
        .with_header("apikey", &config.supabase_anon_key)
        .with_header("Content-Type", "application/json")
        .with_body(body))
}

/// Build an HTTP request for Supabase GoTrue token refresh.
pub fn build_refresh_request(
    config: &SyncConfig,
    refresh_token: &str,
) -> Result<HttpRequest, AuthError> {
    let url = format!(
        "{}/auth/v1/token?grant_type=refresh_token",
        config.supabase_url
    );
    let body = serde_json::to_vec(&RefreshGrantBody { refresh_token })
        .map_err(|e| AuthError::InvalidResponse(e.to_string()))?;

    Ok(HttpRequest::new("POST", url)
        .with_header("apikey", &config.supabase_anon_key)
        .with_header("Content-Type", "application/json")
        .with_body(body))
}

/// Parse a GoTrue token response into a `Session`.
pub fn parse_token_response(resp: &HttpResponse, now_ms: i64) -> Result<Session, AuthError> {
    if resp.status == 400 || resp.status == 401 || resp.status == 403 {
        return Err(AuthError::AuthExpired);
    }

    if resp.status != 200 {
        return Err(AuthError::RefreshFailed(format!("HTTP {}", resp.status)));
    }

    let token_resp: GoTrueTokenResponse = serde_json::from_slice(&resp.body)
        .map_err(|e| AuthError::InvalidResponse(format!("failed to parse token JSON: {e}")))?;

    let expires_in_sec = token_resp.expires_in.unwrap_or(3600);
    let expires_at = now_ms.saturating_add(expires_in_sec * 1000);

    Ok(Session::new(
        token_resp.access_token,
        token_resp.refresh_token,
        expires_at,
    ))
}

/// Perform password login over the given transport.
pub fn login<T: HttpTransport>(
    transport: &T,
    config: &SyncConfig,
    email: &str,
    password: &str,
    now_ms: i64,
) -> Result<Session, AuthError> {
    let req = build_password_login_request(config, email, password)?;
    let resp = transport.send(req)?;
    parse_token_response(&resp, now_ms).map_err(|err| match err {
        AuthError::AuthExpired => AuthError::LoginFailed("invalid email or password".to_string()),
        AuthError::RefreshFailed(msg) => AuthError::LoginFailed(msg),
        other => other,
    })
}

/// Refresh an active session over the given transport.
pub fn refresh<T: HttpTransport>(
    transport: &T,
    config: &SyncConfig,
    refresh_token: &str,
    now_ms: i64,
) -> Result<Session, AuthError> {
    let req = build_refresh_request(config, refresh_token)?;
    let resp = transport.send(req)?;
    parse_token_response(&resp, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockAuthTransport {
        response_status: u16,
        response_body: Vec<u8>,
    }

    impl HttpTransport for MockAuthTransport {
        fn send(&self, _req: HttpRequest) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse::new(
                self.response_status,
                self.response_body.clone(),
            ))
        }
    }

    #[test]
    fn credential_masking_in_debug() {
        let config = SyncConfig {
            api_base: "https://my-api.com".to_string(),
            supabase_url: "https://my-supabase.com".to_string(),
            supabase_anon_key: "super-secret-anon-key-12345".to_string(),
        };
        let session = Session::new("access-token-9999", "refresh-token-8888", 1762344000000);

        let config_debug = format!("{config:?}");
        assert!(!config_debug.contains("super-secret-anon-key-12345"));
        assert!(config_debug.contains("[REDACTED]"));

        let session_debug = format!("{session:?}");
        assert!(!session_debug.contains("access-token-9999"));
        assert!(!session_debug.contains("refresh-token-8888"));
        assert!(session_debug.contains("[REDACTED]"));
    }

    #[test]
    fn session_expiration_check() {
        let session = Session::new("acc", "ref", 10_000);
        // Expiration is at 10_000ms.
        // At 5_000 with margin 2_000 (total 7_000) -> not expired
        assert!(!session.is_expired(5_000, 2_000));
        // At 8_500 with margin 2_000 (total 10_500) -> expired
        assert!(session.is_expired(8_500, 2_000));
        // At 10_000 -> expired
        assert!(session.is_expired(10_000, 0));
    }

    #[test]
    fn login_success_and_failure() {
        let config = SyncConfig::default();
        let success_body = serde_json::to_vec(&serde_json::json!({
            "access_token": "acc-1",
            "refresh_token": "ref-1",
            "expires_in": 3600
        }))
        .unwrap();

        let transport = MockAuthTransport {
            response_status: 200,
            response_body: success_body,
        };

        let session = login(&transport, &config, "u@test.com", "pw", 1000).unwrap();
        assert_eq!(session.access_token, "acc-1");
        assert_eq!(session.refresh_token, "ref-1");
        assert_eq!(session.expires_at, 1000 + 3600 * 1000);

        let fail_body = serde_json::to_vec(&serde_json::json!({
            "error": "invalid_grant",
            "error_description": "Invalid login credentials"
        }))
        .unwrap();
        let fail_transport = MockAuthTransport {
            response_status: 400,
            response_body: fail_body,
        };
        let err = login(&fail_transport, &config, "u@test.com", "pw", 1000).unwrap_err();
        assert!(matches!(err, AuthError::LoginFailed(_)));
    }

    #[test]
    fn refresh_failure_returns_auth_expired() {
        let config = SyncConfig::default();
        let fail_transport = MockAuthTransport {
            response_status: 401,
            response_body: b"{\"error\":\"invalid_grant\"}".to_vec(),
        };
        let err = refresh(&fail_transport, &config, "old-ref-token", 1000).unwrap_err();
        assert_eq!(err, AuthError::AuthExpired);
    }

    #[test]
    fn refresh_500_or_429_returns_refresh_failed_not_auth_expired() {
        let config = SyncConfig::default();
        let server_err_transport = MockAuthTransport {
            response_status: 500,
            response_body: b"Internal Server Error".to_vec(),
        };
        let err = refresh(&server_err_transport, &config, "old-ref-token", 1000).unwrap_err();
        assert!(matches!(err, AuthError::RefreshFailed(_)));
        assert_ne!(err, AuthError::AuthExpired);

        let rate_limit_transport = MockAuthTransport {
            response_status: 429,
            response_body: b"Too Many Requests".to_vec(),
        };
        let err429 = refresh(&rate_limit_transport, &config, "old-ref-token", 1000).unwrap_err();
        assert!(matches!(err429, AuthError::RefreshFailed(_)));
        assert_ne!(err429, AuthError::AuthExpired);
    }

    #[test]
    fn default_endpoints_use_the_official_auth_proxy() {
        let config = SyncConfig::default();
        assert_eq!(config.supabase_url, "https://auth-cf.readest.com");

        let login_req = build_password_login_request(&config, "u@test.com", "pw").unwrap();
        assert_eq!(login_req.method, "POST");
        assert_eq!(
            login_req.url,
            "https://auth-cf.readest.com/auth/v1/token?grant_type=password"
        );
        assert_eq!(
            login_req
                .headers
                .iter()
                .find(|(k, _)| k == "apikey")
                .map(|(_, v)| v.as_str()),
            Some(config.supabase_anon_key.as_str())
        );

        let refresh_req = build_refresh_request(&config, "ref").unwrap();
        assert_eq!(refresh_req.method, "POST");
        assert_eq!(
            refresh_req.url,
            "https://auth-cf.readest.com/auth/v1/token?grant_type=refresh_token"
        );
    }

    #[test]
    fn a_custom_auth_endpoint_still_wins() {
        let config = SyncConfig {
            supabase_url: "https://auth.example.com".to_string(),
            ..SyncConfig::default()
        };

        let login_req = build_password_login_request(&config, "u@test.com", "pw").unwrap();
        assert_eq!(
            login_req.url,
            "https://auth.example.com/auth/v1/token?grant_type=password"
        );
        let refresh_req = build_refresh_request(&config, "ref").unwrap();
        assert_eq!(
            refresh_req.url,
            "https://auth.example.com/auth/v1/token?grant_type=refresh_token"
        );
        // The API base keeps its own default: the two are independent.
        assert_eq!(config.api_base, "https://web.readest.com/api");
    }
}
