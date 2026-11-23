//! Reading-time spans from the reader's statistics to §14.5 upload batches.
//!
//! `stats-*.jsonl` stays the local record (`STAT-001`); this module is the
//! independent queue that carries the same page stays to WeRead. The app feeds
//! one [`StaySpan`] per stay, the queue merges them per book, and an upload
//! takes at most [`MAX_BATCH_SECS`] seconds before [`crate::weread::client`]
//! cuts the spans on the GMT+8 hour boundaries of contract §14.5.
//!
//! # Receipts
//!
//! A batch is moved to [`InflightBatch`] before the request leaves, so a
//! process that dies mid-upload cannot resend it blindly: [`ReadtimeQueue::normalized`]
//! turns a leftover in-flight batch into an unconfirmed one. Only an explicit
//! `succ:1` clears it; an unconfirmed batch is kept for the record and never
//! taken again. A request that provably never left (the pre-flight
//! `getProgress` already failed) is returned to the pending spans instead.
//!
//! # Contract
//!
//! - §14.5: hour buckets are GMT+8; Unix hour boundaries are the same instants,
//!   so the wire zone is [`READ_TIME_ZONE`] and no offset is added.
//! - §14.6: `PageStay{start_time,duration}` maps one to one; no receipt means
//!   keep, no resend; a moving cloud position pauses the queue.

use serde::{Deserialize, Serialize};

use super::client::split_reading_hours;
use super::model::{ReadingHour, ReadingInterval};
use super::WeReadError;

/// The wire time zone of the §14.5 hour buckets (GMT+8, no DST).
pub const READ_TIME_ZONE: &str = "+08:00";
/// Longest span one wire hour bucket can hold; longer spans are truncated.
pub const MAX_SPAN_SECS: i64 = 3_600;
/// Longest local batch per book (a task limit, not a server rule).
pub const MAX_BATCH_SECS: i64 = 600;
/// Schema version of the persisted queue.
pub const READTIME_QUEUE_VERSION: u32 = 1;

/// One page stay, the fields §14.6 maps from `PageStay`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StaySpan {
    /// Unix seconds when the page appeared.
    pub start_time: i64,
    /// Seconds spent on it.
    pub duration: i64,
}

impl StaySpan {
    /// A stay with a positive duration; `None` when it is not worth counting.
    pub fn new(start_time: i64, duration: i64) -> Option<StaySpan> {
        (duration > 0).then_some(StaySpan {
            start_time,
            duration: duration.min(MAX_SPAN_SECS),
        })
    }

    /// Unix second the stay ends at.
    pub fn end_time(self) -> i64 {
        self.start_time.saturating_add(self.duration)
    }
}

/// One queued span of a WeRead book, as stored on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedSpan {
    /// WeRead book id.
    pub book_id: String,
    /// Unix seconds when the page appeared.
    pub start_time: i64,
    /// Seconds spent on it, 1..=[`MAX_SPAN_SECS`].
    pub duration: i64,
}

impl QueuedSpan {
    /// Unix second the span ends at.
    pub fn end_time(&self) -> i64 {
        self.start_time.saturating_add(self.duration)
    }
}

/// Why the queue stopped uploading by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PauseReason {
    /// Another device moved the cloud reading position while we uploaded.
    RemoteMoved,
    /// The cache/credentials want a new sign-in.
    Auth,
}

/// A batch that was sent and has no receipt yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InflightBatch {
    /// Unix seconds when the batch was handed to the transport.
    pub sent_at: i64,
    /// The spans the request carried.
    pub spans: Vec<QueuedSpan>,
}

impl InflightBatch {
    /// Total seconds in the batch.
    pub fn seconds(&self) -> i64 {
        self.spans.iter().map(|span| span.duration).sum()
    }
}

/// The persisted reading-time queue.
///
/// Field order is the on-disk order. Every field has a serde default, so a
/// file with missing fields loads; an unknown [`PauseReason`] or schema version
/// is rejected, and the app then falls back to its `.bak` generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadtimeQueue {
    #[serde(default = "queue_version")]
    pub version: u32,
    /// Spans waiting for a first send.
    #[serde(default)]
    pub spans: Vec<QueuedSpan>,
    /// Spans that were sent without a `succ:1` receipt; never resent.
    #[serde(default)]
    pub unconfirmed: Vec<QueuedSpan>,
    /// The batch in flight, if a process died between send and receipt.
    #[serde(default)]
    pub inflight: Option<InflightBatch>,
    #[serde(default)]
    pub paused: Option<PauseReason>,
    /// Unix seconds before which no upload starts (rate-limit cooldown).
    #[serde(default)]
    pub cooldown_until: i64,
}

fn queue_version() -> u32 {
    READTIME_QUEUE_VERSION
}

impl Default for ReadtimeQueue {
    fn default() -> Self {
        Self {
            version: READTIME_QUEUE_VERSION,
            spans: Vec::new(),
            unconfirmed: Vec::new(),
            inflight: None,
            paused: None,
            cooldown_until: 0,
        }
    }
}

impl ReadtimeQueue {
    /// An empty queue at the current schema version.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load-time repair: a batch left in flight is unconfirmed, and spans are
    /// clamped, de-duplicated and merged.
    pub fn normalized(mut self) -> Self {
        self.version = READTIME_QUEUE_VERSION;
        self.spans = sanitized(std::mem::take(&mut self.spans));
        self.unconfirmed = sanitized(std::mem::take(&mut self.unconfirmed));
        if let Some(mut inflight) = self.inflight.take() {
            inflight.spans = sanitized(inflight.spans);
            if !inflight.spans.is_empty() {
                self.unconfirmed.extend(inflight.spans);
            }
        }
        self.coalesce();
        self
    }

    /// Whether an upload may start at `now_s`.
    pub fn ready(&self, now_s: i64) -> bool {
        self.paused.is_none()
            && self.inflight.is_none()
            && self.cooldown_until <= now_s
            && self.spans.iter().any(|span| span.duration > 0)
    }

    /// Add the stays of one book. Adjacent or overlapping stays merge, so a
    /// reader who turns pages without a break yields one span per batch.
    pub fn enqueue(&mut self, book_id: &str, stays: &[StaySpan]) {
        if book_id.is_empty() {
            return;
        }
        let spans: Vec<QueuedSpan> = stays
            .iter()
            .filter_map(|stay| StaySpan::new(stay.start_time, stay.duration))
            .map(|stay| QueuedSpan {
                book_id: book_id.to_string(),
                start_time: stay.start_time,
                duration: stay.duration,
            })
            .collect();
        if spans.is_empty() {
            return;
        }
        self.spans.extend(spans);
        self.coalesce();
    }

    /// Seconds waiting for a first send.
    pub fn pending_seconds(&self) -> i64 {
        self.spans.iter().map(|span| span.duration).sum()
    }

    /// Seconds of one book waiting for a first send.
    pub fn pending_seconds_of(&self, book_id: &str) -> i64 {
        self.spans
            .iter()
            .filter(|span| span.book_id == book_id)
            .map(|span| span.duration)
            .sum()
    }

    /// Seconds sent without a receipt.
    pub fn unconfirmed_seconds(&self) -> i64 {
        self.unconfirmed.iter().map(|span| span.duration).sum()
    }

    /// Books with pending spans, earliest span first.
    pub fn pending_books(&self) -> Vec<String> {
        let mut books: Vec<(i64, String)> = Vec::new();
        for span in &self.spans {
            match books.iter_mut().find(|(_, book)| *book == span.book_id) {
                Some((first, _)) => *first = (*first).min(span.start_time),
                None => books.push((span.start_time, span.book_id.clone())),
            }
        }
        books.sort();
        books.into_iter().map(|(_, book)| book).collect()
    }

    /// Take up to `max_secs` seconds of one book out of the pending spans.
    ///
    /// A span longer than the room left is split: the taken part goes into the
    /// batch, the remainder stays queued.
    pub fn take_batch(&mut self, book_id: &str, max_secs: i64) -> Option<Vec<QueuedSpan>> {
        if max_secs <= 0 || self.inflight.is_some() {
            return None;
        }
        let mut batch = Vec::new();
        let mut left = max_secs.min(MAX_BATCH_SECS);
        let mut index = 0;
        while index < self.spans.len() && left > 0 {
            if self.spans[index].book_id != book_id {
                index += 1;
                continue;
            }
            if self.spans[index].duration <= left {
                left -= self.spans[index].duration;
                batch.push(self.spans.remove(index));
            } else {
                let taken = left;
                batch.push(QueuedSpan {
                    book_id: self.spans[index].book_id.clone(),
                    start_time: self.spans[index].start_time,
                    duration: taken,
                });
                self.spans[index].start_time = self.spans[index].start_time.saturating_add(taken);
                self.spans[index].duration -= taken;
                left = 0;
            }
        }
        (!batch.is_empty()).then_some(batch)
    }

    /// Mark `batch` as in flight; false when another batch already is.
    pub fn begin_send(&mut self, batch: Vec<QueuedSpan>, sent_at: i64) -> bool {
        if self.inflight.is_some() || batch.is_empty() {
            return false;
        }
        self.inflight = Some(InflightBatch {
            sent_at,
            spans: batch,
        });
        true
    }

    /// A `succ:1` arrived: the in-flight batch leaves the queue.
    pub fn confirm(&mut self) -> Option<InflightBatch> {
        self.inflight.take()
    }

    /// No receipt: keep the batch as unconfirmed; it is never taken again.
    pub fn unconfirm(&mut self) -> Option<InflightBatch> {
        let batch = self.inflight.take()?;
        self.unconfirmed.extend(batch.spans.iter().cloned());
        Some(batch)
    }

    /// The request provably never left: put the batch back in front.
    pub fn return_inflight(&mut self) -> Option<InflightBatch> {
        let batch = self.inflight.take()?;
        self.spans.extend(batch.spans.iter().cloned());
        self.coalesce();
        Some(batch)
    }

    /// Stop uploading until [`ReadtimeQueue::resume`].
    pub fn pause(&mut self, reason: PauseReason) {
        self.paused = Some(reason);
    }

    /// Clear the pause and any cooldown; true when something was paused.
    pub fn resume(&mut self) -> bool {
        let was_paused = self.paused.take().is_some();
        self.cooldown_until = 0;
        was_paused
    }

    /// Sort spans by start time and merge adjacent stays of the same book.
    fn coalesce(&mut self) {
        self.spans.sort_by_key(|span| span.start_time);
        let mut merged: Vec<QueuedSpan> = Vec::with_capacity(self.spans.len());
        for span in self.spans.drain(..) {
            if let Some(last) = merged.last_mut() {
                let joined = span.start_time <= last.end_time()
                    && span.end_time() > last.end_time()
                    && span.book_id == last.book_id;
                if joined && span.end_time() - last.start_time <= MAX_SPAN_SECS {
                    last.duration = span.end_time() - last.start_time;
                    continue;
                }
                // A stay fully inside the previous one adds nothing.
                if span.book_id == last.book_id && span.end_time() <= last.end_time() {
                    continue;
                }
            }
            merged.push(span);
        }
        self.spans = merged;
    }
}

/// Drop invalid spans, clamp long ones and merge the rest.
fn sanitized(spans: Vec<QueuedSpan>) -> Vec<QueuedSpan> {
    spans
        .into_iter()
        .filter(|span| !span.book_id.is_empty() && span.duration > 0)
        .map(|mut span| {
            span.duration = span.duration.min(MAX_SPAN_SECS);
            span
        })
        .collect()
}

/// The §14.5 reading intervals of a batch, in the queue's order.
pub fn intervals_for(spans: &[QueuedSpan]) -> Vec<ReadingInterval> {
    spans
        .iter()
        .filter(|span| span.duration > 0)
        .map(|span| {
            ReadingInterval::new(
                span.start_time,
                span.duration.min(MAX_SPAN_SECS),
                READ_TIME_ZONE,
            )
        })
        .collect()
}

/// The GMT+8 hour buckets the batch uploads as; this is what the signature
/// covers, so the caller can assert it without sending.
pub fn hours_for(spans: &[QueuedSpan]) -> Result<Vec<ReadingHour>, WeReadError> {
    split_reading_hours(&intervals_for(spans))
}

#[cfg(test)]
#[path = "readtime_tests.rs"]
mod tests;
