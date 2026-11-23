//! WeRead E-Ink login flow (contract §14.2).
//!
//! The three pure builders (`build_qrconnect_url`, `poll_status`, `build_login_body`) plus the
//! refresh-token request are enough to drive a QR login without a network stack in this module.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sync::transport::{HttpRequest, HttpResponse};

use super::model::as_string_or_number;
use super::WeReadError;

/// E-Ink gateway used by every authenticated endpoint.
pub const EINK_BASE: &str = "https://i.weread.qq.com";
/// WeChat application id of the WeRead E-Ink client.
pub const WX_APPID: &str = "wxab9b71ad2b90ff34";
/// Scopes requested by the official E-Ink client.
pub const WX_SCOPE: &str = "snsapi_userinfo,snsapi_timeline,snsapi_friend";
/// Public E-Ink client default, used when `/feature` has no `guest_token`.
pub const DEFAULT_GUEST_TOKEN: &str = "5ecdcfd7f";
/// `deviceType=3` identifies the E-Ink client in `/login`.
pub const DEVICE_TYPE_EINK: u8 = 3;
/// Synthetic-safe device name; callers may override it in the login body.
pub const DEFAULT_DEVICE_NAME: &str = "BOOX";
/// Version headers of the E-Ink 2.1.2 client.
pub const USER_AGENT: &str =
    "WeRead/2.1.2 WRBrand/Onyx wr_eink Dalvik/2.1.0 (Linux; U; Android 11; BOOX Build/onyx)";

/// Account credentials for the E-Ink channel.
///
/// `Debug` masks every field: these values are secrets and must not reach logs (contract §14.1).
#[derive(Clone)]
pub struct Credentials {
    pub vid: String,
    pub access_token: String,
    pub refresh_token: String,
    pub device_id: String,
}

impl Credentials {
    /// Create credentials for one E-Ink account.
    pub fn new(
        vid: impl Into<String>,
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        Self {
            vid: vid.into(),
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            device_id: device_id.into(),
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("vid", &"[REDACTED]")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("device_id", &"[REDACTED]")
            .finish()
    }
}

/// `GET /wxticket?nonceStr=weread` response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QrTicket {
    pub signature: String,
    #[serde(rename = "timeStamp", deserialize_with = "de_i64")]
    pub time_stamp: i64,
}

fn de_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .ok_or_else(|| serde::de::Error::custom("expected an integer"))
}

/// `POST /login` body for the QR-code exchange.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LoginBody {
    pub code: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
    #[serde(rename = "deviceType")]
    pub device_type: u8,
    pub timestamp: i64,
    pub random: u16,
    #[serde(rename = "trackId")]
    pub track_id: String,
    pub signature: String,
    #[serde(rename = "appFirstInstall")]
    pub app_first_install: u8,
    #[serde(rename = "installId")]
    pub install_id: String,
    #[serde(rename = "isAutoLogout")]
    pub is_auto_logout: u8,
    #[serde(rename = "isFromQrcode")]
    pub is_from_qrcode: u8,
}

/// `POST /login` body for refresh-token rotation.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RefreshBody {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
    #[serde(rename = "deviceType")]
    pub device_type: u8,
    pub timestamp: i64,
    pub random: u16,
    #[serde(rename = "trackId")]
    pub track_id: String,
    pub signature: String,
    #[serde(rename = "refreshToken")]
    pub refresh_token: String,
    #[serde(rename = "inBackground")]
    pub in_background: u8,
    #[serde(rename = "kickType")]
    pub kick_type: u8,
    #[serde(rename = "refCgi")]
    pub ref_cgi: String,
}

/// Result of one long-poll round (contract §14.2 step 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrPollStatus {
    /// 404 — the code was scanned.
    Scanned,
    /// 405 — the user confirmed; the value is the `wx_code` used by `/login`.
    Confirmed(String),
    /// 402 — the QR code expired.
    Expired,
    /// 403 — the user declined.
    Declined,
    /// 408 — keep polling.
    Continue,
}

fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// Percent-encode one query value (RFC 3986 unreserved bytes stay literal).
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// Build the WeChat QR-code URL from a ticket (contract §14.2 step 2).
pub fn build_qrconnect_url(ticket: &QrTicket) -> String {
    format!(
        "https://open.weixin.qq.com/connect/sdk/qrconnect?appid={WX_APPID}&noncestr=weread&timestamp={}&scope={}&signature={}",
        ticket.time_stamp,
        percent_encode(WX_SCOPE),
        percent_encode(&ticket.signature)
    )
}

/// Parse `GET /wxticket?nonceStr=weread`.
pub fn parse_qr_ticket(resp: &HttpResponse) -> Result<QrTicket, WeReadError> {
    if let Some(error) = super::response_error(resp.status, &resp.body) {
        return Err(error);
    }
    serde_json::from_slice(&resp.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("wxticket: {error}")))
}

/// Parse the `qrconnect` response and return its `uuid`.
pub fn parse_qrconnect_response(resp: &HttpResponse) -> Result<String, WeReadError> {
    #[derive(Deserialize)]
    struct Wire {
        errcode: Option<serde_json::Value>,
        uuid: Option<String>,
    }
    if let Some(error) = super::response_error(resp.status, &resp.body) {
        return Err(error);
    }
    let wire: Wire = serde_json::from_slice(&resp.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("qrconnect: {error}")))?;
    let code = wire
        .errcode
        .as_ref()
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .unwrap_or(0);
    if code != 0 {
        return Err(WeReadError::Api {
            code,
            message: "qrconnect rejected".to_string(),
        });
    }
    match wire.uuid {
        Some(uuid) if !uuid.is_empty() => Ok(uuid),
        _ => Err(WeReadError::InvalidResponse(
            "qrconnect returned no uuid".to_string(),
        )),
    }
}

/// Decode one long-poll response into the §14.2 state machine.
pub fn poll_status(resp: &HttpResponse) -> Result<QrPollStatus, WeReadError> {
    if let Some(error) = super::response_error(resp.status, &resp.body) {
        return Err(error);
    }
    let value: serde_json::Value = serde_json::from_slice(&resp.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("qr poll: {error}")))?;
    let code = value
        .get("wx_errcode")
        .or_else(|| value.get("wx_errcode_str"))
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .ok_or_else(|| WeReadError::InvalidResponse("qr poll has no wx_errcode".to_string()))?;
    match code {
        404 => Ok(QrPollStatus::Scanned),
        405 => match value.get("wx_code").and_then(|value| value.as_str()) {
            Some(code) if !code.is_empty() => Ok(QrPollStatus::Confirmed(code.to_string())),
            _ => Err(WeReadError::InvalidResponse(
                "qr poll confirmation has no wx_code".to_string(),
            )),
        },
        402 => Ok(QrPollStatus::Expired),
        403 => Ok(QrPollStatus::Declined),
        408 => Ok(QrPollStatus::Continue),
        other => Err(WeReadError::InvalidResponse(format!(
            "qr poll returned unknown status {other}"
        ))),
    }
}

/// Build the QR-code `/login` body, including `sha256(timestamp + deviceId + random)`.
pub fn build_login_body(
    code: impl Into<String>,
    device_id: impl Into<String>,
    device_name: impl Into<String>,
    install_id: impl Into<String>,
    timestamp: i64,
    random: u16,
) -> LoginBody {
    let device_id = device_id.into();
    let signature = sha256_hex(&format!("{timestamp}{device_id}{random}"));
    LoginBody {
        code: code.into(),
        device_id,
        device_name: device_name.into(),
        device_type: DEVICE_TYPE_EINK,
        timestamp,
        random,
        track_id: String::new(),
        signature,
        app_first_install: 1,
        install_id: install_id.into(),
        is_auto_logout: 0,
        is_from_qrcode: 1,
    }
}

/// Build the refresh-token `/login` body; the signature uses the same recipe as QR login.
pub fn build_refresh_body(credentials: &Credentials, timestamp: i64, random: u16) -> RefreshBody {
    let signature = sha256_hex(&format!("{timestamp}{}{random}", credentials.device_id));
    RefreshBody {
        device_id: credentials.device_id.clone(),
        device_name: DEFAULT_DEVICE_NAME.to_string(),
        device_type: DEVICE_TYPE_EINK,
        timestamp,
        random,
        track_id: String::new(),
        signature,
        refresh_token: credentials.refresh_token.clone(),
        in_background: 0,
        kick_type: 1,
        ref_cgi: String::new(),
    }
}

/// Headers sent on every E-Ink request.
pub fn version_headers() -> Vec<(String, String)> {
    vec![
        ("baseapi".to_string(), "30".to_string()),
        ("appver".to_string(), "2.1.2.10245900".to_string()),
        ("basever".to_string(), "2.1.2.10245900".to_string()),
        ("osver".to_string(), "11".to_string()),
        ("channelId".to_string(), "900".to_string()),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
    ]
}

/// Headers that authenticate one account (plain `vid` and `accessToken` headers, §14.1).
pub fn auth_headers(credentials: &Credentials) -> Vec<(String, String)> {
    let mut headers = version_headers();
    headers.push(("vid".to_string(), credentials.vid.clone()));
    headers.push(("accessToken".to_string(), credentials.access_token.clone()));
    headers
}

fn json_request(url: String, body: &impl Serialize) -> Result<HttpRequest, WeReadError> {
    let body = serde_json::to_vec(body)
        .map_err(|error| WeReadError::InvalidParameter(format!("json body: {error}")))?;
    let mut request = HttpRequest::new("POST", url)
        .with_header("Content-Type", "application/json; charset=UTF-8")
        .with_body(body);
    for (name, value) in version_headers() {
        request = request.with_header(name, value);
    }
    Ok(request)
}

/// Build the QR-code `/login` request.
pub fn build_login_request(base_url: &str, body: &LoginBody) -> Result<HttpRequest, WeReadError> {
    json_request(format!("{}/login", base_url.trim_end_matches('/')), body)
}

/// Build the refresh-token `/login` request.
pub fn build_refresh_request(
    base_url: &str,
    body: &RefreshBody,
) -> Result<HttpRequest, WeReadError> {
    json_request(format!("{}/login", base_url.trim_end_matches('/')), body)
}

/// Parse `{vid, accessToken, refreshToken}` into credentials.
pub fn parse_login_response(
    resp: &HttpResponse,
    device_id: &str,
) -> Result<Credentials, WeReadError> {
    #[derive(Deserialize)]
    struct Wire {
        vid: Option<serde_json::Value>,
        #[serde(rename = "accessToken")]
        access_token: Option<String>,
        #[serde(rename = "refreshToken")]
        refresh_token: Option<String>,
    }
    if let Some(error) = super::response_error(resp.status, &resp.body) {
        return Err(error);
    }
    let wire: Wire = serde_json::from_slice(&resp.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("login: {error}")))?;
    let vid = wire
        .vid
        .as_ref()
        .and_then(as_string_or_number)
        .ok_or_else(|| WeReadError::InvalidResponse("login returned no vid".to_string()))?;
    let access_token = wire
        .access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WeReadError::InvalidResponse("login returned no accessToken".to_string()))?;
    let refresh_token = wire
        .refresh_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            WeReadError::InvalidResponse("login returned no refreshToken".to_string())
        })?;
    Ok(Credentials::new(
        vid,
        access_token,
        refresh_token,
        device_id,
    ))
}

/// Parse a `POST /login` token-refresh response into updated credentials.
///
/// WeRead may omit `refreshToken` (or `vid`) when renewing an `accessToken`,
/// meaning the existing `refreshToken` remains valid.
pub fn parse_refresh_response(
    resp: &HttpResponse,
    current: &Credentials,
) -> Result<Credentials, WeReadError> {
    #[derive(Deserialize)]
    struct Wire {
        vid: Option<serde_json::Value>,
        #[serde(rename = "accessToken")]
        access_token: Option<String>,
        #[serde(rename = "refreshToken")]
        refresh_token: Option<String>,
    }
    if let Some(error) = super::response_error(resp.status, &resp.body) {
        return Err(error);
    }
    let wire: Wire = serde_json::from_slice(&resp.body)
        .map_err(|error| WeReadError::InvalidResponse(format!("refresh: {error}")))?;
    let vid = match wire.vid.as_ref().and_then(as_string_or_number) {
        Some(vid) if vid != current.vid => {
            return Err(WeReadError::InvalidResponse(
                "refresh returned a different vid".to_string(),
            ));
        }
        Some(vid) => vid,
        None => current.vid.clone(),
    };
    let access_token = wire
        .access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            WeReadError::InvalidResponse("refresh returned no accessToken".to_string())
        })?;
    let refresh_token = wire
        .refresh_token
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| current.refresh_token.clone());
    Ok(Credentials::new(
        vid,
        access_token,
        refresh_token,
        current.device_id.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_signature_matches_the_golden_vector() {
        // §14.2: sha256(timestamp + deviceId + random), fixed for a reproducible test.
        let body = build_login_body(
            "wx-code-fixture",
            "eink-test-device-0001",
            DEFAULT_DEVICE_NAME,
            "eink31000000000000000000000001",
            1_700_000_000_000,
            123,
        );
        assert_eq!(
            body.signature,
            "1fcb8d169ca57ad76bfb0e40ffa707995f6e2d6b20d3e575f698c2c0076ab71f"
        );
        assert_eq!(body.device_type, 3);
        assert_eq!(body.app_first_install, 1);
        assert_eq!(body.is_from_qrcode, 1);
    }

    #[test]
    fn qrconnect_url_encodes_the_scope_and_signature() {
        let ticket = QrTicket {
            signature: "sig+/=".to_string(),
            time_stamp: 1_700_000_000,
        };
        let url = build_qrconnect_url(&ticket);
        assert_eq!(
            url,
            "https://open.weixin.qq.com/connect/sdk/qrconnect?appid=wxab9b71ad2b90ff34&noncestr=weread&timestamp=1700000000&scope=snsapi_userinfo%2Csnsapi_timeline%2Csnsapi_friend&signature=sig%2B%2F%3D"
        );
    }

    #[test]
    fn poll_status_covers_every_contract_code() {
        let case = |body: &str| poll_status(&HttpResponse::new(200, body)).expect("status");
        assert_eq!(case(r#"{"wx_errcode":404}"#), QrPollStatus::Scanned);
        assert_eq!(
            case(r#"{"wx_errcode":405,"wx_code":"wx-code"}"#),
            QrPollStatus::Confirmed("wx-code".to_string())
        );
        assert_eq!(case(r#"{"wx_errcode":402}"#), QrPollStatus::Expired);
        assert_eq!(case(r#"{"wx_errcode":403}"#), QrPollStatus::Declined);
        assert_eq!(case(r#"{"wx_errcode":408}"#), QrPollStatus::Continue);
        assert!(poll_status(&HttpResponse::new(200, r#"{"wx_errcode":405}"#)).is_err());
        assert!(poll_status(&HttpResponse::new(200, r#"{"wx_errcode":999}"#)).is_err());
    }

    #[test]
    fn credentials_debug_never_leaks_any_field() {
        let credentials =
            Credentials::new("vid-secret", "access-secret", "refresh-secret", "dev-1");
        let debug = format!("{credentials:?}");
        for secret in ["vid-secret", "access-secret", "refresh-secret", "dev-1"] {
            assert!(!debug.contains(secret), "debug leaked {secret}: {debug}");
        }
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn login_and_refresh_requests_are_json_posts() {
        let body = build_login_body("code", "dev", "BOOX", "eink31", 1, 2);
        let request = build_login_request("https://i.weread.qq.com/", &body).expect("request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://i.weread.qq.com/login");
        let credentials = Credentials::new("1", "a", "r", "dev");
        let refresh = build_refresh_body(&credentials, 1, 2);
        assert_eq!(refresh.device_type, 3);
        assert_eq!(refresh.in_background, 0);
        assert_eq!(refresh.kick_type, 1);
    }

    #[test]
    fn parse_refresh_response_keeps_existing_refresh_token_when_omitted() {
        let current = Credentials::new("1001", "old-access", "keep-refresh", "dev-1");
        let resp = HttpResponse::new(200, r#"{"vid":1001,"accessToken":"new-access"}"#);
        let renewed = parse_refresh_response(&resp, &current).expect("renewed");
        assert_eq!(renewed.vid, "1001");
        assert_eq!(renewed.access_token, "new-access");
        assert_eq!(renewed.refresh_token, "keep-refresh");
        assert_eq!(renewed.device_id, "dev-1");

        let mismatch = HttpResponse::new(200, r#"{"vid":9999,"accessToken":"new-access"}"#);
        assert!(parse_refresh_response(&mismatch, &current).is_err());
    }
}
