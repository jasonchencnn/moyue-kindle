//! Synchronous HTTP transport abstraction decoupled from any concrete network stack.

use std::fmt;

/// An HTTP request for synchronous transmission.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// Create a new `HttpRequest`.
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Add a header to the request.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Set the body bytes.
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

fn sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "apikey"
            | "cookie"
            | "set-cookie"
            | "vid"
            | "accesstoken"
            | "x-vid"
            | "x-skey"
    )
}

fn mask_sensitive_url(url: &str) -> String {
    if let Some(idx) = url.find('?') {
        let query = &url[idx + 1..];
        let lower = query.to_ascii_lowercase();
        if lower.contains("signature")
            || lower.contains("x-amz-")
            || lower.contains("sig=")
            || lower.contains("token=")
            || lower.contains("secret=")
            || lower.contains("auth=")
            || lower.contains("filekey=")
            || lower.contains("ticket=")
            || lower.contains("otp=")
        {
            format!("{}?[QUERY_REDACTED]", &url[..idx])
        } else {
            url.to_string()
        }
    } else {
        url.to_string()
    }
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact any Authorization or API key headers in debug logs
        let masked_headers: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                if sensitive_header(k) {
                    (k.clone(), "[REDACTED]".to_string())
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect();

        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &mask_sensitive_url(&self.url))
            .field("headers", &masked_headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// An HTTP response received from the transport.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Create a new `HttpResponse`.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// Add a header to the response.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let masked_headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                let lower = k.to_ascii_lowercase();
                if lower == "authorization"
                    || lower == "apikey"
                    || lower == "set-cookie"
                    || lower == "www-authenticate"
                {
                    (k.as_str(), "[REDACTED]")
                } else {
                    (k.as_str(), v.as_str())
                }
            })
            .collect();

        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &masked_headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Transport-level failures occurring before or during HTTP message exchange.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("network connection failure: {0}")]
    Network(String),
    #[error("request timeout")]
    Timeout,
    #[error("unexpected http status {0}: {1}")]
    HttpStatus(u16, String),
    /// The request never left the device: the device's policy refused the
    /// address, or the build has no HTTPS. The network may be perfectly fine,
    /// so this is not a network failure.
    #[error("endpoint not reachable from this device: {0}")]
    Blocked(String),
    /// The response exceeded the local byte limit.
    #[error("response exceeded the {0}-byte limit")]
    TooLarge(u64),
    #[error("transport error: {0}")]
    Other(String),
}

/// Synchronous HTTP transport abstraction.
pub trait HttpTransport {
    /// Send an HTTP request and block until response or transport error.
    fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_masks_sensitive_headers() {
        let req = HttpRequest::new("POST", "https://example.com/api/sync")
            .with_header("Authorization", "Bearer sensitive-secret-jwt")
            .with_header("apikey", "my-secret-anon-key")
            .with_header("Content-Type", "application/json");

        let debug_str = format!("{req:?}");
        assert!(!debug_str.contains("sensitive-secret-jwt"));
        assert!(!debug_str.contains("my-secret-anon-key"));
        assert!(debug_str.contains("[REDACTED]"));
        assert!(debug_str.contains("application/json"));

        let resp = HttpResponse::new(200, vec![])
            .with_header("Set-Cookie", "session=super-secret-cookie")
            .with_header("WWW-Authenticate", "Basic realm=secret")
            .with_header("Content-Type", "application/json");
        let resp_debug = format!("{resp:?}");
        assert!(!resp_debug.contains("super-secret-cookie"));
        assert!(!resp_debug.contains("Basic realm=secret"));
        assert!(resp_debug.contains("[REDACTED]"));
        assert!(resp_debug.contains("application/json"));
    }

    #[test]
    fn debug_masks_sensitive_query_in_url() {
        let req = HttpRequest::new(
            "GET",
            "https://r2.storage.example.com/books/123.epub?X-Amz-Signature=secret123&X-Amz-Algorithm=AWS4-HMAC-SHA256",
        );
        let debug_str = format!("{req:?}");
        assert!(!debug_str.contains("secret123"));
        assert!(!debug_str.contains("X-Amz-Signature"));
        assert!(debug_str.contains("[QUERY_REDACTED]"));
        assert!(debug_str.contains("https://r2.storage.example.com/books/123.epub"));
    }
}
