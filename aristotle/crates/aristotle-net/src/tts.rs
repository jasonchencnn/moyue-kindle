// SPDX-License-Identifier: AGPL-3.0-or-later
//! Online AI Text-to-Speech (TTS) client for Bluetooth audiobook reading.
//!
//! Supports the three API families most commonly used by modern reading apps
//! (Legado, Readest, KOReader TTS plugins, MultiTTS):
//!
//! - [`Provider::OpenAiCompatible`]: `POST {base}/v1/audio/speech` — covers
//!   OpenAI (`gpt-4o-mini-tts` / `tts-1`), SiliconFlow (`FunAudioLLM/CosyVoice2-0.5B`,
//!   `fishaudio/fish-speech-1.5`), Fish Audio, One-API, and Edge-TTS-OpenAI proxies.
//! - [`Provider::Volcengine`]: ByteDance / Volcengine Doubao Speech HTTP V1
//!   (`POST https://openspeech.bytedance.com/api/v1/tts`), returning base64 PCM.
//! - [`Provider::MiniMax`]: MiniMax T2A v2 (`POST https://api.minimaxi.com/v1/t2a_v2`),
//!   returning hex-encoded PCM.
//!
//! All providers request raw 16-bit signed little-endian PCM (24 kHz mono by
//! default) so the Kindle client never needs heavy audio codecs. If a server
//! responds with a RIFF/WAVE container instead of raw PCM, the WAV header is
//! parsed and stripped automatically.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::http::{Client, Error as HttpError, Request, Timeouts};
use crate::policy::Policy;
use crate::url::Url;

/// Maximum audio response size per chunk (8 MiB, ~2.8 minutes of 24 kHz mono PCM).
const MAX_AUDIO_BODY: u64 = 8 * 1024 * 1024;

/// Default sample rate requested from online TTS models (24 kHz).
pub const DEFAULT_SAMPLE_RATE: u32 = 24_000;

/// Default target character length per spoken chunk.
pub const DEFAULT_CHUNK_CHARS: usize = 160;

/// Supported online TTS API families.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// OpenAI-compatible `POST .../v1/audio/speech` (SiliconFlow CosyVoice2 /
    /// FishSpeech, OpenAI, One-API, Edge-TTS proxy).
    #[default]
    OpenAiCompatible,
    /// Volcengine / ByteDance Doubao Speech HTTP V1 (`openspeech.bytedance.com`).
    Volcengine,
    /// MiniMax T2A v2 (`api.minimaxi.com/v1/t2a_v2`).
    MiniMax,
}

impl Provider {
    /// Default API endpoint for this provider.
    pub fn default_endpoint(self) -> &'static str {
        match self {
            Provider::OpenAiCompatible => "https://api.siliconflow.cn/v1/audio/speech",
            Provider::Volcengine => "https://openspeech.bytedance.com/api/v1/tts",
            Provider::MiniMax => "https://api.minimaxi.com/v1/t2a_v2",
        }
    }

    /// Default model name for this provider.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::OpenAiCompatible => "FunAudioLLM/CosyVoice2-0.5B",
            Provider::Volcengine => "volcano_tts",
            Provider::MiniMax => "speech-02-turbo",
        }
    }

    /// Default voice identifier for this provider.
    pub fn default_voice(self) -> &'static str {
        match self {
            Provider::OpenAiCompatible => "FunAudioLLM/CosyVoice2-0.5B:alex",
            Provider::Volcengine => "BV700_V2_streaming",
            Provider::MiniMax => "male-qn-qingse",
        }
    }
}

/// Configuration for an online TTS provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// API protocol family.
    pub provider: Provider,
    /// Full endpoint URL or base URL. Empty uses [`Provider::default_endpoint`].
    pub endpoint: String,
    /// Bearer API key / access token.
    pub api_key: String,
    /// Volcengine `appid` (unused for OpenAI-compatible / MiniMax).
    pub app_id: String,
    /// Volcengine `cluster` (defaults to `"volcano_tts"`).
    pub cluster: String,
    /// Model name (e.g. `FunAudioLLM/CosyVoice2-0.5B`, `gpt-4o-mini-tts`, `speech-02-turbo`).
    pub model: String,
    /// Voice ID / speaker name.
    pub voice: String,
    /// Playback speech rate (`0.5..=2.0`, default `1.0`).
    pub speed: f32,
    /// Sample rate in Hz (default `24000`).
    pub sample_rate: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: Provider::OpenAiCompatible,
            endpoint: Provider::OpenAiCompatible.default_endpoint().to_string(),
            api_key: String::new(),
            app_id: String::new(),
            cluster: "volcano_tts".to_string(),
            model: Provider::OpenAiCompatible.default_model().to_string(),
            voice: Provider::OpenAiCompatible.default_voice().to_string(),
            speed: 1.0,
            sample_rate: DEFAULT_SAMPLE_RATE,
        }
    }
}

impl Config {
    /// Effective endpoint URL, resolving OpenAI base URLs to `/v1/audio/speech`.
    pub fn resolved_endpoint(&self) -> String {
        let raw = self.endpoint.trim();
        if raw.is_empty() {
            return self.provider.default_endpoint().to_string();
        }
        if self.provider == Provider::OpenAiCompatible {
            let trimmed = raw.trim_end_matches('/');
            if trimmed.ends_with("/audio/speech") {
                trimmed.to_string()
            } else if trimmed.ends_with("/v1") {
                format!("{trimmed}/audio/speech")
            } else {
                format!("{trimmed}/v1/audio/speech")
            }
        } else {
            raw.to_string()
        }
    }

    /// Host and port (`host:port`) needed for the device firewall allowlist.
    pub fn allowlist_host(&self) -> Option<String> {
        let url = Url::parse(&self.resolved_endpoint()).ok()?;
        Some(url.host_port())
    }

    /// Clamped playback speed in `[0.5, 2.0]`.
    pub fn clamped_speed(&self) -> f32 {
        if self.speed.is_finite() {
            self.speed.clamp(0.5, 2.0)
        } else {
            1.0
        }
    }
}

/// Synthesized audio buffer in raw 16-bit signed little-endian PCM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcmAudio {
    /// Sample rate in Hz (e.g. `24000`).
    pub sample_rate: u32,
    /// Channel count (`1` = mono, `2` = stereo).
    pub channels: u16,
    /// Raw 16-bit signed little-endian PCM samples.
    pub samples: Vec<u8>,
}

/// Errors returned by [`TtsClient::synthesize`].
#[derive(Debug)]
pub enum TtsError {
    /// Configuration is incomplete (e.g. missing API key or invalid URL).
    Config(String),
    /// Network or HTTP failure.
    Http(HttpError),
    /// Provider returned a malformed or error payload.
    Api(String),
}

impl fmt::Display for TtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TtsError::Config(msg) => write!(f, "TTS 配置无效: {msg}"),
            TtsError::Http(err) => write!(f, "TTS 网络错误: {err}"),
            TtsError::Api(msg) => write!(f, "TTS 服务错误: {msg}"),
        }
    }
}

impl std::error::Error for TtsError {}

impl From<HttpError> for TtsError {
    fn from(value: HttpError) -> Self {
        TtsError::Http(value)
    }
}

/// Blocking TTS client that respects the device's [`Policy`] allowlist.
pub struct TtsClient {
    http: Client,
    config: Config,
}

impl TtsClient {
    /// Create a new TTS client with the given allowlist policy and config.
    pub fn new(policy: Policy, config: Config) -> Self {
        let timeouts = Timeouts {
            connect: Duration::from_secs(8),
            response: Duration::from_secs(30),
            body: Duration::from_secs(45),
        };
        Self {
            http: Client::new(policy, timeouts),
            config,
        }
    }

    /// Synthesize one chunk of text into raw 16-bit signed LE PCM.
    pub fn synthesize(&self, text: &str) -> Result<PcmAudio, TtsError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(PcmAudio {
                sample_rate: self.effective_rate(),
                channels: 1,
                samples: Vec::new(),
            });
        }
        match self.config.provider {
            Provider::OpenAiCompatible => self.synthesize_openai(trimmed),
            Provider::Volcengine => self.synthesize_volcengine(trimmed),
            Provider::MiniMax => self.synthesize_minimax(trimmed),
        }
    }

    fn effective_rate(&self) -> u32 {
        if self.config.sample_rate == 0 {
            DEFAULT_SAMPLE_RATE
        } else {
            self.config.sample_rate
        }
    }

    fn synthesize_openai(&self, text: &str) -> Result<PcmAudio, TtsError> {
        let url = Url::parse(&self.config.resolved_endpoint())
            .map_err(|e| TtsError::Config(e.to_string()))?;
        let model = if self.config.model.trim().is_empty() {
            Provider::OpenAiCompatible.default_model()
        } else {
            self.config.model.trim()
        };
        let voice = if self.config.voice.trim().is_empty() {
            Provider::OpenAiCompatible.default_voice()
        } else {
            self.config.voice.trim()
        };
        let body = serde_json::json!({
            "model": model,
            "input": text,
            "voice": voice,
            "response_format": "pcm",
            "sample_rate": self.effective_rate(),
            "speed": self.config.clamped_speed(),
        });
        let payload = serde_json::to_vec(&body).map_err(|e| TtsError::Api(e.to_string()))?;
        let mut req = Request::post(url, payload).header("Content-Type", "application/json");
        let key = self.config.api_key.trim();
        if !key.is_empty() {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let resp = self.http.send(&req, MAX_AUDIO_BODY)?;
        if !(200..300).contains(&resp.status) {
            return Err(TtsError::Api(format!(
                "HTTP {}: {}",
                resp.status,
                truncate_utf8(&resp.body, 200)
            )));
        }
        Ok(parse_pcm_or_wav(&resp.body, self.effective_rate()))
    }

    fn synthesize_volcengine(&self, text: &str) -> Result<PcmAudio, TtsError> {
        let app_id = self.config.app_id.trim();
        let token = self.config.api_key.trim();
        if app_id.is_empty() || token.is_empty() {
            return Err(TtsError::Config(
                "火山引擎需要配置 app_id 和 api_key (token)".into(),
            ));
        }
        let url = Url::parse(&self.config.resolved_endpoint())
            .map_err(|e| TtsError::Config(e.to_string()))?;
        let cluster = if self.config.cluster.trim().is_empty() {
            "volcano_tts"
        } else {
            self.config.cluster.trim()
        };
        let voice = if self.config.voice.trim().is_empty() {
            Provider::Volcengine.default_voice()
        } else {
            self.config.voice.trim()
        };
        let reqid = make_req_id();
        let body = serde_json::json!({
            "app": {
                "appid": app_id,
                "token": token,
                "cluster": cluster,
            },
            "user": {
                "uid": "moyue-kindle",
            },
            "audio": {
                "voice_type": voice,
                "encoding": "pcm",
                "rate": self.effective_rate(),
                "speed_ratio": self.config.clamped_speed(),
            },
            "request": {
                "reqid": reqid,
                "text": text,
                "text_type": "plain",
                "operation": "query",
            },
        });
        let payload = serde_json::to_vec(&body).map_err(|e| TtsError::Api(e.to_string()))?;
        let req = Request::post(url, payload)
            .header("Content-Type", "application/json")
            .header("Authorization", &format!("Bearer;{token}"));
        let resp = self.http.send(&req, MAX_AUDIO_BODY)?;
        if !(200..300).contains(&resp.status) {
            return Err(TtsError::Api(format!(
                "HTTP {}: {}",
                resp.status,
                truncate_utf8(&resp.body, 200)
            )));
        }
        let v: serde_json::Value =
            serde_json::from_slice(&resp.body).map_err(|e| TtsError::Api(e.to_string()))?;
        let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 3000 {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(TtsError::Api(format!("code {code}: {msg}")));
        }
        let b64 = v
            .get("data")
            .and_then(|d| d.as_str())
            .ok_or_else(|| TtsError::Api("缺少 data 音频字段".into()))?;
        let raw = decode_base64(b64).map_err(|e| TtsError::Api(format!("base64 解码失败: {e}")))?;
        Ok(parse_pcm_or_wav(&raw, self.effective_rate()))
    }

    fn synthesize_minimax(&self, text: &str) -> Result<PcmAudio, TtsError> {
        let key = self.config.api_key.trim();
        if key.is_empty() {
            return Err(TtsError::Config("MiniMax 需要配置 api_key".into()));
        }
        let url = Url::parse(&self.config.resolved_endpoint())
            .map_err(|e| TtsError::Config(e.to_string()))?;
        let model = if self.config.model.trim().is_empty() {
            Provider::MiniMax.default_model()
        } else {
            self.config.model.trim()
        };
        let voice = if self.config.voice.trim().is_empty() {
            Provider::MiniMax.default_voice()
        } else {
            self.config.voice.trim()
        };
        let body = serde_json::json!({
            "model": model,
            "text": text,
            "stream": false,
            "voice_setting": {
                "voice_id": voice,
                "speed": self.config.clamped_speed(),
            },
            "audio_setting": {
                "sample_rate": self.effective_rate(),
                "format": "pcm",
                "channel": 1,
            },
        });
        let payload = serde_json::to_vec(&body).map_err(|e| TtsError::Api(e.to_string()))?;
        let req = Request::post(url, payload)
            .header("Content-Type", "application/json")
            .header("Authorization", &format!("Bearer {key}"));
        let resp = self.http.send(&req, MAX_AUDIO_BODY)?;
        if !(200..300).contains(&resp.status) {
            return Err(TtsError::Api(format!(
                "HTTP {}: {}",
                resp.status,
                truncate_utf8(&resp.body, 200)
            )));
        }
        let v: serde_json::Value =
            serde_json::from_slice(&resp.body).map_err(|e| TtsError::Api(e.to_string()))?;
        let status_code = v
            .get("base_resp")
            .and_then(|b| b.get("status_code"))
            .and_then(|c| c.as_i64())
            .unwrap_or(0);
        if status_code != 0 {
            let msg = v
                .get("base_resp")
                .and_then(|b| b.get("status_msg"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(TtsError::Api(format!("status {status_code}: {msg}")));
        }
        let hex_audio = v
            .get("data")
            .and_then(|d| d.get("audio"))
            .and_then(|a| a.as_str())
            .ok_or_else(|| TtsError::Api("缺少 data.audio 字段".into()))?;
        let raw = decode_hex(hex_audio).map_err(|e| TtsError::Api(format!("hex 解码失败: {e}")))?;
        Ok(parse_pcm_or_wav(&raw, self.effective_rate()))
    }
}

/// Split page text into natural spoken chunks (preferring sentence boundaries)
/// of at most `max_chars` Unicode scalar values.
pub fn split_into_chunks(text: &str, max_chars: usize) -> Vec<String> {
    let limit = max_chars.max(20);
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect();

    let mut sentences = Vec::new();
    let mut cur = String::new();
    for ch in cleaned.chars() {
        if ch == '\n' {
            let s = cur.trim();
            if !s.is_empty() {
                sentences.push(s.to_string());
            }
            cur.clear();
            continue;
        }
        cur.push(ch);
        if matches!(ch, '。' | '！' | '？' | '；' | '!' | '?' | ';') {
            let s = cur.trim();
            if !s.is_empty() {
                sentences.push(s.to_string());
            }
            cur.clear();
        }
    }
    let tail = cur.trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut buf_len = 0usize;

    for sent in sentences {
        let sent_len = sent.chars().count();
        if sent_len > limit {
            if !buf.is_empty() {
                chunks.push(std::mem::take(&mut buf));
                buf_len = 0;
            }
            let mut part = String::new();
            let mut part_len = 0usize;
            for ch in sent.chars() {
                part.push(ch);
                part_len += 1;
                if part_len >= limit {
                    chunks.push(std::mem::take(&mut part));
                    part_len = 0;
                }
            }
            if !part.is_empty() {
                buf = part;
                buf_len = part_len;
            }
            continue;
        }
        if buf_len > 0 && buf_len + sent_len + 1 > limit {
            chunks.push(std::mem::take(&mut buf));
            buf_len = 0;
        }
        if !buf.is_empty() {
            buf.push(' ');
            buf_len += 1;
        }
        buf.push_str(&sent);
        buf_len += sent_len;
    }
    if !buf.is_empty() {
        chunks.push(buf);
    }
    chunks
}

/// If `bytes` begins with a RIFF/WAVE header, parse its sample rate & channels
/// and return the `data` chunk payload; otherwise treat `bytes` as raw mono PCM.
pub fn parse_pcm_or_wav(bytes: &[u8], default_rate: u32) -> PcmAudio {
    if bytes.len() >= 44 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        let mut channels = 1u16;
        let mut sample_rate = default_rate;
        let mut pos = 12usize;
        while pos + 8 <= bytes.len() {
            let chunk_id = &bytes[pos..pos + 4];
            let chunk_len = u32::from_le_bytes([
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ]) as usize;
            let data_start = pos + 8;
            if chunk_id == b"fmt " && data_start + 16 <= bytes.len() {
                channels = u16::from_le_bytes([bytes[data_start + 2], bytes[data_start + 3]]);
                sample_rate = u32::from_le_bytes([
                    bytes[data_start + 4],
                    bytes[data_start + 5],
                    bytes[data_start + 6],
                    bytes[data_start + 7],
                ]);
            } else if chunk_id == b"data" {
                let end = data_start.saturating_add(chunk_len).min(bytes.len());
                return PcmAudio {
                    sample_rate: if sample_rate == 0 {
                        default_rate
                    } else {
                        sample_rate
                    },
                    channels: if channels == 0 { 1 } else { channels },
                    samples: bytes[data_start..end].to_vec(),
                };
            }
            // WAV chunks are word-aligned (padded to even length).
            let step = chunk_len.saturating_add(chunk_len & 1);
            pos = match data_start.checked_add(step) {
                Some(next) if next > pos => next,
                _ => break,
            };
        }
    }
    PcmAudio {
        sample_rate: default_rate,
        channels: 1,
        samples: bytes.to_vec(),
    }
}

fn make_req_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("moyue-{nanos}")
}

fn truncate_utf8(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.chars().take(max).collect()
}

fn decode_hex(input: &str) -> Result<Vec<u8>, &'static str> {
    let bytes = input.trim().as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("odd hex length");
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let (pairs, _) = bytes.as_chunks::<2>();
    for pair in pairs {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("invalid hex digit"),
    }
}

fn decode_base64(input: &str) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &b in input.as_bytes() {
        if b.is_ascii_whitespace() || b == b'=' {
            continue;
        }
        let val = match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err("invalid base64 byte"),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, Server};

    #[test]
    fn split_chunks_respects_sentence_boundaries() {
        let text = "第一句话很短。第二句话也不长！第三句话是问句吗？\n第四段开始了。";
        let chunks = split_into_chunks(text, 22);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "第一句话很短。 第二句话也不长！");
        assert_eq!(chunks[1], "第三句话是问句吗？ 第四段开始了。");
    }

    #[test]
    fn resolved_endpoint_normalizes_openai_base() {
        let mut cfg = Config {
            endpoint: "https://api.openai.com".into(),
            ..Config::default()
        };
        assert_eq!(
            cfg.resolved_endpoint(),
            "https://api.openai.com/v1/audio/speech"
        );
        cfg.endpoint = "https://api.siliconflow.cn/v1/".into();
        assert_eq!(
            cfg.resolved_endpoint(),
            "https://api.siliconflow.cn/v1/audio/speech"
        );
        cfg.endpoint = "https://example.com/v1/audio/speech".into();
        assert_eq!(
            cfg.resolved_endpoint(),
            "https://example.com/v1/audio/speech"
        );
    }

    #[test]
    fn parse_wav_strips_header_and_extracts_format() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&40u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
        wav.extend_from_slice(&16_000u32.to_le_bytes()); // 16kHz
        wav.extend_from_slice(&64_000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[1, 2, 3, 4]);

        let pcm = parse_pcm_or_wav(&wav, 24_000);
        assert_eq!(pcm.sample_rate, 16_000);
        assert_eq!(pcm.channels, 2);
        assert_eq!(pcm.samples, vec![1, 2, 3, 4]);
    }

    #[test]
    fn openai_compatible_synthesizes_pcm() {
        let srv = Server::start(|req| {
            assert_eq!(req.method, "POST");
            assert_eq!(req.path, "/v1/audio/speech");
            assert_eq!(req.header("authorization"), Some("Bearer sk-test"));
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["input"], "你好，墨阅。");
            assert_eq!(body["response_format"], "pcm");
            Reply::typed(200, "audio/pcm", &[0x10, 0x20, 0x30, 0x40])
        });
        let cfg = Config {
            provider: Provider::OpenAiCompatible,
            endpoint: srv.url("/").to_string(),
            api_key: "sk-test".into(),
            ..Config::default()
        };
        let client = TtsClient::new(Policy::open(), cfg);
        let audio = client.synthesize("你好，墨阅。").unwrap();
        assert_eq!(audio.sample_rate, 24_000);
        assert_eq!(audio.channels, 1);
        assert_eq!(audio.samples, vec![0x10, 0x20, 0x30, 0x40]);
    }

    #[test]
    fn volcengine_decodes_base64_pcm() {
        let srv = Server::start(|req| {
            assert_eq!(req.header("authorization"), Some("Bearer;tok-123"));
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["app"]["appid"], "app-99");
            assert_eq!(body["audio"]["encoding"], "pcm");
            // base64 of [0x01, 0x02, 0x03, 0x04] is "AQIDBA=="
            Reply::typed(
                200,
                "application/json",
                br#"{"code":3000,"message":"Success","data":"AQIDBA=="}"#,
            )
        });
        let cfg = Config {
            provider: Provider::Volcengine,
            endpoint: srv.url("/api/v1/tts").to_string(),
            app_id: "app-99".into(),
            api_key: "tok-123".into(),
            ..Config::default()
        };
        let client = TtsClient::new(Policy::open(), cfg);
        let audio = client.synthesize("测试豆包").unwrap();
        assert_eq!(audio.samples, vec![1, 2, 3, 4]);
    }

    #[test]
    fn minimax_decodes_hex_pcm() {
        let srv = Server::start(|req| {
            assert_eq!(req.header("authorization"), Some("Bearer mm-key"));
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["audio_setting"]["format"], "pcm");
            Reply::typed(
                200,
                "application/json",
                br#"{"base_resp":{"status_code":0,"status_msg":"success"},"data":{"audio":"0a0b0c0d"}}"#,
            )
        });
        let cfg = Config {
            provider: Provider::MiniMax,
            endpoint: srv.url("/v1/t2a_v2").to_string(),
            api_key: "mm-key".into(),
            ..Config::default()
        };
        let client = TtsClient::new(Policy::open(), cfg);
        let audio = client.synthesize("测试MiniMax").unwrap();
        assert_eq!(audio.samples, vec![0x0a, 0x0b, 0x0c, 0x0d]);
    }
}
