// SPDX-License-Identifier: AGPL-3.0-or-later
//! Translation: Microsoft's Edge translator, MyMemory as the fallback, and
//! the reader's own OpenAI-compatible service.
//!
//! The reader translates a word, a phrase or a whole sentence, so the
//! service is asked for one piece of text at a time:
//!
//! - **Edge** ([`Backend::Edge`]) is the free translator the browser uses.
//!   It needs a token first, which the service hands out without a login;
//!   the token is kept until it expires and reused for the next text.
//! - **MyMemory** ([`Backend::MyMemory`]) is the second service: it needs
//!   no key either, and answers when Edge does not.
//! - **Custom** ([`Backend::Custom`]) is the reader's own service, an
//!   OpenAI-compatible chat endpoint with a model and, when it wants one,
//!   a key.
//!
//! [`Backend::Auto`] tries Edge first and MyMemory when Edge fails, so a
//! translation still comes when one service is down.
//!
//! Everything here blocks; the app runs it on a worker thread
//! (`crate::translate`).

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::http::{Client, Error as HttpError, Request, Timeouts};
use crate::policy::Policy;
use crate::url::{self, Url};

/// The most characters one request carries. Edge takes more, but a
/// sentence is the longest thing the reader sends, and a shorter request
/// answers faster.
pub const MAX_CHARS: usize = 1500;

/// The most characters MyMemory takes in one request.
const MYMEMORY_MAX_CHARS: usize = 500;

/// How long a token is used when it does not say when it expires.
const TOKEN_LIFETIME: Duration = Duration::from_secs(8 * 60);

/// The most bytes a service may answer with.
const MAX_BODY: u64 = 256 * 1024;

/// Which service answers a translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Edge first, MyMemory when Edge does not answer.
    Auto,
    /// Only Edge.
    Edge,
    /// Only MyMemory.
    MyMemory,
    /// The reader's own OpenAI-compatible service.
    Custom,
}

/// The reader's own translation service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Custom {
    /// The address of the service: the base of an OpenAI-compatible API,
    /// or the full `.../chat/completions` address.
    pub endpoint: String,
    /// The key, when the service wants one.
    pub api_key: String,
    /// The model to ask for.
    pub model: String,
}

/// Where the services live.
///
/// The addresses can point at a mirror, which is how the tests talk to a
/// mock server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// Where the Edge token comes from.
    pub edge_auth: String,
    /// The Edge translation API.
    pub edge_api: String,
    /// The MyMemory API.
    pub mymemory: String,
}

impl Default for Endpoints {
    fn default() -> Endpoints {
        Endpoints {
            edge_auth: "https://edge.microsoft.com/translate/auth".into(),
            edge_api: "https://api-edge.cognitive.microsofttranslator.com".into(),
            mymemory: "https://api.mymemory.translated.net".into(),
        }
    }
}

/// How a translation is made.
#[derive(Clone, Debug)]
pub struct Config {
    /// Which service answers.
    pub backend: Backend,
    /// The language to translate into. `None` picks one from the text:
    /// Chinese into English, everything else into Chinese.
    pub target: Option<String>,
    /// The reader's own service, for [`Backend::Custom`].
    pub custom: Option<Custom>,
    /// Where the services live.
    pub endpoints: Endpoints,
    /// The most characters one request carries.
    pub max_chars: usize,
    /// How long a request may take.
    pub timeouts: Timeouts,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            backend: Backend::Auto,
            target: None,
            custom: None,
            endpoints: Endpoints::default(),
            max_chars: MAX_CHARS,
            timeouts: Timeouts {
                connect: Duration::from_secs(5),
                response: Duration::from_secs(10),
                body: Duration::from_secs(10),
            },
        }
    }
}

/// Which service answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Microsoft's Edge translator.
    Edge,
    /// MyMemory.
    MyMemory,
    /// The reader's own service, with the model that answered.
    Custom(String),
}

impl Source {
    /// The name to show in the reader.
    pub fn label(&self) -> &str {
        match self {
            Source::Edge => "Edge",
            Source::MyMemory => "MyMemory",
            Source::Custom(model) => model,
        }
    }
}

/// A translation, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    /// The translated text.
    pub text: String,
    /// The service that answered.
    pub source: Source,
    /// The language the service found in the text.
    pub detected: Option<String>,
    /// The language it translated into.
    pub target: String,
}

/// Why a translation did not come.
#[derive(Debug)]
pub enum Error {
    /// There was nothing to translate.
    Empty,
    /// The service could not be reached, or refused.
    Http(HttpError),
    /// The service answered with something we cannot read.
    BadAnswer(String),
    /// Every service failed; their messages, in the order they were tried.
    AllFailed(Vec<String>),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Empty => f.write_str("there is nothing to translate"),
            Error::Http(e) => write!(f, "{e}"),
            Error::BadAnswer(d) => write!(f, "the service answered with something odd: {d}"),
            Error::AllFailed(list) => {
                for (i, m) in list.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    f.write_str(m)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<HttpError> for Error {
    fn from(e: HttpError) -> Error {
        Error::Http(e)
    }
}

/// The translator: the client, the settings, and the Edge token.
pub struct Translator {
    client: Client,
    config: Config,
    /// The Edge token, until it expires.
    token: Mutex<Option<Token>>,
}

struct Token {
    value: String,
    until: Instant,
}

impl Translator {
    /// A translator that reaches the services the policy allows.
    pub fn new(policy: Policy, config: Config) -> Translator {
        let client = Client::new(policy, config.timeouts);
        Translator {
            client,
            config,
            token: Mutex::new(None),
        }
    }

    /// The settings this translator works with.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The language a text is written in, as far as its letters tell:
    /// Chinese when at least half of them are Han characters, English
    /// otherwise.
    pub fn detect_language(text: &str) -> &'static str {
        let mut han = 0usize;
        let mut letters = 0usize;
        for c in text.chars() {
            if !c.is_alphabetic() {
                continue;
            }
            letters += 1;
            if is_han(c) {
                han += 1;
            }
        }
        if han > 0 && han * 2 >= letters {
            "zh-Hans"
        } else {
            "en"
        }
    }

    /// The language a text is translated into: the one the reader chose,
    /// or the one that fits the text (Chinese into English, everything
    /// else into Chinese).
    pub fn target_language(&self, text: &str) -> String {
        match &self.config.target {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                if Self::detect_language(text) == "zh-Hans" {
                    "en".into()
                } else {
                    "zh-Hans".into()
                }
            }
        }
    }

    /// Translate `text` into the target language.
    pub fn translate(&self, text: &str) -> Result<Translation, Error> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Empty);
        }
        let text = truncate(text, self.config.max_chars);
        let target = self.target_language(&text);
        match self.config.backend {
            Backend::Edge => self.edge(&text, &target),
            Backend::MyMemory => self.mymemory(&text, &target),
            Backend::Custom => self.custom(&text, &target),
            Backend::Auto => match self.edge(&text, &target) {
                Ok(t) => Ok(t),
                Err(first) => match self.mymemory(&text, &target) {
                    Ok(t) => Ok(t),
                    Err(second) => Err(Error::AllFailed(vec![
                        first.to_string(),
                        second.to_string(),
                    ])),
                },
            },
        }
    }

    /// The Edge translator: a token first, then the text.
    fn edge(&self, text: &str, target: &str) -> Result<Translation, Error> {
        let token = self.edge_token()?;
        let url = format!(
            "{}/translate?from=&to={}&api-version=3.0",
            self.config.endpoints.edge_api.trim_end_matches('/'),
            url::encode_component(target),
        );
        let url = Url::parse(&url).map_err(|e| Error::BadAnswer(e.to_string()))?;
        let body = serde_json::to_string(&[serde_json::json!({ "Text": text })])
            .unwrap_or_else(|_| "[]".to_string());
        let req = Request::post(url, body.into_bytes())
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json; charset=UTF-8");
        let resp = self.client.send(&req, MAX_BODY)?;
        if resp.status != 200 {
            return Err(Error::Http(HttpError::Status(
                resp.status,
                cut(&resp.text()),
            )));
        }
        let value: serde_json::Value =
            serde_json::from_str(&resp.text()).map_err(|e| Error::BadAnswer(e.to_string()))?;
        let Some((text, detected, to)) = edge_answer(&value) else {
            return Err(Error::BadAnswer("no translation in the answer".into()));
        };
        Ok(Translation {
            text,
            source: Source::Edge,
            detected,
            target: if to.is_empty() {
                target.to_string()
            } else {
                to
            },
        })
    }

    /// The Edge token: fetched when there is none, and reused until it
    /// expires.
    fn edge_token(&self) -> Result<String, Error> {
        let mut slot = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = slot.as_ref() {
            if t.until > Instant::now() {
                return Ok(t.value.clone());
            }
        }
        let url = Url::parse(&self.config.endpoints.edge_auth)
            .map_err(|e| Error::BadAnswer(e.to_string()))?;
        let resp = self.client.send(&Request::get(url), MAX_BODY)?;
        if resp.status != 200 {
            return Err(Error::Http(HttpError::Status(
                resp.status,
                cut(&resp.text()),
            )));
        }
        let value = resp.text().trim().to_string();
        if value.is_empty() {
            return Err(Error::BadAnswer("the token was empty".into()));
        }
        let until = token_expiry(&value).unwrap_or_else(|| Instant::now() + TOKEN_LIFETIME);
        *slot = Some(Token {
            value: value.clone(),
            until,
        });
        Ok(value)
    }

    /// MyMemory: no key, and it names the language it found.
    fn mymemory(&self, text: &str, target: &str) -> Result<Translation, Error> {
        let text = truncate(text, MYMEMORY_MAX_CHARS.min(self.config.max_chars));
        // The source is the one the text is in; "Autodetect" lets the
        // service find it, which is the only way for a language we cannot
        // name (a Polish book, say).
        let source = if Self::detect_language(&text) == "zh-Hans" {
            "zh-Hans"
        } else {
            "Autodetect"
        };
        let url = format!(
            "{}/get?q={}&langpair={}|{}",
            self.config.endpoints.mymemory.trim_end_matches('/'),
            url::encode_component(&text),
            url::encode_component(source),
            url::encode_component(target),
        );
        let url = Url::parse(&url).map_err(|e| Error::BadAnswer(e.to_string()))?;
        let resp = self.client.send(&Request::get(url), MAX_BODY)?;
        if resp.status != 200 {
            return Err(Error::Http(HttpError::Status(
                resp.status,
                cut(&resp.text()),
            )));
        }
        let value: serde_json::Value =
            serde_json::from_str(&resp.text()).map_err(|e| Error::BadAnswer(e.to_string()))?;
        let (text, detected) = mymemory_answer(&value).map_err(Error::BadAnswer)?;
        Ok(Translation {
            text,
            source: Source::MyMemory,
            detected,
            target: target.to_string(),
        })
    }

    /// The reader's own service: an OpenAI-compatible chat endpoint.
    fn custom(&self, text: &str, target: &str) -> Result<Translation, Error> {
        let Some(custom) = self.config.custom.as_ref() else {
            return Err(Error::BadAnswer("no custom service is set".into()));
        };
        let url = chat_url(&custom.endpoint);
        let url = Url::parse(&url).map_err(|e| Error::BadAnswer(e.to_string()))?;
        let source = Self::detect_language(text);
        let prompt = format!(
            "Translate the text between the markers into {target}. \
             Answer with the translation only, without the markers. \
             <text>{text}</text>"
        );
        let body = serde_json::json!({
            "model": custom.model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.2,
        });
        let body = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
        let mut req = Request::post(url, body.into_bytes())
            .header("Content-Type", "application/json; charset=UTF-8");
        if !custom.api_key.trim().is_empty() {
            req = req.header(
                "Authorization",
                &format!("Bearer {}", custom.api_key.trim()),
            );
        }
        let resp = self.client.send(&req, MAX_BODY)?;
        if resp.status != 200 {
            return Err(Error::Http(HttpError::Status(
                resp.status,
                cut(&resp.text()),
            )));
        }
        let value: serde_json::Value =
            serde_json::from_str(&resp.text()).map_err(|e| Error::BadAnswer(e.to_string()))?;
        let Some(text) = chat_answer(&value) else {
            return Err(Error::BadAnswer("no answer in the reply".into()));
        };
        Ok(Translation {
            text,
            source: Source::Custom(custom.model.clone()),
            detected: Some(source.to_string()),
            target: target.to_string(),
        })
    }
}

/// The sentence a selection stands in: the pieces from the one after the
/// last sentence end before it to the one with the next sentence end.
///
/// The ends are the Chinese and English marks (`。！？.!?`) and the line
/// break. A piece that holds a mark in the middle is kept whole, so the
/// sentence may begin a little early or end a little late, but no word is
/// cut in half.
pub fn sentence_span(pieces: &[&str], start: usize, end: usize) -> (usize, usize) {
    if pieces.is_empty() {
        return (0, 0);
    }
    let start = start.min(pieces.len() - 1);
    let end = end.min(pieces.len() - 1).max(start);
    // The end of the sentence: the first piece at or after the selection
    // that ends one.
    let mut last = pieces.len() - 1;
    for (i, piece) in pieces.iter().enumerate().skip(end) {
        if ends_sentence(piece) || has_terminator(piece) {
            last = i;
            break;
        }
    }
    // Its start: the piece after the sentence end before it. The piece
    // that ends the sentence is skipped: its own mark ends this sentence,
    // it does not start the next one. A selection that is the last word
    // of a sentence therefore keeps that sentence.
    let mut first = 0;
    for (i, piece) in pieces.iter().enumerate().take(last + 1).rev() {
        if ends_sentence(piece) {
            if i != last {
                first = i + 1;
                break;
            }
            continue;
        }
        if has_terminator(piece) {
            if i != last {
                first = i;
                break;
            }
            continue;
        }
    }
    (first, last)
}

/// Whether the piece ends a sentence.
fn ends_sentence(piece: &str) -> bool {
    let t = piece.trim_end_matches([' ', '\t', '\r']);
    let Some(last) = t.chars().last() else {
        return false;
    };
    match last {
        '\n' | '。' | '！' | '？' | '!' | '?' => true,
        '.' => {
            // A dot between digits ("3.14") is not a sentence end.
            let before = t[..t.len() - last.len_utf8()].chars().last();
            !matches!(before, Some(c) if c.is_ascii_digit())
        }
        _ => false,
    }
}

/// Whether the piece holds a sentence end anywhere.
fn has_terminator(piece: &str) -> bool {
    let chars: Vec<char> = piece.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '。' | '！' | '？' | '!' | '?' | '\n' => return true,
            '.' => {
                let before = i.checked_sub(1).map(|j| chars[j]);
                let after = chars.get(i + 1).copied();
                let between_digits = matches!(before, Some(b) if b.is_ascii_digit())
                    && matches!(after, Some(a) if a.is_ascii_digit());
                if !between_digits {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// A Han character: the Chinese, Japanese and Korean ideographs.
fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF)
}

/// The text of an Edge answer: the translation, the language it found and
/// the language it went into.
fn edge_answer(value: &serde_json::Value) -> Option<(String, Option<String>, String)> {
    let item = value.as_array()?.first()?;
    let translation = item.get("translations")?.as_array()?.first()?;
    let text = translation.get("text")?.as_str()?.to_string();
    let to = translation
        .get("to")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let detected = item
        .get("detectedLanguage")
        .and_then(|d| d.get("language"))
        .and_then(|l| l.as_str())
        .map(str::to_string);
    Some((text, detected, to))
}

/// The text of a MyMemory answer: the translation and the language it
/// found.
fn mymemory_answer(value: &serde_json::Value) -> Result<(String, Option<String>), String> {
    let status = value.get("responseStatus");
    let ok = match status {
        Some(v) => v.as_i64() == Some(200) || v.as_str() == Some("200"),
        None => false,
    };
    let detail = || {
        value
            .get("responseDetails")
            .and_then(|d| d.as_str())
            .filter(|d| !d.trim().is_empty())
            .unwrap_or("the service did not translate the text")
            .to_string()
    };
    if !ok {
        return Err(detail());
    }
    let data = value.get("responseData");
    let text = data
        .and_then(|d| d.get("translatedText"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err(detail());
    }
    let detected = data
        .and_then(|d| d.get("detectedLanguage"))
        .and_then(|d| d.as_str())
        .map(str::to_string);
    Ok((decode_entities(text), detected))
}

/// The text of an OpenAI-compatible answer.
fn chat_answer(value: &serde_json::Value) -> Option<String> {
    let choice = value.get("choices")?.as_array()?.first()?;
    let content = choice.get("message")?.get("content")?.as_str()?;
    let content = content.trim();
    if content.is_empty() {
        return None;
    }
    Some(content.to_string())
}

/// The address of the chat endpoint of an OpenAI-compatible service.
fn chat_url(endpoint: &str) -> String {
    let base = endpoint.trim().trim_end_matches('/');
    if base.contains("chat/completions") {
        return base.to_string();
    }
    format!("{base}/chat/completions")
}

/// When a token expires, if it says so.
fn token_expiry(token: &str) -> Option<Instant> {
    let payload = token.split('.').nth(1)?;
    let json = String::from_utf8(base64url_decode(payload)?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    let exp = value.get("exp")?.as_i64()?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let left = exp - now;
    Some(Instant::now() + Duration::from_secs(left.max(0) as u64))
}

/// Decodes base64url, the alphabet a token's payload uses.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = value(c)?;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The HTML entities a service may leave in the text.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|e| *e <= 8) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            other => other
                .strip_prefix('#')
                .and_then(|n| n.parse::<u32>().ok())
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Cuts a text to `max` characters, at a word boundary when there is one.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    // A word boundary in the last fifth of the text: a word broken in
    // half reads worse than a few characters less.
    let floor = max * 4 / 5;
    let cut = head
        .char_indices()
        .rev()
        .find(|(i, c)| *i >= floor && c.is_whitespace())
        .map(|(i, _)| i);
    match cut {
        Some(i) => head[..i].trim_end().to_string(),
        None => head.trim_end().to_string(),
    }
}

/// The start of a message that goes into a status line.
fn cut(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= 120 {
        return t.to_string();
    }
    let head: String = t.chars().take(120).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, Server};

    /// A server that answers the Edge token and the Edge translation.
    fn edge_server() -> Server {
        Server::start(|r| {
            if r.path.starts_with("/auth") {
                return Reply::text(200, "TOKEN-1");
            }
            Reply::typed(
                200,
                "application/json",
                br#"[{"translations":[{"text":"Hello world","to":"en"}],"detectedLanguage":{"language":"zh-Hans","score":1.0}}]"#,
            )
        })
    }

    fn config(server: &Server) -> Config {
        let mut c = Config {
            backend: Backend::Edge,
            ..Config::default()
        };
        c.endpoints.edge_auth = server.url("/auth").to_string();
        c.endpoints.edge_api = server.url("/").to_string();
        c
    }

    #[test]
    fn edge_translates_and_keeps_its_token() {
        let server = edge_server();
        let t = Translator::new(Policy::open(), config(&server));
        let first = t.translate("你好世界").unwrap();
        assert_eq!(first.text, "Hello world");
        assert_eq!(first.source, Source::Edge);
        assert_eq!(first.detected.as_deref(), Some("zh-Hans"));
        assert_eq!(first.target, "en");
        // The second text uses the token that is already there.
        let second = t.translate("再见").unwrap();
        assert_eq!(second.text, "Hello world");
        assert_eq!(server.hits("/auth"), 1);
        assert_eq!(server.hits("/translate?from=&to=en&api-version=3.0"), 2);
    }

    #[test]
    fn the_token_is_sent_and_the_text_asked_for() {
        let server = edge_server();
        let t = Translator::new(Policy::open(), config(&server));
        t.translate("你好").unwrap();
        let requests = server.requests.lock().unwrap();
        let req = &requests[1];
        assert_eq!(req.method, "POST");
        assert_eq!(req.header("authorization"), Some("Bearer TOKEN-1"));
        let body = String::from_utf8_lossy(&req.body);
        assert!(body.contains("你好"), "{body}");
    }

    #[test]
    fn a_failing_edge_falls_back_to_mymemory() {
        let server = Server::start(|r| {
            if r.path.starts_with("/auth") {
                return Reply::text(500, "no token today");
            }
            if r.path.starts_with("/get") {
                return Reply::typed(
                    200,
                    "application/json",
                    br#"{"responseData":{"translatedText":"Hello world","detectedLanguage":"zh-Hans"},"responseStatus":200}"#,
                );
            }
            Reply::text(404, "")
        });
        let mut c = config(&server);
        c.backend = Backend::Auto;
        c.endpoints.mymemory = server.url("/").to_string();
        let t = Translator::new(Policy::open(), c);
        let out = t.translate("你好世界").unwrap();
        assert_eq!(out.text, "Hello world");
        assert_eq!(out.source, Source::MyMemory);
        assert_eq!(out.detected.as_deref(), Some("zh-Hans"));
    }

    #[test]
    fn both_services_failing_says_why() {
        let server = Server::start(|_| Reply::text(500, "down"));
        let mut c = config(&server);
        c.backend = Backend::Auto;
        c.endpoints.mymemory = server.url("/").to_string();
        let t = Translator::new(Policy::open(), c);
        let err = t.translate("hello").unwrap_err();
        match err {
            Error::AllFailed(list) => assert_eq!(list.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mymemory_asks_with_the_language_it_found() {
        let server = Server::start(|r| {
            if r.path.starts_with("/get") {
                return Reply::typed(
                    200,
                    "application/json",
                    br#"{"responseData":{"translatedText":"Hello","detectedLanguage":"zh-Hans"},"responseStatus":200}"#,
                );
            }
            Reply::text(404, "")
        });
        let mut c = Config {
            backend: Backend::MyMemory,
            ..Config::default()
        };
        c.endpoints.mymemory = server.url("/").to_string();
        let t = Translator::new(Policy::open(), c);
        let out = t.translate("你好").unwrap();
        assert_eq!(out.source, Source::MyMemory);
        let requests = server.requests.lock().unwrap();
        let path = &requests[0].path;
        assert!(path.contains("langpair=zh-Hans%7Cen"), "{path}");
    }

    #[test]
    fn a_custom_service_answers_with_its_model() {
        let server = Server::start(|r| {
            if r.path.starts_with("/v1/chat/completions") {
                return Reply::typed(
                    200,
                    "application/json",
                    "{\"choices\":[{\"message\":{\"content\":\"你好世界\"}}]}".as_bytes(),
                );
            }
            Reply::text(404, "")
        });
        let mut c = Config {
            backend: Backend::Custom,
            ..Config::default()
        };
        c.custom = Some(Custom {
            endpoint: server.url("/v1").to_string(),
            api_key: "secret".into(),
            model: "my-model".into(),
        });
        let t = Translator::new(Policy::open(), c);
        let out = t.translate("hello world").unwrap();
        assert_eq!(out.text, "你好世界");
        assert_eq!(out.source, Source::Custom("my-model".into()));
        let requests = server.requests.lock().unwrap();
        let req = &requests[0];
        assert_eq!(req.header("authorization"), Some("Bearer secret"));
        let body = String::from_utf8_lossy(&req.body);
        assert!(body.contains("my-model"), "{body}");
        assert!(body.contains("hello world"), "{body}");
    }

    #[test]
    fn nothing_to_translate_asks_nothing() {
        let server = edge_server();
        let t = Translator::new(Policy::open(), config(&server));
        assert!(matches!(t.translate("   "), Err(Error::Empty)));
        assert_eq!(server.hits("/auth"), 0);
    }

    #[test]
    fn a_long_text_is_cut_at_a_word() {
        let server = edge_server();
        let mut c = config(&server);
        c.max_chars = 20;
        let t = Translator::new(Policy::open(), c);
        t.translate("the quick brown fox jumps over the lazy dog")
            .unwrap();
        let requests = server.requests.lock().unwrap();
        let body = String::from_utf8_lossy(&requests[1].body);
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
        let text = sent[0]["Text"].as_str().unwrap();
        assert!(text.chars().count() <= 20, "{text:?}");
        assert!(!text.ends_with(' '), "{text:?}");
    }

    #[test]
    fn the_target_language_follows_the_text() {
        let server = edge_server();
        let t = Translator::new(Policy::open(), config(&server));
        assert_eq!(t.target_language("你好世界"), "en");
        assert_eq!(t.target_language("hello world"), "zh-Hans");
        assert_eq!(t.target_language("Zażółć gęślą jaźń"), "zh-Hans");
    }

    #[test]
    fn a_chosen_target_language_wins() {
        let server = edge_server();
        let mut c = config(&server);
        c.target = Some("pl".into());
        let t = Translator::new(Policy::open(), c);
        assert_eq!(t.target_language("你好世界"), "pl");
    }

    #[test]
    fn a_sentence_is_the_one_the_selection_stands_in() {
        let pieces = ["Hello", "world.", "How", "are", "you?"];
        assert_eq!(sentence_span(&pieces, 0, 1), (0, 1));
        assert_eq!(sentence_span(&pieces, 2, 2), (2, 4));
        assert_eq!(sentence_span(&pieces, 3, 3), (2, 4));
    }

    #[test]
    fn a_chinese_sentence_stops_at_its_marks() {
        let pieces = ["你", "好", "。", "世", "界", "！", "再", "见"];
        assert_eq!(sentence_span(&pieces, 3, 4), (3, 5));
        assert_eq!(sentence_span(&pieces, 6, 7), (6, 7));
    }

    #[test]
    fn a_word_that_ends_a_sentence_keeps_that_sentence() {
        // The selection is the last word of the sentence: the sentence it
        // stands in is the one before the mark, not the one after it.
        let pieces = ["Hello", "world."];
        assert_eq!(sentence_span(&pieces, 1, 1), (0, 1));
        let pieces = ["A.", "B", "C."];
        assert_eq!(sentence_span(&pieces, 2, 2), (1, 2));
        assert_eq!(sentence_span(&pieces, 1, 1), (1, 2));
    }

    #[test]
    fn a_sentence_is_never_backwards() {
        let pieces = ["One.", "Two.", "Three"];
        for i in 0..pieces.len() {
            let (first, last) = sentence_span(&pieces, i, i);
            assert!(first <= last, "{first} > {last} at {i}");
        }
    }

    #[test]
    fn mymemory_refuses_a_request_it_did_not_translate() {
        let server = Server::start(|_| {
            Reply::typed(
                200,
                "application/json",
                br#"{"responseData":{"translatedText":"PLEASE SELECT TWO DISTINCT LANGUAGES"},"responseStatus":"403","responseDetails":"PLEASE SELECT TWO DISTINCT LANGUAGES"}"#,
            )
        });
        let mut c = Config {
            backend: Backend::MyMemory,
            ..Config::default()
        };
        c.endpoints.mymemory = server.url("/").to_string();
        let t = Translator::new(Policy::open(), c);
        // The service answers with 200 and refuses in the body: the
        // refusal must not be shown as a translation.
        let err = t.translate("hello").unwrap_err();
        match err {
            Error::BadAnswer(detail) => assert!(detail.contains("DISTINCT"), "{detail}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_sentence_without_an_end_runs_to_the_page() {
        let pieces = ["One", "two", "three"];
        assert_eq!(sentence_span(&pieces, 1, 1), (0, 2));
    }

    #[test]
    fn a_dot_between_digits_is_not_an_end() {
        let pieces = ["Version", "3.14", "is", "here."];
        assert_eq!(sentence_span(&pieces, 2, 2), (0, 3));
    }

    #[test]
    fn a_token_says_when_it_expires() {
        // {"exp":4102444800} in base64url.
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjQxMDI0NDQ4MDB9.sig";
        let until = token_expiry(token).unwrap();
        assert!(until > Instant::now() + Duration::from_secs(3600));
    }

    #[test]
    fn a_token_without_an_expiry_is_used_for_a_few_minutes() {
        assert!(token_expiry("not-a-token").is_none());
    }
}
