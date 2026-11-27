// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background Bluetooth audiobook (AI TTS) playback service.
//!
//! Runs on a dedicated worker thread and wakes the main event loop through
//! [`crate::wake::channel`] when a paragraph starts (`Event::ChunkStarted`),
//! when a page finishes (`Event::PageFinished`, triggering an automatic page
//! turn), or when an error occurs (`Event::Error`).
//!
//! While chunk `N` plays on [`aristotle_hal::audio::AudioSink`], a prefetch
//! thread fetches chunk `N + 1` from the configured online voice model
//! (`aristotle_net::tts::TtsClient`) so inter-sentence gaps stay minimal.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use aristotle_hal::audio::{self, AudioFormat};
use aristotle_hal::system;
use aristotle_net::tts::{
    split_into_chunks, Config as NetTtsConfig, PcmAudio, Provider, TtsClient, DEFAULT_CHUNK_CHARS,
    DEFAULT_SAMPLE_RATE,
};
use aristotle_render::book::Word;
use serde::{Deserialize, Serialize};

use crate::net;

const SILICONFLOW_VOICES: &[&str] = &[
    "FunAudioLLM/CosyVoice2-0.5B:alex",
    "FunAudioLLM/CosyVoice2-0.5B:anna",
    "FunAudioLLM/CosyVoice2-0.5B:bella",
    "FunAudioLLM/CosyVoice2-0.5B:benjamin",
    "FunAudioLLM/CosyVoice2-0.5B:charles",
    "FunAudioLLM/CosyVoice2-0.5B:claire",
    "FunAudioLLM/CosyVoice2-0.5B:david",
    "FunAudioLLM/CosyVoice2-0.5B:diana",
];

const OPENAI_VOICES: &[&str] = &[
    "alloy", "ash", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer",
];

const VOLCENGINE_VOICES: &[&str] = &[
    "BV700_V2_streaming",
    "BV701_V2_streaming",
    "BV001_streaming",
    "BV002_streaming",
    "BV406_V2_streaming",
    "BV115_streaming",
];

const MINIMAX_VOICES: &[&str] = &[
    "male-qn-qingse",
    "female-shaonv",
    "female-yujie",
    "male-qn-jingying",
    "presenter_male",
    "presenter_female",
    "audiobook_male_1",
    "audiobook_female_1",
];

/// Persistent TTS preferences stored under `"tts"` in `reader.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TtsPrefs {
    /// API protocol family (`open_ai_compatible`, `volcengine`, `mini_max`).
    pub provider: Provider,
    /// Endpoint URL (defaults to SiliconFlow CosyVoice2 OpenAI-compatible URL).
    pub endpoint: String,
    /// Bearer API key / access token.
    pub api_key: String,
    /// Volcengine `appid`.
    pub app_id: String,
    /// Volcengine `cluster`.
    pub cluster: String,
    /// Model name.
    pub model: String,
    /// Voice ID.
    pub voice: String,
    /// Speech rate (`0.5..=2.0`).
    pub speed: f32,
    /// Sample rate in Hz (default `24000`).
    pub sample_rate: u32,
    /// When `true`, use [`aristotle_hal::audio::NullTimingSink`] instead of
    /// Kindle `mixersink` so the full playback + sleep keepalive loop can be
    /// tested without Bluetooth earphones.
    pub simulate_audio: bool,
}

impl Default for TtsPrefs {
    fn default() -> Self {
        let net = NetTtsConfig::default();
        Self {
            provider: net.provider,
            endpoint: net.endpoint,
            api_key: net.api_key,
            app_id: net.app_id,
            cluster: net.cluster,
            model: net.model,
            voice: net.voice,
            speed: net.speed,
            sample_rate: net.sample_rate,
            simulate_audio: false,
        }
    }
}

impl TtsPrefs {
    /// Convert to [`aristotle_net::tts::Config`].
    pub fn to_net_config(&self) -> NetTtsConfig {
        NetTtsConfig {
            provider: self.provider,
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
            app_id: self.app_id.clone(),
            cluster: self.cluster.clone(),
            model: self.model.clone(),
            voice: self.voice.clone(),
            speed: self.speed,
            sample_rate: self.sample_rate,
        }
    }

    /// Whether enough credentials are configured to call the online TTS API,
    /// or `simulate_audio` is enabled for dry-run testing without a key.
    pub fn is_ready(&self) -> bool {
        if self.simulate_audio {
            return true;
        }
        match self.provider {
            Provider::Volcengine => {
                !self.api_key.trim().is_empty() && !self.app_id.trim().is_empty()
            }
            Provider::OpenAiCompatible | Provider::MiniMax => !self.api_key.trim().is_empty(),
        }
    }

    /// Preset index for the 4-way provider segmented control:
    /// `0` = 硅基流动, `1` = OpenAI, `2` = 火山豆包, `3` = MiniMax.
    pub fn provider_preset_index(&self) -> usize {
        match self.provider {
            Provider::OpenAiCompatible => {
                if self.endpoint.contains("openai.com") {
                    1
                } else {
                    0
                }
            }
            Provider::Volcengine => 2,
            Provider::MiniMax => 3,
        }
    }

    /// Apply one of the 4 provider presets, resetting endpoint/model/voice to
    /// that provider's defaults while preserving `api_key` and `speed`.
    pub fn apply_provider_preset(&mut self, index: usize) {
        match index {
            1 => {
                self.provider = Provider::OpenAiCompatible;
                self.endpoint = "https://api.openai.com/v1/audio/speech".into();
                self.model = "gpt-4o-mini-tts".into();
                self.voice = "alloy".into();
            }
            2 => {
                self.provider = Provider::Volcengine;
                self.endpoint = Provider::Volcengine.default_endpoint().into();
                self.model = Provider::Volcengine.default_model().into();
                self.voice = Provider::Volcengine.default_voice().into();
                if self.cluster.trim().is_empty() {
                    self.cluster = "volcano_tts".into();
                }
            }
            3 => {
                self.provider = Provider::MiniMax;
                self.endpoint = Provider::MiniMax.default_endpoint().into();
                self.model = Provider::MiniMax.default_model().into();
                self.voice = Provider::MiniMax.default_voice().into();
            }
            _ => {
                self.provider = Provider::OpenAiCompatible;
                self.endpoint = Provider::OpenAiCompatible.default_endpoint().into();
                self.model = Provider::OpenAiCompatible.default_model().into();
                self.voice = Provider::OpenAiCompatible.default_voice().into();
            }
        }
    }

    /// Human-readable Chinese provider name.
    pub fn provider_display_name(&self) -> &'static str {
        match self.provider_preset_index() {
            0 => "硅基流动",
            1 => "OpenAI",
            2 => "火山豆包",
            _ => "MiniMax",
        }
    }

    /// Built-in voice preset list for the current provider.
    pub fn preset_voices(&self) -> &'static [&'static str] {
        match self.provider_preset_index() {
            0 => SILICONFLOW_VOICES,
            1 => OPENAI_VOICES,
            2 => VOLCENGINE_VOICES,
            _ => MINIMAX_VOICES,
        }
    }

    /// Switch to the previous or next preset voice.
    pub fn cycle_voice(&mut self, delta: isize) {
        let list = self.preset_voices();
        if list.is_empty() {
            return;
        }
        let cur = list
            .iter()
            .position(|v| *v == self.voice.trim())
            .unwrap_or(0);
        let len = list.len() as isize;
        let next = ((cur as isize + delta) % len + len) % len;
        self.voice = list[next as usize].to_string();
    }

    /// Adjust speech rate by `delta`, clamped to `[0.5, 2.0]` and rounded to 0.1.
    pub fn adjust_speed(&mut self, delta: f32) {
        let next = (self.speed + delta).clamp(0.5, 2.0);
        self.speed = (next * 10.0).round() / 10.0;
    }

    /// Redacted summary of `api_key` for display in settings.
    pub fn api_key_summary(&self) -> String {
        let k = self.api_key.trim();
        if k.is_empty() {
            "未设置（点击输入）".to_string()
        } else {
            let prefix: String = k.chars().take(4).collect();
            format!("已设置 ({prefix}…)")
        }
    }

    /// Compact summary shown on the main Settings screen row.
    pub fn settings_summary(&self) -> String {
        if self.simulate_audio && self.api_key.trim().is_empty() {
            format!("模拟模式 · {:.1}x", self.speed)
        } else if self.is_ready() {
            format!("{} · {:.1}x", self.provider_display_name(), self.speed)
        } else {
            "未配置 API".to_string()
        }
    }
}

/// Ensure the device's scoped firewall allows the configured TTS host.
pub fn allow_host(prefs: &TtsPrefs) {
    let Some(host) = prefs.to_net_config().allowlist_host() else {
        return;
    };
    let policy = net::Net::policy();
    let mut wanted = BTreeSet::new();
    wanted.insert((host, "tts"));
    for (entry, feature) in aristotle_net::policy::missing_entries(&policy, &wanted) {
        match system::net_allow(true, &entry, feature) {
            Ok(message) => crate::perf!("tts: allowlist: {message}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::perf!("tts: no agent, no allowlist to change for {entry}")
            }
            Err(error) => eprintln!("aristotle: tts allowlist {entry}: {error}"),
        }
    }
}

/// Join a page's layout [`Word`]s into clean reading text.
///
/// Strips U+00AD soft hyphens and inserts spaces only between adjacent ASCII /
/// Latin words, keeping CJK characters contiguous.
pub fn join_page_words(words: &[Word]) -> String {
    let mut out = String::new();
    let mut prev_ascii = false;
    for w in words {
        let piece = w.text.replace('\u{00ad}', "");
        let trimmed = piece.trim();
        if trimmed.is_empty() {
            continue;
        }
        let first_ascii = trimmed
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        if !out.is_empty() && prev_ascii && first_ascii {
            out.push(' ');
        }
        out.push_str(trimmed);
        prev_ascii = trimmed
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric());
    }
    out
}

/// Split page text into paragraph/sentence chunks using the standard TTS chunk size.
pub fn page_chunks(text: &str) -> Vec<String> {
    split_into_chunks(text, DEFAULT_CHUNK_CHARS)
}

/// Events sent from the TTS worker back to the main event loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Started speaking `chunk_index` (0-based) out of `total_chunks` on `location`.
    ChunkStarted {
        seq: u64,
        location: usize,
        chunk_index: usize,
        total_chunks: usize,
        text: String,
        next_text: Option<String>,
    },
    /// Finished speaking every chunk on `location`; reader should turn forward.
    PageFinished { seq: u64, location: usize },
    /// Synthesis or playback failed; playback has stopped.
    Error { seq: u64, message: String },
}

struct SpeakJob {
    seq: u64,
    location: usize,
    chunks: Vec<String>,
    start_chunk: usize,
    prefs: TtsPrefs,
    cancel: Arc<AtomicBool>,
}

/// Background TTS playback service.
pub struct Service {
    tx: Sender<SpeakJob>,
    rx: Receiver<Event>,
    seq: u64,
    active: bool,
    cancel: Arc<AtomicBool>,
}

impl Service {
    /// Start the background TTS worker thread.
    pub fn start() -> Self {
        let (tx, jobs) = mpsc::channel::<SpeakJob>();
        let (events, rx) = crate::wake::channel::<Event>();
        let _ = thread::Builder::new()
            .name("tts-player".into())
            .spawn(move || {
                for job in jobs {
                    if job.cancel.load(Ordering::Acquire) {
                        continue;
                    }
                    let outcome = run_page_job(&job, &events);
                    if job.cancel.load(Ordering::Acquire) {
                        continue;
                    }
                    let ev = match outcome {
                        Ok(()) => Event::PageFinished {
                            seq: job.seq,
                            location: job.location,
                        },
                        Err(message) => Event::Error {
                            seq: job.seq,
                            message,
                        },
                    };
                    if events.send(ev).is_err() {
                        break;
                    }
                }
            });
        Self {
            tx,
            rx,
            seq: 0,
            active: false,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether audiobook playback is currently active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Begin speaking `chunks` from `start_chunk` for `location`, cancelling any
    /// previous page job.
    pub fn speak_chunks(
        &mut self,
        location: usize,
        chunks: Vec<String>,
        start_chunk: usize,
        prefs: TtsPrefs,
    ) -> u64 {
        self.cancel.store(true, Ordering::Release);
        while self.rx.try_recv().is_ok() {}
        self.seq += 1;
        let seq = self.seq;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.active = true;
        let _ = self.tx.send(SpeakJob {
            seq,
            location,
            chunks,
            start_chunk,
            prefs,
            cancel,
        });
        seq
    }

    /// Stop playback immediately.
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.active = false;
        while self.rx.try_recv().is_ok() {}
    }

    /// Poll for the next event belonging to the current playback sequence.
    pub fn poll(&mut self) -> Option<Event> {
        while let Ok(ev) = self.rx.try_recv() {
            let seq = match &ev {
                Event::ChunkStarted { seq, .. }
                | Event::PageFinished { seq, .. }
                | Event::Error { seq, .. } => *seq,
            };
            if seq == self.seq {
                if matches!(ev, Event::Error { .. }) {
                    self.active = false;
                }
                return Some(ev);
            }
        }
        None
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn run_page_job(job: &SpeakJob, events: &crate::wake::Sender<Event>) -> Result<(), String> {
    let total_chunks = job.chunks.len();
    if total_chunks == 0 || job.start_chunk >= total_chunks {
        return Ok(());
    }

    let dry_run_no_key = job.prefs.simulate_audio && job.prefs.api_key.trim().is_empty();
    let policy = net::Net::policy();
    let net_cfg = job.prefs.to_net_config();
    let cancel_fetch = job.cancel.clone();
    let chunks_for_fetch: Vec<(usize, String, Option<String>)> = (job.start_chunk..total_chunks)
        .map(|i| {
            let next = job.chunks.get(i + 1).cloned();
            (i, job.chunks[i].clone(), next)
        })
        .collect();

    // Bounded channel of capacity 2: prefetches chunk N+1 while chunk N plays.
    let (pcm_tx, pcm_rx) =
        mpsc::sync_channel::<Result<(usize, String, Option<String>, PcmAudio), String>>(2);
    let fetcher = thread::Builder::new()
        .name("tts-prefetch".into())
        .spawn(move || {
            let client = TtsClient::new(policy, net_cfg);
            for (idx, chunk, next_text) in chunks_for_fetch {
                if cancel_fetch.load(Ordering::Acquire) {
                    break;
                }
                let res = if dry_run_no_key {
                    Ok(synthesize_silent_pcm(&chunk, DEFAULT_SAMPLE_RATE))
                } else {
                    client.synthesize(&chunk).map_err(|e| e.to_string())
                };
                let msg = res.map(|pcm| (idx, chunk, next_text, pcm));
                if pcm_tx.send(msg).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| format!("启动预取线程失败: {e}"))?;

    let mut sink = audio::open_sink(job.prefs.simulate_audio);
    let mut play_err: Option<String> = None;
    for item in pcm_rx {
        if job.cancel.load(Ordering::Acquire) {
            break;
        }
        match item {
            Ok((chunk_index, text, next_text, audio_buf)) => {
                let _ = events.send(Event::ChunkStarted {
                    seq: job.seq,
                    location: job.location,
                    chunk_index,
                    total_chunks,
                    text,
                    next_text,
                });
                let fmt = AudioFormat {
                    sample_rate: audio_buf.sample_rate,
                    channels: audio_buf.channels,
                };
                if let Err(e) = sink.play_pcm(&audio_buf.samples, fmt, &job.cancel) {
                    play_err = Some(format!("音频输出失败: {e}"));
                    job.cancel.store(true, Ordering::Release);
                    break;
                }
            }
            Err(e) => {
                play_err = Some(e);
                job.cancel.store(true, Ordering::Release);
                break;
            }
        }
    }
    let _ = fetcher.join();
    match play_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Generate timing-accurate silent PCM (~4.5 chars/sec, capped at 3 s per chunk)
/// for earphone-less / key-less dry-run testing.
fn synthesize_silent_pcm(text: &str, sample_rate: u32) -> PcmAudio {
    let chars = text.chars().count().max(1);
    let millis = ((chars as u64) * 220).clamp(600, 3_000);
    let bytes = ((sample_rate as u64) * 2 * millis / 1000) as usize;
    PcmAudio {
        sample_rate,
        channels: 1,
        samples: vec![0u8; bytes],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aristotle_render::book::Bounds;

    fn word(text: &str) -> Word {
        Word {
            text: text.to_string(),
            rect: Bounds {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 10,
            },
            offset: 0,
            start_xpointer: None,
            end_xpointer: None,
        }
    }

    #[test]
    fn join_page_words_handles_cjk_and_latin() {
        let words = vec![
            word("你"),
            word("好"),
            word("Hello"),
            word("world"),
            word("。"),
        ];
        assert_eq!(join_page_words(&words), "你好Hello world。");
    }

    #[test]
    fn prefs_presets_voice_cycle_and_speed_work() {
        let mut p = TtsPrefs::default();
        assert_eq!(p.provider_preset_index(), 0);
        p.cycle_voice(1);
        assert_eq!(p.voice, "FunAudioLLM/CosyVoice2-0.5B:anna");
        p.adjust_speed(0.2);
        assert!((p.speed - 1.2).abs() < 0.001);
        p.apply_provider_preset(2);
        assert_eq!(p.provider, Provider::Volcengine);
        assert_eq!(p.provider_display_name(), "火山豆包");
    }
}
