// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ephemeral LAN HTTP server for phone QR-code input.
//!
//! Typing long API keys, URLs, or model names on an e-ink touchscreen keyboard
//! is tedious. While a QR panel is open, [`LanInputServer`] listens on the
//! Kindle's Wi-Fi interface, opens the port in `iptables` via
//! [`crate::transfer::firewall::Firewall`], serves a mobile-friendly HTML form,
//! and delivers the submitted text or full TTS configuration back to the app.
//! Dropping the server stops the listener thread and closes the firewall port.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::transfer::firewall::Firewall;
use crate::transfer::local::{address_of, WLAN};
use crate::tts::Provider;

/// Default port for the LAN QR input page (separate from book transfer 8080).
pub const DEFAULT_PORT: u16 = 8088;

/// Maximum POST body accepted (64 KiB is plenty for config/text input).
const MAX_BODY: usize = 64 * 1024;

/// Payload submitted by the phone browser for full TTS configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TtsConfigPayload {
    /// Preset index: 0=硅基流动, 1=OpenAI, 2=火山豆包, 3=MiniMax.
    pub provider_index: usize,
    pub endpoint: String,
    pub api_key: String,
    pub app_id: String,
    pub model: String,
    pub voice: String,
    pub speed: f32,
    pub simulate_audio: bool,
}

impl TtsConfigPayload {
    /// Map `provider_index` to [`Provider`].
    pub fn provider(&self) -> Provider {
        match self.provider_index {
            2 => Provider::Volcengine,
            3 => Provider::MiniMax,
            _ => Provider::OpenAiCompatible,
        }
    }
}

/// Which form the LAN input server presents to the phone browser.
#[derive(Clone, Debug)]
pub enum Mode {
    /// A single text field (reusable for any text input screen).
    SingleField {
        title: String,
        initial: String,
        secret: bool,
    },
    /// Full audiobook TTS configuration form (API + voice settings in one go).
    TtsConfig(TtsConfigPayload),
}

/// Value received from the phone browser.
#[derive(Clone, Debug, PartialEq)]
pub enum Submission {
    Text(String),
    TtsConfig(TtsConfigPayload),
}

/// Running LAN QR-input server. Dropping it stops the thread and closes the port.
pub struct LanInputServer {
    /// Full `http://<ip>:<port>` URL to encode into the QR code.
    pub url: String,
    /// Actual bound TCP port.
    pub port: u16,
    stop: Arc<AtomicBool>,
    rx: Receiver<Submission>,
    handle: Option<JoinHandle<()>>,
    _firewall: Firewall,
}

impl LanInputServer {
    /// Start listening on `0.0.0.0:DEFAULT_PORT` (or an ephemeral port if busy).
    ///
    /// `on_receive` is invoked on the server thread right after a submission is
    /// queued so the caller can wake its event loop immediately.
    pub fn start(mode: Mode, on_receive: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        Self::start_on(DEFAULT_PORT, mode, on_receive)
    }

    /// Start listening on a specific `port` (pass `0` in tests for an OS-assigned port).
    pub fn start_on(
        preferred_port: u16,
        mode: Mode,
        on_receive: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", preferred_port))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;

        let mut firewall = Firewall::new(port, WLAN);
        let _ = firewall.open();

        let ip = address_of(WLAN)
            .map(|a| a.to_string())
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let url = format!("http://{ip}:{port}");

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let (tx, rx) = mpsc::channel::<Submission>();
        let notify = Arc::new(on_receive);

        let handle = thread::Builder::new()
            .name("lan-input".into())
            .spawn(move || {
                serve_loop(listener, mode, stop_bg, tx, notify);
            })?;

        Ok(Self {
            url,
            port,
            stop,
            rx,
            handle: Some(handle),
            _firewall: firewall,
        })
    }

    /// Non-blocking poll for a submission from the phone browser.
    pub fn try_recv(&self) -> Option<Submission> {
        self.rx.try_recv().ok()
    }
}

impl Drop for LanInputServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve_loop(
    listener: TcpListener,
    mode: Mode,
    stop: Arc<AtomicBool>,
    tx: Sender<Submission>,
    notify: Arc<dyn Fn() + Send + Sync>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                handle_conn(&mut stream, &mode, &tx, &*notify);
                let _ = stream.shutdown(Shutdown::Both);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(60));
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_conn(stream: &mut TcpStream, mode: &Mode, tx: &Sender<Submission>, notify: &dyn Fn()) {
    let Some((method, path, body)) = read_http_request(stream) else {
        return;
    };
    if method == "GET" && (path == "/" || path.starts_with("/?")) {
        let html = render_html(mode);
        write_response(stream, 200, "text/html; charset=utf-8", html.as_bytes());
        return;
    }
    if method == "POST" && path == "/submit" {
        match mode {
            Mode::SingleField { .. } => {
                #[derive(Deserialize)]
                struct TextReq {
                    value: String,
                }
                match serde_json::from_slice::<TextReq>(&body) {
                    Ok(req) => {
                        let _ = tx.send(Submission::Text(req.value.trim().to_string()));
                        notify();
                        write_response(
                            stream,
                            200,
                            "application/json; charset=utf-8",
                            br#"{"ok":true}"#,
                        );
                    }
                    Err(e) => {
                        let msg = format!(r#"{{"ok":false,"error":{:?}}}"#, e.to_string());
                        write_response(
                            stream,
                            400,
                            "application/json; charset=utf-8",
                            msg.as_bytes(),
                        );
                    }
                }
            }
            Mode::TtsConfig(_) => match serde_json::from_slice::<TtsConfigPayload>(&body) {
                Ok(cfg) => {
                    let _ = tx.send(Submission::TtsConfig(cfg));
                    notify();
                    write_response(
                        stream,
                        200,
                        "application/json; charset=utf-8",
                        br#"{"ok":true}"#,
                    );
                }
                Err(e) => {
                    let msg = format!(r#"{{"ok":false,"error":{:?}}}"#, e.to_string());
                    write_response(
                        stream,
                        400,
                        "application/json; charset=utf-8",
                        msg.as_bytes(),
                    );
                }
            },
        }
        return;
    }
    write_response(stream, 404, "text/plain; charset=utf-8", b"Not Found");
}

fn read_http_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
    };

    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut content_len = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_len > MAX_BODY {
        return None;
    }

    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_len {
        let need = (content_len - body.len()).min(tmp.len());
        let n = stream.read(&mut tmp[..need]).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_len);
    Some((method, path, body))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_html(mode: &Mode) -> String {
    match mode {
        Mode::SingleField {
            title,
            initial,
            secret,
        } => {
            let t = html_escape(title);
            let v = if *secret {
                String::new()
            } else {
                html_escape(initial)
            };
            let placeholder = if *secret {
                "粘贴 API Key / 密钥…"
            } else {
                "输入或粘贴内容…"
            };
            format!(
                r#"<!doctype html><html lang="zh"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>墨阅扫码输入</title><style>body{{font-family:-apple-system,sans-serif;max-width:480px;margin:24px auto;padding:0 16px;background:#f7f7f5;color:#111}}h2{{margin:0 0 12px}}textarea{{width:100%;box-sizing:border-box;min-height:110px;font-size:16px;padding:12px;border:1.5px solid #222;border-radius:10px}}button{{margin-top:14px;width:100%;padding:14px;font-size:17px;font-weight:600;background:#111;color:#fff;border:none;border-radius:10px}}#msg{{margin-top:12px;font-size:15px;color:#0a7}}</style></head><body><h2>墨阅 · {t}</h2><textarea id="val" placeholder="{placeholder}">{v}</textarea><button onclick="send()">发送到 Kindle</button><div id="msg"></div><script>async function send(){{const v=document.getElementById('val').value;const r=await fetch('/submit',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{value:v}})}});const j=await r.json();document.getElementById('msg').innerText=j.ok?'✓ 已发送到 Kindle，可以关闭此页面':'失败：'+(j.error||'');}}</script></body></html>"#
            )
        }
        Mode::TtsConfig(cfg) => {
            let endpoint = html_escape(&cfg.endpoint);
            let api_key = html_escape(&cfg.api_key);
            let app_id = html_escape(&cfg.app_id);
            let model = html_escape(&cfg.model);
            let voice = html_escape(&cfg.voice);
            let speed = format!("{:.1}", cfg.speed);
            let sim_checked = if cfg.simulate_audio { "checked" } else { "" };
            let p_idx = cfg.provider_index.min(3);
            format!(
                r#"<!doctype html><html lang="zh"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>墨阅听书配置</title><style>body{{font-family:-apple-system,sans-serif;max-width:520px;margin:18px auto;padding:0 16px 36px;background:#f7f7f5;color:#111}}h2{{margin:0 0 6px}}.sub{{color:#666;font-size:13px;margin-bottom:16px}}label{{display:block;font-size:14px;font-weight:600;margin:12px 0 4px}}input,select{{width:100%;box-sizing:border-box;font-size:15px;padding:10px 12px;border:1.5px solid #bbb;border-radius:10px;background:#fff}}.row{{display:flex;align-items:center;gap:10px;margin-top:14px}}.row input{{width:auto}}button{{margin-top:18px;width:100%;padding:14px;font-size:17px;font-weight:600;background:#111;color:#fff;border:none;border-radius:12px}}#msg{{margin-top:14px;font-size:15px;font-weight:600;color:#0a7}}</style></head><body><h2>墨阅 · 听书 API 配置</h2><div class="sub">局域网直连 Kindle，粘贴后点击保存即刻同步生效</div><label>语音大模型服务商</label><select id="provider" onchange="applyPreset()"><option value="0">硅基流动 (CosyVoice2 / FishSpeech)</option><option value="1">OpenAI 兼容 (/v1/audio/speech)</option><option value="2">火山引擎 · 豆包语音大模型</option><option value="3">MiniMax Speech (Token Plan / T2A v2)</option></select><label>API 地址</label><input id="endpoint" value="{endpoint}"><label>API Key / Token</label><input id="api_key" value="{api_key}" placeholder="粘贴你的 API Key"><label>火山引擎 App ID（仅火山豆包需要）</label><input id="app_id" value="{app_id}" placeholder="可选"><label>语音模型</label><input id="model" value="{model}"><label>音色 (Voice ID)</label><input id="voice" value="{voice}"><label>朗读语速 (0.5 - 2.0)</label><input id="speed" type="number" step="0.1" min="0.5" max="2.0" value="{speed}"><div class="row"><input id="sim" type="checkbox" {sim_checked}><label for="sim" style="margin:0">无耳机模拟模式（无蓝牙耳机时测试自动翻页）</label></div><button onclick="save()">一键保存到 Kindle</button><div id="msg"></div><script>const presets=[{{ep:'https://api.siliconflow.cn/v1/audio/speech',m:'FunAudioLLM/CosyVoice2-0.5B',v:'FunAudioLLM/CosyVoice2-0.5B:alex'}},{{ep:'https://api.openai.com/v1/audio/speech',m:'gpt-4o-mini-tts',v:'alloy'}},{{ep:'https://openspeech.bytedance.com/api/v1/tts',m:'volcano_tts',v:'BV700_V2_streaming'}},{{ep:'https://api.minimaxi.com/v1/t2a_v2',m:'speech-02-turbo',v:'male-qn-qingse'}}];document.getElementById('provider').value='{p_idx}';function applyPreset(){{const p=presets[Number(document.getElementById('provider').value)]||presets[0];document.getElementById('endpoint').value=p.ep;document.getElementById('model').value=p.m;document.getElementById('voice').value=p.v;}}async function save(){{const payload={{provider_index:Number(document.getElementById('provider').value),endpoint:document.getElementById('endpoint').value.trim(),api_key:document.getElementById('api_key').value.trim(),app_id:document.getElementById('app_id').value.trim(),model:document.getElementById('model').value.trim(),voice:document.getElementById('voice').value.trim(),speed:parseFloat(document.getElementById('speed').value)||1.0,simulate_audio:document.getElementById('sim').checked}};const r=await fetch('/submit',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify(payload)}});const j=await r.json();document.getElementById('msg').innerText=j.ok?'✓ 已保存并同步到 Kindle！可以关闭此页面':'失败：'+(j.error||'');}}</script></body></html>"#
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Client, Request, Timeouts};
    use crate::policy::Policy;
    use crate::url::Url;

    #[test]
    fn lan_input_serves_html_and_receives_tts_config() {
        let woken = Arc::new(AtomicBool::new(false));
        let woken_bg = woken.clone();
        let initial = TtsConfigPayload {
            provider_index: 3,
            endpoint: "https://api.minimaxi.com/v1/t2a_v2".into(),
            api_key: String::new(),
            app_id: String::new(),
            model: "speech-02-turbo".into(),
            voice: "male-qn-qingse".into(),
            speed: 1.0,
            simulate_audio: false,
        };
        let srv = LanInputServer::start_on(0, Mode::TtsConfig(initial.clone()), move || {
            woken_bg.store(true, Ordering::Release);
        })
        .unwrap();

        let client = Client::new(Policy::open(), Timeouts::quick(Duration::from_secs(3)));
        let base = format!("http://127.0.0.1:{}", srv.port);
        let page = client
            .send(&Request::get(Url::parse(&base).unwrap()), 64 * 1024)
            .unwrap();
        assert_eq!(page.status, 200);
        assert!(page.text().contains("墨阅 · 听书 API 配置"));

        let submitted = TtsConfigPayload {
            api_key: "sk-minimax-tokenplan".into(),
            speed: 1.2,
            ..initial
        };
        let body = serde_json::to_vec(&submitted).unwrap();
        let post = Request::post(Url::parse(&format!("{base}/submit")).unwrap(), body)
            .header("Content-Type", "application/json");
        let resp = client.send(&post, 4096).unwrap();
        assert_eq!(resp.status, 200);
        assert!(woken.load(Ordering::Acquire));
        assert_eq!(srv.try_recv(), Some(Submission::TtsConfig(submitted)));
    }

    #[test]
    fn lan_input_receives_single_field_text() {
        let srv = LanInputServer::start_on(
            0,
            Mode::SingleField {
                title: "API Key".into(),
                initial: String::new(),
                secret: true,
            },
            || {},
        )
        .unwrap();
        let client = Client::new(Policy::open(), Timeouts::quick(Duration::from_secs(3)));
        let post = Request::post(
            Url::parse(&format!("http://127.0.0.1:{}/submit", srv.port)).unwrap(),
            br#"{"value":"  my-secret-key  "}"#.to_vec(),
        )
        .header("Content-Type", "application/json");
        let resp = client.send(&post, 4096).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(
            srv.try_recv(),
            Some(Submission::Text("my-secret-key".into()))
        );
    }
}
