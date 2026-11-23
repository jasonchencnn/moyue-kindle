// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests of the §14.6 reading-time mapping and the persistent queue.

use super::*;
use crate::weread::client::{build_batch_upload_body, BatchUploadRequest};
use crate::weread::{BatchUploadBody, Credentials};

fn credentials() -> Credentials {
    Credentials::new(
        "1234567890",
        "access-token-fixture",
        "refresh-token-fixture",
        "eink-test-device-0001",
    )
}

fn body(spans: &[QueuedSpan]) -> BatchUploadBody {
    let mut request = BatchUploadRequest::new(
        "100001",
        42,
        intervals_for(spans),
        1_795_006_800,
        READ_TIME_ZONE,
    );
    request.random = 123;
    request.outer_timestamp_ms = 1_795_006_800_000;
    request.outer_random = 456;
    build_batch_upload_body(
        &credentials(),
        "config-token-fixture",
        "guest-token-fixture",
        &request,
    )
    .expect("upload body")
}

fn span(book: &str, start: i64, duration: i64) -> QueuedSpan {
    QueuedSpan {
        book_id: book.to_string(),
        start_time: start,
        duration,
    }
}

#[test]
fn the_wire_zone_is_gmt8_and_the_hours_cross_its_boundary() {
    // 13:30 UTC = 21:30 GMT+8; 2 400 s run into the next hour bucket.
    let hours = hours_for(&[span("100001", 1_795_008_600, 2_400)]).expect("hours");
    assert_eq!(hours.len(), 2);
    assert_eq!(hours[0].start_of_hour, 1_795_006_800);
    assert_eq!(hours[0].time_zone, READ_TIME_ZONE);
    assert_eq!(hours[0].reading_time, 1_800);
    assert_eq!(hours[0].tts_time, 0);
    assert_eq!(hours[1].start_of_hour, 1_795_010_400);
    assert_eq!(hours[1].reading_time, 600);
    // UNIX hour boundaries and GMT+8 hour boundaries are the same instants.
    assert_eq!(hours[0].start_of_hour % 3_600, 0);
    assert_eq!(hours[1].start_of_hour % 3_600, 0);
    // The body of the same spans carries both buckets in its signature.
    let body = body(&[span("100001", 1_795_008_600, 2_400)]);
    let book = &body.books[0];
    assert_eq!(book.reading_time, 2_400);
    assert_eq!(book.hours.len(), 2);
    assert_eq!(book.hours[0].reading_time, 1_800);
    assert_eq!(book.hours[1].reading_time, 600);
    assert_eq!(
        book.signature,
        "316fe9981f6cd2fbc21f523018bb944297a0e21ad91a501103014f477a7d5292"
    );
}

#[test]
fn a_span_over_an_hour_is_truncated_before_it_is_queued() {
    let stay = StaySpan::new(1_795_008_600, 5_400).expect("stay");
    assert_eq!(stay.duration, MAX_SPAN_SECS);
    let mut queue = ReadtimeQueue::new();
    queue.enqueue("100001", &[stay]);
    let hours = hours_for(&queue.spans).expect("hours");
    assert_eq!(
        hours.iter().map(|hour| hour.reading_time).sum::<i64>(),
        3_600
    );
    assert_eq!(hours[0].reading_time, 1_800);
    assert_eq!(hours[1].reading_time, 1_800);
    // An invalid stay is not a span at all.
    assert_eq!(StaySpan::new(1_795_008_600, 0), None);
}

#[test]
fn the_batch_body_matches_the_golden_vector() {
    // A ten-minute batch: 300 s + 300 s, one GMT+8 hour bucket.
    let batch = [
        span("100001", 1_795_008_600, 300),
        span("100001", 1_795_008_900, 300),
    ];
    let body = body(&batch);
    let book = &body.books[0];
    assert_eq!(book.reading_time, 600);
    assert_eq!(book.hours.len(), 1);
    assert_eq!(book.hours[0].start_of_hour, 1_795_006_800);
    assert_eq!(book.hours[0].reading_time, 600);
    assert_eq!(
        book.signature,
        "ddd3a1dbcb83b58f7752c82e5f16d52597f6d1d6ee63c5a708b581f37f44c483"
    );
    // The outer signature covers the same fixed timestamp/config pair as WR-001.
    assert_eq!(
        book.signature.len(),
        64,
        "a SHA-256 hex signature, not the outer one"
    );
}

#[test]
fn a_truncated_hour_still_matches_its_golden_vector() {
    let batch = [span("100001", 1_795_008_600, MAX_SPAN_SECS)];
    let body = body(&batch);
    let book = &body.books[0];
    assert_eq!(book.reading_time, 3_600);
    assert_eq!(book.hours.len(), 2);
    assert_eq!(
        book.signature,
        "7e17586ad40c699393182c39d3e03a48a3ec403547995cbb10e160a18e55c3ff"
    );
}

#[test]
fn adjacent_stays_merge_and_an_overlap_counts_once() {
    let mut queue = ReadtimeQueue::new();
    queue.enqueue(
        "100001",
        &[
            StaySpan::new(1_000, 60).expect("stay"),
            StaySpan::new(1_060, 60).expect("stay"),
        ],
    );
    assert_eq!(queue.spans.len(), 1);
    assert_eq!(queue.spans[0].duration, 120);
    assert_eq!(queue.pending_seconds(), 120);
    // A stay inside the merged one must not add its seconds twice.
    queue.enqueue(
        "100001",
        &[
            StaySpan::new(1_000, 60).expect("stay"),
            StaySpan::new(1_040, 60).expect("stay"),
        ],
    );
    assert_eq!(queue.spans.len(), 1);
    assert_eq!(queue.pending_seconds(), 120);
    // Another book never merges into the first.
    queue.enqueue("100002", &[StaySpan::new(1_060, 30).expect("stay")]);
    assert_eq!(queue.spans.len(), 2);
}

#[test]
fn take_batch_caps_at_six_hundred_and_splits_the_remainder() {
    let mut queue = ReadtimeQueue::new();
    queue.enqueue("100001", &[StaySpan::new(0, 1_500).expect("stay")]);
    let first = queue.take_batch("100001", MAX_BATCH_SECS).expect("batch");
    assert_eq!(first.iter().map(|span| span.duration).sum::<i64>(), 600);
    assert_eq!(queue.pending_seconds(), 900);
    let second = queue.take_batch("100001", MAX_BATCH_SECS).expect("batch");
    assert_eq!(second[0].start_time, 600);
    assert_eq!(queue.pending_seconds(), 300);
    let third = queue.take_batch("100001", MAX_BATCH_SECS).expect("batch");
    assert_eq!(third[0].duration, 300);
    assert_eq!(queue.pending_seconds(), 0);
    assert!(queue.take_batch("100001", MAX_BATCH_SECS).is_none());
    // An in-flight batch blocks another one.
    assert!(queue.begin_send(third, 1_795_006_800));
    assert!(queue.take_batch("100001", MAX_BATCH_SECS).is_none());
}

#[test]
fn only_a_receipt_clears_the_batch_and_unconfirmed_is_never_taken_again() {
    let mut queue = ReadtimeQueue::new();
    queue.enqueue("100001", &[StaySpan::new(0, 600).expect("stay")]);
    let batch = queue.take_batch("100001", MAX_BATCH_SECS).expect("batch");
    assert!(queue.begin_send(batch, 1_795_006_800));
    assert!(queue.confirm().is_some());
    assert_eq!(queue.pending_seconds(), 0);
    assert_eq!(queue.unconfirmed_seconds(), 0);

    queue.enqueue("100001", &[StaySpan::new(1_000, 600).expect("stay")]);
    let batch = queue.take_batch("100001", MAX_BATCH_SECS).expect("batch");
    assert!(queue.begin_send(batch, 1_795_006_800));
    let kept = queue.unconfirm().expect("unconfirmed batch");
    assert_eq!(kept.seconds(), 600);
    assert_eq!(queue.pending_seconds(), 0);
    assert_eq!(queue.unconfirmed_seconds(), 600);
    assert!(queue.take_batch("100001", MAX_BATCH_SECS).is_none());
    // The unconfirmed seconds survive a save/load round trip.
    let encoded = serde_json::to_vec(&queue).expect("json");
    let decoded: ReadtimeQueue = serde_json::from_slice(&encoded).expect("decode");
    assert_eq!(decoded.unconfirmed_seconds(), 600);
}

#[test]
fn a_batch_left_in_flight_is_unconfirmed_after_a_reload() {
    let mut queue = ReadtimeQueue::new();
    queue.enqueue("100001", &[StaySpan::new(0, 300).expect("stay")]);
    let batch = queue.take_batch("100001", MAX_BATCH_SECS).expect("batch");
    assert!(queue.begin_send(batch, 1_795_006_800));
    let encoded = serde_json::to_vec(&queue).expect("json");
    let decoded: ReadtimeQueue = serde_json::from_slice(&encoded).expect("decode");
    let mut repaired = decoded.normalized();
    assert!(repaired.inflight.is_none());
    assert_eq!(repaired.pending_seconds(), 0);
    assert_eq!(repaired.unconfirmed_seconds(), 300);
    assert!(repaired.take_batch("100001", MAX_BATCH_SECS).is_none());
}

#[test]
fn a_batch_that_never_left_is_returned_to_the_queue() {
    let mut queue = ReadtimeQueue::new();
    queue.enqueue("100001", &[StaySpan::new(0, 1_500).expect("stay")]);
    let batch = queue.take_batch("100001", MAX_BATCH_SECS).expect("batch");
    assert!(queue.begin_send(batch, 1_795_006_800));
    assert_eq!(queue.pending_seconds(), 900);
    let returned = queue.return_inflight().expect("returned");
    assert_eq!(returned.seconds(), 600);
    assert_eq!(queue.pending_seconds(), 1_500, "merged back into one span");
    assert_eq!(queue.unconfirmed_seconds(), 0);
}

#[test]
fn pause_cooldown_and_readiness_follow_the_clock() {
    let mut queue = ReadtimeQueue::new();
    queue.enqueue("100001", &[StaySpan::new(0, 60).expect("stay")]);
    assert!(queue.ready(10));
    queue.pause(PauseReason::RemoteMoved);
    assert!(!queue.ready(10));
    assert_eq!(queue.paused, Some(PauseReason::RemoteMoved));
    assert!(queue.resume());
    assert!(queue.ready(10));
    queue.cooldown_until = 100;
    assert!(!queue.ready(99));
    assert!(queue.ready(100));
    assert!(
        !queue.resume(),
        "no pause to clear, but the cooldown is gone"
    );
    assert!(queue.ready(10));
}

#[test]
fn pending_books_are_ordered_by_their_earliest_span() {
    let mut queue = ReadtimeQueue::new();
    queue.enqueue("book-b", &[StaySpan::new(200, 30).expect("stay")]);
    queue.enqueue("book-a", &[StaySpan::new(300, 30).expect("stay")]);
    queue.enqueue("book-a", &[StaySpan::new(100, 30).expect("stay")]);
    assert_eq!(
        queue.pending_books(),
        vec!["book-a".to_string(), "book-b".to_string()]
    );
}

#[test]
fn a_loaded_queue_drops_invalid_spans_and_clamps_long_ones() {
    let queue = ReadtimeQueue {
        spans: vec![
            span("", 0, 60),
            span("book", 0, 0),
            span("book", 10, 9_999),
            span("book", 5_000, 60),
        ],
        ..ReadtimeQueue::new()
    };
    let queue = queue.normalized();
    assert_eq!(queue.spans.len(), 2);
    assert_eq!(queue.spans[0].duration, MAX_SPAN_SECS);
    assert_eq!(queue.spans[1].duration, 60);
}
