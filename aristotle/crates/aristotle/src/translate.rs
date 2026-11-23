// SPDX-License-Identifier: AGPL-3.0-or-later
//! Translations on a thread of their own.
//!
//! A translation takes a moment: the service is on the network, and the
//! reader must not wait for it. So the reader asks here, and the answer
//! comes back on the thread that wakes the event loop ([`crate::wake`]).
//!
//! Every request carries a number. An answer that belongs to an older
//! request is dropped: the reader has moved on, and a late answer must
//! not overwrite what it shows now.

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, Sender};

use aristotle_hal::system;
use aristotle_net::translate::{Config, Translation, Translator};

use crate::net;

/// The hosts the translation services live on.
const HOSTS: [(&str, &str); 3] = [
    ("edge.microsoft.com:443", "translate"),
    (
        "api-edge.cognitive.microsofttranslator.com:443",
        "translate",
    ),
    ("api.mymemory.translated.net:443", "translate"),
];

/// Make sure the device's firewall lets the translation services through.
///
/// A device in `scoped` mode reaches only the hosts its `net-allow` names,
/// so the services are added before the first request. In `off` mode, or
/// without the agent, this does nothing.
pub fn allow_hosts() {
    let policy = net::Net::policy();
    let wanted: BTreeSet<(String, &'static str)> = HOSTS
        .iter()
        .map(|(entry, feature)| (entry.to_string(), *feature))
        .collect();
    for (entry, feature) in aristotle_net::policy::missing_entries(&policy, &wanted) {
        match system::net_allow(true, &entry, feature) {
            Ok(message) => crate::perf!("net: allowlist: {message}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::perf!("net: no agent, no allowlist to change for {entry}")
            }
            Err(error) => eprintln!("aristotle: allowlist {entry}: {error}"),
        }
    }
}

/// A request for a translation.
pub enum Request {
    Translate {
        /// The request's number.
        seq: u64,
        /// The text to translate.
        text: String,
    },
}

/// The answer to a request.
pub enum Reply {
    Translated {
        /// The request's number.
        seq: u64,
        /// The translation, or why there is none.
        result: Result<Translation, String>,
    },
}

/// The translation service.
pub struct Service {
    tx: Sender<Request>,
    rx: Receiver<Reply>,
    /// The number of the last request.
    seq: u64,
}

impl Service {
    /// Start the thread.
    ///
    /// The service reads the device's policy when it starts, so it only
    /// reaches the hosts the device allows.
    pub fn start() -> Service {
        let (tx, requests) = std::sync::mpsc::channel::<Request>();
        let (answers, rx) = crate::wake::channel();
        let _ = std::thread::Builder::new()
            .name("translate".into())
            .spawn(move || {
                let policy = net::Net::policy();
                let translator = Translator::new(policy, Config::default());
                for request in requests {
                    let Request::Translate { seq, text } = request;
                    let result = match translator.translate(&text) {
                        Ok(t) => Ok(t),
                        Err(e) => Err(e.to_string()),
                    };
                    if answers.send(Reply::Translated { seq, result }).is_err() {
                        break;
                    }
                }
            });
        Service { tx, rx, seq: 0 }
    }

    /// Ask for a translation; returns the request's number.
    pub fn ask(&mut self, text: String) -> u64 {
        self.seq += 1;
        let seq = self.seq;
        // Answers nobody waits for are dropped.
        while self.rx.try_recv().is_ok() {}
        let _ = self.tx.send(Request::Translate { seq, text });
        seq
    }

    /// The answer to the newest request, if it came.
    ///
    /// An answer to an older request is dropped: the reader has asked
    /// again since, and that answer is stale.
    pub fn take(&mut self) -> Option<Reply> {
        let mut newest: Option<(u64, Result<Translation, String>)> = None;
        while let Ok(Reply::Translated { seq, result }) = self.rx.try_recv() {
            newest = Some((seq, result));
        }
        let (seq, result) = newest?;
        if seq != self.seq {
            return None;
        }
        Some(Reply::Translated { seq, result })
    }
}
