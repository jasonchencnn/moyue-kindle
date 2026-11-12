// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading statistics: how long each page stayed on the panel.
//!
//! A page turn only hands one [`PageStay`] to a writer thread through a
//! channel, so the turn never writes to the FAT userstore and never waits
//! for a lock held by the layout engine. The thread writes JSON Lines in
//! batches, appended to one file per month (`stats-2026-11.jsonl`) in the
//! library data directory.
//!
//! The record is Readest's `statPages` row, field for field
//! (`book_hash`, `page`, `start_time`, `duration`, `total_pages`), so S4 can
//! push it as it is. `total_pages` is kept per stay because a change of font
//! size changes the pagination; [`Store::progress`] compares stays by their
//! share of the book rather than by raw page numbers.
//!
//! # Same page twice
//!
//! KOReader's statistics plugin keeps a list of `{ timestamp, duration }`
//! tuples per page (`plugins/statistics.koplugin/main.lua`, `page_stat`) and
//! inserts one row per visit, `UNIQUE (id_book, page, start_time)`. We do the
//! same: every visit is a record of its own, and [`Store::records`] drops
//! rows with a duplicate `(book_hash, page, start_time)`.
//!
//! # Durability
//!
//! Appends never rewrite or truncate an existing line, so the worst a power
//! cut can do is leave an incomplete last line; the reader skips any line
//! that does not parse. Each batch is `fsync`ed after the write, so all
//! batches but the last are on the card. No `.bak` rotation is needed:
//! [`crate::progress`] needs one because it replaces a whole file, while an
//! append cannot damage the bytes before it. (FAT can still lose clusters on
//! a yank, as it can for any file.)

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::speed::{MAX_SECS, MIN_SECS};

/// Records held before the writer thread starts a batch.
pub const FLUSH_RECORDS: usize = 32;
/// Longest a record waits for company before the writer thread writes it.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
/// Pending records the writer thread keeps when the disk refuses them.
const MAX_PENDING: usize = 4096;

/// One stay on a page, the same row Readest calls a `statPage`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageStay {
    /// KOReader-compatible partial MD5, as in the library index.
    pub book_hash: String,
    /// 1-based page number in the layout it was read in.
    pub page: u32,
    /// Unix seconds when the page appeared.
    pub start_time: i64,
    /// Seconds spent on it, between 5 s and 10 min.
    pub duration: u32,
    /// Pages the book had in that layout.
    pub total_pages: u32,
}

impl PageStay {
    /// A stay, or `None` when the duration is outside the shared limits
    /// (shorter is a skim or a double tap, longer is a break).
    pub fn new(
        book_hash: impl Into<String>,
        page: u32,
        total_pages: u32,
        start_time: i64,
        secs: f32,
    ) -> Option<PageStay> {
        if !(MIN_SECS..=MAX_SECS).contains(&secs) || page == 0 || total_pages == 0 {
            return None;
        }
        Some(PageStay {
            book_hash: book_hash.into(),
            page: page.min(total_pages),
            start_time,
            duration: secs as u32,
            total_pages,
        })
    }
}

/// What the reader tells the recorder: the page on the panel now.
struct Shown {
    book_hash: String,
    page: u32,
    total_pages: u32,
    since: Instant,
    started: i64,
}

enum Command {
    Record(PageStay),
    Flush(Sender<()>),
    Stop,
}

/// The reader's handle on the statistics. One long-lived writer thread owns
/// the files; the reader only sends it records.
pub struct Stats {
    tx: Option<Sender<Command>>,
    thread: Option<thread::JoinHandle<()>>,
    shown: Option<Shown>,
    #[cfg(test)]
    recording: bool,
}

impl Stats {
    /// Start the writer thread for the data directory `dir`.
    pub fn new(dir: &Path) -> Stats {
        let dir = dir.to_path_buf();
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("stats".into())
            .spawn(move || writer(dir, rx))
            .ok();
        Stats {
            tx: Some(tx),
            thread,
            shown: None,
            #[cfg(test)]
            recording: true,
        }
    }

    /// Start timing a page that has just appeared (a book opened, a wake).
    pub fn page_shown(
        &mut self,
        book_hash: &str,
        page: u32,
        total_pages: u32,
        now: Instant,
        wall: i64,
    ) {
        #[cfg(test)]
        if !self.recording {
            return;
        }
        self.shown = Some(Shown {
            book_hash: book_hash.to_string(),
            page,
            total_pages,
            since: now,
            started: wall,
        });
    }

    /// Record the page just left and start timing `page`.
    ///
    /// This is the only call on the page-turn path. It computes the stay and
    /// hands it to the writer thread (one short lock on the channel, no file
    /// I/O).
    pub fn record_page(
        &mut self,
        book_hash: &str,
        page: u32,
        total_pages: u32,
        now: Instant,
        wall: i64,
    ) {
        #[cfg(test)]
        if !self.recording {
            return;
        }
        self.finish_page(now);
        self.page_shown(book_hash, page, total_pages, now, wall);
    }

    /// Record the page on the panel and forget it (sleep, leaving the book).
    pub fn finish_page(&mut self, now: Instant) {
        let Some(shown) = self.shown.take() else {
            return;
        };
        let secs = now.saturating_duration_since(shown.since).as_secs_f32();
        if let Some(stay) = PageStay::new(
            shown.book_hash,
            shown.page,
            shown.total_pages,
            shown.started,
            secs,
        ) {
            if let Some(tx) = &self.tx {
                let _ = tx.send(Command::Record(stay));
            }
        }
    }

    /// Write everything queued so far and wait for it (sleep, back to the
    /// library, quit). Not for the page-turn path.
    pub fn flush(&self) {
        let Some(tx) = &self.tx else {
            return;
        };
        let (ack, done) = mpsc::channel();
        if tx.send(Command::Flush(ack)).is_ok() {
            // The writer is a local thread; a second is far more than a
            // batch of appends needs.
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
    }

    /// Let tests turn recording off to measure the turn path without it.
    #[cfg(test)]
    pub fn set_recording(&mut self, recording: bool) {
        self.recording = recording;
        if !recording {
            self.shown = None;
        }
    }

    /// Move the start of the current stay back, so the next turn records it
    /// without a five-second wait (benchmarks only).
    #[cfg(test)]
    pub fn age_page(&mut self, secs: u64) {
        if let Some(shown) = &mut self.shown {
            shown.since = shown
                .since
                .checked_sub(Duration::from_secs(secs))
                .unwrap_or(shown.since);
        }
    }

    /// Hand a record to the writer without timing (tests).
    #[cfg(test)]
    pub fn record_at(&self, stay: PageStay) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Command::Record(stay));
        }
    }
}

impl Drop for Stats {
    fn drop(&mut self) {
        self.finish_page(Instant::now());
        self.flush();
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Command::Stop);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn writer(dir: PathBuf, rx: Receiver<Command>) {
    crate::pagecache::lower_priority();
    let mut pending: Vec<PageStay> = Vec::new();
    let mut first: Option<Instant> = None;
    loop {
        let timeout = first.map_or(FLUSH_INTERVAL, |t| {
            FLUSH_INTERVAL.saturating_sub(t.elapsed())
        });
        match rx.recv_timeout(timeout) {
            Ok(Command::Record(stay)) => {
                pending.push(stay);
                first.get_or_insert_with(Instant::now);
                if pending.len() >= FLUSH_RECORDS {
                    write_pending(&dir, &mut pending);
                    first = None;
                }
            }
            Ok(Command::Flush(ack)) => {
                write_pending(&dir, &mut pending);
                first = None;
                let _ = ack.send(());
            }
            Ok(Command::Stop) => {
                write_pending(&dir, &mut pending);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                write_pending(&dir, &mut pending);
                first = None;
            }
            Err(RecvTimeoutError::Disconnected) => {
                write_pending(&dir, &mut pending);
                break;
            }
        }
    }
}

/// Append the pending records, grouped by the month they started in. On a
/// failure the records stay for the next flush, up to [`MAX_PENDING`].
fn write_pending(dir: &Path, pending: &mut Vec<PageStay>) {
    if pending.is_empty() {
        return;
    }
    let mut by_file: BTreeMap<PathBuf, Vec<PageStay>> = BTreeMap::new();
    for stay in pending.drain(..) {
        by_file
            .entry(dir.join(file_name(date_of(stay.start_time))))
            .or_default()
            .push(stay);
    }
    let mut failed = Vec::new();
    for (path, stays) in by_file {
        if let Err(e) = append(&path, &stays) {
            eprintln!("aristotle: can't write {}: {e}", path.display());
            failed.extend(stays);
        }
    }
    if !failed.is_empty() {
        let keep = failed.len().min(MAX_PENDING);
        pending.extend(failed[failed.len() - keep..].iter().cloned());
    }
}

fn append(path: &Path, stays: &[PageStay]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut buf = Vec::with_capacity(stays.len() * 96);
    for stay in stays {
        serde_json::to_writer(&mut buf, stay)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        buf.push(b'\n');
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&buf)?;
    file.sync_data()
}

/// `stats-2026-11.jsonl`: one file per month of local time.
fn file_name(date: LocalDate) -> String {
    format!("stats-{:04}-{:02}.jsonl", date.year, date.month)
}

// ---------------------------------------------------------------- reading

/// Reading side of the store, for the statistics screens. It reads whole
/// files; the caller decides when to refresh.
pub struct Store {
    dir: PathBuf,
}

// The reading calendar and the "mine" screen read the store; the rest of
// the methods are for the screens that come (the book statistics and the
// daily figures) and for the tests.
#[allow(dead_code)]
impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    /// Every stay, oldest file first, with duplicate
    /// `(book_hash, page, start_time)` rows removed. A truncated last line
    /// (power loss) or any other unreadable line is skipped.
    pub fn records(&self) -> Vec<PageStay> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("stats-") && n.ends_with(".jsonl"))
            })
            .collect();
        files.sort();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for path in files {
            let Ok(data) = fs::read(&path) else {
                continue;
            };
            for line in data.split(|&b| b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let Ok(stay) = serde_json::from_slice::<PageStay>(line) else {
                    continue;
                };
                if seen.insert((stay.book_hash.clone(), stay.page, stay.start_time)) {
                    out.push(stay);
                }
            }
        }
        out
    }

    /// Seconds read per local calendar day.
    pub fn daily_totals(&self) -> BTreeMap<LocalDate, u64> {
        daily_totals_by(&self.records(), &|ts| date_of(ts))
    }

    /// Time, days and pages for each book.
    pub fn book_totals(&self) -> BTreeMap<String, BookTotals> {
        book_totals_by(&self.records(), &|ts| date_of(ts))
    }

    /// Consecutive days with reading up to today, or up to yesterday when
    /// nothing has been read yet today (a streak is not broken before the
    /// day is over). Zero when neither today nor yesterday has a record.
    pub fn current_streak(&self, now: i64) -> u32 {
        current_streak_by(&self.records(), date_of(now), &|ts| date_of(ts))
    }

    /// Seconds read today (local midnight to now).
    pub fn seconds_today(&self, now: i64) -> u64 {
        let today = date_of(now);
        seconds_on(&self.records(), today, &|ts| date_of(ts))
    }

    /// Seconds read today, from the file of this month only. The home
    /// screen reads this once when it opens, and the older files are none
    /// of its business.
    pub fn seconds_today_this_month(&self, now: i64) -> u64 {
        let today = date_of(now);
        let records = self.records_in_month(today.year, today.month);
        seconds_on(&records, today, &|ts| date_of(ts))
    }

    /// The stays of one month, from its file alone.
    fn records_in_month(&self, year: i32, month: u8) -> Vec<PageStay> {
        let path = self.dir.join(file_name(LocalDate {
            year,
            month,
            day: 1,
        }));
        let Ok(data) = fs::read(&path) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(stay) = serde_json::from_slice::<PageStay>(line) else {
                continue;
            };
            if seen.insert((stay.book_hash.clone(), stay.page, stay.start_time)) {
                out.push(stay);
            }
        }
        out
    }

    /// Seconds read this week, Monday up to today (KOReader's default week).
    pub fn seconds_this_week(&self, now: i64) -> u64 {
        seconds_this_week_by(&self.records(), date_of(now), &|ts| date_of(ts))
    }

    /// How far into the book the furthest stay got, as a share of the
    /// pagination it was read in. Comparable across font-size changes.
    pub fn progress(&self, book_hash: &str) -> Option<f32> {
        progress_by(&self.records(), book_hash)
    }

    /// How long the rest of `book_hash` will take at the reader's own pace;
    /// see [`estimate_minutes_left`].
    pub fn estimate_minutes_left(&self, book_hash: &str, fraction: f32) -> Option<u32> {
        estimate_minutes_left(&self.records(), book_hash, fraction)
    }

    /// The earliest local calendar day with a stay for `book_hash`.
    pub fn book_start_date(&self, book_hash: &str) -> Option<LocalDate> {
        book_start_date(&self.records(), book_hash, &|ts| date_of(ts))
    }

    /// Seconds read per day for `book_hash` over the 7 days ending on `today`.
    pub fn book_recent_daily_seconds(
        &self,
        book_hash: &str,
        today: LocalDate,
    ) -> Vec<(LocalDate, u64)> {
        book_recent_daily_seconds(&self.records(), book_hash, today, &|ts| date_of(ts))
    }

    /// Predicts completion date and remaining days based on recent 14-day pace.
    pub fn book_estimate_completion(
        &self,
        book_hash: &str,
        today: LocalDate,
        remaining_pages: u32,
    ) -> Option<(LocalDate, u32)> {
        book_estimate_completion(&self.records(), book_hash, today, remaining_pages, &|ts| {
            date_of(ts)
        })
    }
}

/// Totals for one book.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BookTotals {
    /// Seconds read, as recorded (each stay at most 10 min).
    pub duration: u64,
    /// Days with at least one stay.
    pub days: u32,
    /// Pages read: distinct `(page, total_pages)` pairs, so a page read in
    /// two layouts counts twice and the same page twice counts once.
    pub pages: u32,
}

/// A local calendar day, the unit of every daily query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

/// Seconds read per day.
pub fn daily_totals_by(
    records: &[PageStay],
    date_of: &dyn Fn(i64) -> LocalDate,
) -> BTreeMap<LocalDate, u64> {
    let mut out = BTreeMap::new();
    for stay in records {
        *out.entry(date_of(stay.start_time)).or_insert(0) += stay.duration as u64;
    }
    out
}

/// Totals per book.
pub fn book_totals_by(
    records: &[PageStay],
    date_of: &dyn Fn(i64) -> LocalDate,
) -> BTreeMap<String, BookTotals> {
    struct Acc {
        totals: BookTotals,
        days: BTreeSet<LocalDate>,
        pages: HashSet<(u32, u32)>,
    }
    let mut books: BTreeMap<String, Acc> = BTreeMap::new();
    for stay in records {
        let acc = books.entry(stay.book_hash.clone()).or_insert_with(|| Acc {
            totals: BookTotals::default(),
            days: BTreeSet::new(),
            pages: HashSet::new(),
        });
        acc.totals.duration += stay.duration as u64;
        acc.days.insert(date_of(stay.start_time));
        acc.pages.insert((stay.page, stay.total_pages));
    }
    books
        .into_iter()
        .map(|(hash, acc)| {
            let totals = BookTotals {
                duration: acc.totals.duration,
                days: acc.days.len() as u32,
                pages: acc.pages.len() as u32,
            };
            (hash, totals)
        })
        .collect()
}

/// Consecutive days ending today or yesterday.
pub fn current_streak_by(
    records: &[PageStay],
    today: LocalDate,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> u32 {
    let days: BTreeSet<i64> = records
        .iter()
        .map(|s| days_from_civil(date_of(s.start_time)))
        .collect();
    let today = days_from_civil(today);
    let mut day = if days.contains(&today) {
        today
    } else if days.contains(&(today - 1)) {
        today - 1
    } else {
        return 0;
    };
    let mut streak = 0;
    while days.contains(&day) {
        streak += 1;
        day -= 1;
    }
    streak
}

/// Seconds read on `date`.
pub fn seconds_on(
    records: &[PageStay],
    date: LocalDate,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> u64 {
    records
        .iter()
        .filter(|s| date_of(s.start_time) == date)
        .map(|s| s.duration as u64)
        .sum()
}

/// Seconds read from Monday of `date`'s week through `date`.
pub fn seconds_this_week_by(
    records: &[PageStay],
    date: LocalDate,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> u64 {
    let today = days_from_civil(date);
    // 1970-01-01 was a Thursday; with Monday as 0 that is 3.
    let monday = today - (today + 3).rem_euclid(7);
    records
        .iter()
        .filter(|s| {
            let day = days_from_civil(date_of(s.start_time));
            (monday..=today).contains(&day)
        })
        .map(|s| s.duration as u64)
        .sum()
}

/// Seconds read per day of one month, in the local calendar. The days
/// without reading are not in the map.
pub fn month_daily_seconds(
    records: &[PageStay],
    year: i32,
    month: u8,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> BTreeMap<LocalDate, u64> {
    let mut out = BTreeMap::new();
    for stay in records {
        let date = date_of(stay.start_time);
        if date.year == year && date.month == month {
            *out.entry(date).or_insert(0) += stay.duration as u64;
        }
    }
    out
}

/// What a month adds up to on the calendar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonthTotals {
    /// The minutes read in the month: the sum of the minutes of its days.
    pub minutes: u32,
    /// The days with at least one stay.
    pub days: u32,
}

/// The totals of a month, from the seconds read per day. The minutes of a
/// day are rounded up, as the screen shows them, and the month adds up
/// what its days show.
pub fn month_totals(daily: &BTreeMap<LocalDate, u64>) -> MonthTotals {
    let mut totals = MonthTotals::default();
    for seconds in daily.values() {
        if *seconds > 0 {
            totals.minutes += ((*seconds + 59) / 60) as u32;
            totals.days += 1;
        }
    }
    totals
}

/// The seconds read per book on one day, the most read first.
pub fn day_book_seconds(
    records: &[PageStay],
    date: LocalDate,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> Vec<(String, u64)> {
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    for stay in records {
        if date_of(stay.start_time) == date {
            *totals.entry(stay.book_hash.clone()).or_insert(0) += stay.duration as u64;
        }
    }
    let mut out: Vec<(String, u64)> = totals.into_iter().collect();
    // The most read first; the hash breaks a tie, so the order is stable.
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The month of the earliest stay: the first month the calendar shows.
pub fn earliest_month(
    records: &[PageStay],
    date_of: &dyn Fn(i64) -> LocalDate,
) -> Option<(i32, u8)> {
    records
        .iter()
        .map(|s| date_of(s.start_time))
        .map(|d| (d.year, d.month))
        .min()
}

/// Furthest stay as a share of its own pagination.
pub fn progress_by(records: &[PageStay], book_hash: &str) -> Option<f32> {
    records
        .iter()
        .filter(|s| s.book_hash == book_hash && s.total_pages > 0)
        .map(|s| s.page.min(s.total_pages) as f32 / s.total_pages as f32)
        .fold(None, |max, f| Some(max.map_or(f, |m: f32| m.max(f))))
}

/// The earliest local calendar day with a stay for `book_hash`.
pub fn book_start_date(
    records: &[PageStay],
    book_hash: &str,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> Option<LocalDate> {
    records
        .iter()
        .filter(|s| s.book_hash == book_hash)
        .map(|s| date_of(s.start_time))
        .min()
}

/// The average time per page as `(minutes, seconds)`, rounded to the second.
/// Returns `None` when no pages have been read.
pub fn book_avg_seconds_per_page(totals: &BookTotals) -> Option<(u32, u32)> {
    if totals.pages == 0 {
        return None;
    }
    let secs = totals.duration as f64 / totals.pages as f64;
    let mins = (secs / 60.0).floor() as u32;
    let rem_secs = (secs % 60.0).round() as u32;
    if rem_secs == 60 {
        Some((mins + 1, 0))
    } else {
        Some((mins, rem_secs))
    }
}

/// How many minutes are left in a book, from how fast the reader has been
/// going in it.
///
/// The pages left come from the last layout the reader saw (`total_pages`)
/// and the book's progress. The pace is the recorded reading time divided
/// by the distinct pages read. `None` when the book has no recorded stays,
/// the progress is not partial, or the estimate is under a minute: the home
/// screen then says nothing rather than guessing.
pub fn estimate_minutes_left(records: &[PageStay], book_hash: &str, fraction: f32) -> Option<u32> {
    if !(0.0..1.0).contains(&fraction) {
        return None;
    }
    let last = records
        .iter()
        .filter(|s| s.book_hash == book_hash && s.total_pages > 0)
        .max_by_key(|s| s.start_time)?;
    let total_pages = last.total_pages as f64;
    let mut duration = 0u64;
    let mut pages = HashSet::new();
    for stay in records.iter().filter(|s| s.book_hash == book_hash) {
        duration += stay.duration as u64;
        pages.insert((stay.page, stay.total_pages));
    }
    if duration == 0 || pages.is_empty() {
        return None;
    }
    let secs_per_page = duration as f64 / pages.len() as f64;
    let remaining_pages = (total_pages * (1.0 - f64::from(fraction))).round();
    if remaining_pages < 1.0 {
        return None;
    }
    let minutes = (remaining_pages * secs_per_page / 60.0).ceil() as u32;
    (minutes > 0).then_some(minutes)
}

/// The seconds read per day for `book_hash` over the 7 days ending on `today`
/// (from `today - 6` up to `today`, inclusive).
pub fn book_recent_daily_seconds(
    records: &[PageStay],
    book_hash: &str,
    today: LocalDate,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> Vec<(LocalDate, u64)> {
    let mut by_day: BTreeMap<LocalDate, u64> = BTreeMap::new();
    for i in (0..7).rev() {
        let d = days_before(today, i);
        by_day.insert(d, 0);
    }
    for stay in records {
        if stay.book_hash == book_hash {
            let d = date_of(stay.start_time);
            if let Some(total) = by_day.get_mut(&d) {
                *total += stay.duration as u64;
            }
        }
    }
    by_day.into_iter().collect()
}

/// Predicts the completion date and remaining days based on the daily average
/// pages read in the last 14 days (from `today - 13` up to `today`, inclusive).
///
/// Returns `None` when there is no reading recorded in the last 14 days or
/// when `remaining_pages` is 0.
pub fn book_estimate_completion(
    records: &[PageStay],
    book_hash: &str,
    today: LocalDate,
    remaining_pages: u32,
    date_of: &dyn Fn(i64) -> LocalDate,
) -> Option<(LocalDate, u32)> {
    if remaining_pages == 0 {
        return None;
    }
    let start_day = days_before(today, 13);
    let today_days = days_from_civil(today);
    let start_days = days_from_civil(start_day);

    let mut pages_in_window = HashSet::new();
    for stay in records {
        if stay.book_hash == book_hash {
            let d = date_of(stay.start_time);
            let day_num = days_from_civil(d);
            if day_num >= start_days && day_num <= today_days {
                pages_in_window.insert((stay.page, stay.total_pages));
            }
        }
    }
    let pages_count = pages_in_window.len() as u32;
    if pages_count == 0 {
        return None;
    }

    let daily_avg = pages_count as f64 / 14.0;
    let days_left = (remaining_pages as f64 / daily_avg).round().max(1.0) as u32;
    let target_day = civil_from_days(today_days + days_left as i64);
    Some((target_day, days_left))
}

// ------------------------------------------------------------------ dates

/// The local calendar day of a Unix timestamp, from the local timezone
/// (`localtime_r`, so DST is respected).
pub fn date_of(unix: i64) -> LocalDate {
    date_at(unix, local_offset(unix))
}

/// The number of the local day of a Unix timestamp, counting from the
/// epoch. The day turns at local midnight, not at UTC midnight.
pub fn day_number(unix: i64) -> i64 {
    (unix + local_offset(unix)).div_euclid(86_400)
}

/// The calendar day of `unix` at a fixed UTC offset, in seconds. Tests use
/// this to pin a timezone; production goes through [`date_of`].
pub fn date_at(unix: i64, offset: i64) -> LocalDate {
    civil_from_days((unix + offset).div_euclid(86_400))
}

/// The day of the week of a date, 0 for Sunday. The days are counted
/// from the first of January 1970, which was a Thursday.
pub fn weekday_of(date: LocalDate) -> u32 {
    (days_from_civil(date).rem_euclid(7) as u32 + 4) % 7
}

/// The UTC offset the local timezone had at `unix`, in seconds.
fn local_offset(unix: i64) -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` writes a `struct tm` we own and only reads
        // `unix`; zeroing the struct first keeps padding defined.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // `time_t` is `c_long` on every target we build for; the alias
        // itself is deprecated while musl moves to 64 bits.
        let time = unix as libc::c_long;
        // SAFETY: both pointers are valid for the call's duration.
        if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
    #[cfg(not(unix))]
    {
        let _ = unix;
        0
    }
}

/// Days since 1970-01-01 for a calendar day (Howard Hinnant's algorithm).
fn days_from_civil(date: LocalDate) -> i64 {
    let y = date.year as i64 - i64::from(date.month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = date.month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + date.day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The calendar day `n` days before `date`.
pub fn days_before(date: LocalDate, n: i32) -> LocalDate {
    civil_from_days(days_from_civil(date) - i64::from(n))
}

/// The calendar day `days` after 1970-01-01.
fn civil_from_days(days: i64) -> LocalDate {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    LocalDate {
        year: (y + i64::from(month <= 2)) as i32,
        month: month as u8,
        day: day as u8,
    }
}

/// The number of days of a month.
pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Whether a year has a 29th of February.
fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The empty cells before the first of a month in a week that starts on
/// Monday: 0 for a month that starts on a Monday, 6 for one that starts
/// on a Sunday.
pub fn month_lead(year: i32, month: u8) -> u8 {
    // `weekday_of` counts Sunday as 0; a week that starts on Monday moves
    // it to the last column.
    let weekday = weekday_of(LocalDate {
        year,
        month,
        day: 1,
    });
    ((weekday + 6) % 7) as u8
}

/// The month `delta` months away, the year turning over in December and
/// January.
pub fn month_step(year: i32, month: u8, delta: i32) -> (i32, u8) {
    let total = year as i64 * 12 + (month as i64 - 1) + delta as i64;
    let year = total.div_euclid(12) as i32;
    let month = total.rem_euclid(12) as u8 + 1;
    (year, month)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stay(hash: &str, page: u32, total: u32, start: i64, secs: u32) -> PageStay {
        PageStay::new(hash, page, total, start, secs as f32).unwrap()
    }

    /// 2025-11-05 00:00:00 UTC, a Wednesday.
    const NOV5: i64 = 1_762_300_800;

    fn utc(ts: i64) -> LocalDate {
        date_at(ts, 0)
    }

    #[test]
    fn limits_are_the_reading_speed_ones() {
        assert!(PageStay::new("b", 1, 10, 0, 4.9).is_none());
        assert!(PageStay::new("b", 1, 10, 0, 5.0).is_some());
        assert!(PageStay::new("b", 1, 10, 0, 600.0).is_some());
        assert!(PageStay::new("b", 1, 10, 0, 600.1).is_none());
        assert!(PageStay::new("b", 0, 10, 0, 30.0).is_none());
        assert!(PageStay::new("b", 1, 0, 0, 30.0).is_none());
        // A page past a stale total is clamped, not dropped.
        assert_eq!(PageStay::new("b", 12, 10, 0, 30.0).unwrap().page, 10);
    }

    #[test]
    fn calendar_maths_round_trips_across_months_and_years() {
        for days in [
            days_from_civil(LocalDate {
                year: 1970,
                month: 1,
                day: 1,
            }),
            days_from_civil(LocalDate {
                year: 2024,
                month: 2,
                day: 29,
            }),
            days_from_civil(LocalDate {
                year: 2025,
                month: 12,
                day: 31,
            }),
            days_from_civil(LocalDate {
                year: 2026,
                month: 3,
                day: 1,
            }),
        ] {
            assert_eq!(days_from_civil(civil_from_days(days)), days);
        }
        assert_eq!(
            civil_from_days(0),
            LocalDate {
                year: 1970,
                month: 1,
                day: 1
            }
        );
    }

    #[test]
    fn local_midnight_starts_a_new_day_in_the_given_timezone() {
        // 2026-11-05 23:59 UTC is already 2026-11-06 in UTC+8.
        let late = NOV5 + 23 * 3600 + 59 * 60;
        assert_eq!(date_at(late, 0).day, 5);
        assert_eq!(date_at(late, 8 * 3600).day, 6);
        // And 00:01 UTC is still 2026-11-04 in UTC-5.
        assert_eq!(date_at(NOV5 + 60, -5 * 3600).day, 4);
    }

    fn sample() -> Vec<PageStay> {
        vec![
            // Day 1: two stays on the same page (re-read later).
            stay("a", 10, 100, NOV5, 60),
            stay("a", 10, 100, NOV5 + 3600, 30),
            stay("a", 20, 100, NOV5 + 7200, 40),
            stay("b", 5, 50, NOV5 + 7800, 20),
            // Day 2.
            stay("a", 30, 200, NOV5 + 86_400, 70),
            // Day 4, and across the new year.
            stay("a", 40, 200, NOV5 + 3 * 86_400, 10),
        ]
    }

    #[test]
    fn same_page_visits_are_separate_records() {
        let records = sample();
        let a10: Vec<_> = records
            .iter()
            .filter(|s| s.book_hash == "a" && s.page == 10)
            .collect();
        assert_eq!(a10.len(), 2, "each visit is kept");
        let deduped = Store::records_from(records.clone());
        assert_eq!(deduped.len(), records.len());
        let mut twice = records.clone();
        twice.push(records[0].clone());
        assert_eq!(
            Store::records_from(twice).len(),
            records.len(),
            "a duplicate row is dropped on read"
        );
    }

    #[test]
    fn daily_totals_use_local_days() {
        let totals = daily_totals_by(&sample(), &utc);
        assert_eq!(totals[&utc(NOV5)], 60 + 30 + 40 + 20);
        assert_eq!(totals[&utc(NOV5 + 86_400)], 70);
        assert_eq!(totals[&utc(NOV5 + 3 * 86_400)], 10);
        // 00:30 UTC+8 belongs to the next local day.
        let shifted = daily_totals_by(&[stay("a", 1, 10, NOV5 + 23 * 3600, 30)], &|ts| {
            date_at(ts, 8 * 3600)
        });
        assert!(shifted.contains_key(&date_at(NOV5 + 23 * 3600, 8 * 3600)));
    }

    #[test]
    fn book_totals_count_days_and_layouts() {
        let totals = book_totals_by(&sample(), &utc);
        let a = totals["a"];
        assert_eq!(a.duration, 60 + 30 + 40 + 70 + 10);
        assert_eq!(a.days, 3);
        // Page 10 twice counts once; page 30/200 is a different layout.
        assert_eq!(a.pages, 4);
        let b = totals["b"];
        assert_eq!(b.duration, 20);
        assert_eq!(b.days, 1);
        assert_eq!(b.pages, 1);
    }

    #[test]
    fn progress_is_normalised_by_each_layouts_page_count() {
        let records = [
            stay("a", 50, 100, NOV5, 30),
            stay("a", 120, 200, NOV5 + 60, 30),
            stay("a", 20, 50, NOV5 + 120, 30),
        ];
        let p = progress_by(&records, "a").unwrap();
        assert!((p - 0.6).abs() < 1e-6, "120/200 is the furthest: {p}");
        assert!(progress_by(&records, "missing").is_none());
    }

    #[test]
    fn streaks_cross_months_and_years_and_survive_a_day_in_progress() {
        let dec30 = days_from_civil(LocalDate {
            year: 2025,
            month: 12,
            day: 30,
        }) * 86_400;
        let records = [
            stay("a", 1, 10, dec30, 30),
            stay("a", 2, 10, dec30 + 86_400, 30),
            // New year's eve and day.
            stay("a", 3, 10, dec30 + 2 * 86_400, 30),
            stay("a", 4, 10, dec30 + 3 * 86_400, 30),
        ];
        let today = date_at(dec30 + 3 * 86_400, 0);
        assert_eq!(current_streak_by(&records, today, &utc), 4);
        // Nothing yet today: yesterday keeps the streak alive.
        let tomorrow = date_at(dec30 + 4 * 86_400, 0);
        assert_eq!(current_streak_by(&records, tomorrow, &utc), 4);
        // A missed day breaks it.
        let later = date_at(dec30 + 6 * 86_400, 0);
        assert_eq!(current_streak_by(&records, later, &utc), 0);
        // Months: 2026-01-31 -> 2026-02-01 -> 2026-02-02.
        let jan31 = days_from_civil(LocalDate {
            year: 2026,
            month: 1,
            day: 31,
        }) * 86_400;
        let feb = [
            stay("a", 1, 10, jan31, 30),
            stay("a", 2, 10, jan31 + 86_400, 30),
            stay("a", 3, 10, jan31 + 2 * 86_400, 30),
        ];
        assert_eq!(
            current_streak_by(&feb, date_at(jan31 + 2 * 86_400, 0), &utc),
            3
        );
    }

    #[test]
    fn today_and_this_week_start_on_monday() {
        // 2026-11-05 is a Thursday.
        let records = sample();
        assert_eq!(seconds_on(&records, utc(NOV5), &utc), 150);
        // Mon 3rd through Sun 9th: 150 (Wed) and 70 (Thu).
        assert_eq!(seconds_this_week_by(&records, utc(NOV5), &utc), 150);
        assert_eq!(
            seconds_this_week_by(&records, utc(NOV5 + 86_400), &utc),
            220
        );
        // The next Monday starts a new week.
        let next_monday = NOV5 + 5 * 86_400;
        assert_eq!(seconds_this_week_by(&records, utc(next_monday), &utc), 0);
    }

    #[test]
    fn a_month_holds_only_its_own_days() {
        let records = sample();
        // The sample spans 2025-11-05 to 2025-11-08.
        let daily = month_daily_seconds(&records, 2025, 11, &utc);
        assert_eq!(daily.len(), 3);
        assert_eq!(daily[&utc(NOV5)], 60 + 30 + 40 + 20);
        assert_eq!(daily[&utc(NOV5 + 86_400)], 70);
        assert_eq!(daily[&utc(NOV5 + 3 * 86_400)], 10);
        // Another month holds nothing of it.
        let empty = month_daily_seconds(&records, 2025, 12, &utc);
        assert!(empty.is_empty());
        // The day the stays fall on is the local one.
        let shifted =
            month_daily_seconds(&[stay("a", 1, 10, NOV5 + 23 * 3600, 30)], 2025, 11, &|ts| {
                date_at(ts, 8 * 3600)
            });
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[&date_at(NOV5 + 23 * 3600, 8 * 3600)], 30);
    }

    #[test]
    fn a_month_adds_up_to_its_minutes_and_its_days() {
        let daily = month_daily_seconds(&sample(), 2025, 11, &utc);
        let totals = month_totals(&daily);
        // The first day holds 150 seconds, so 3 minutes.
        assert_eq!(totals.minutes, 3 + 2 + 1);
        assert_eq!(totals.days, 3);
        // A day without reading is not counted, and adds nothing.
        let mut daily = daily;
        daily.insert(
            LocalDate {
                year: 2025,
                month: 11,
                day: 20,
            },
            0,
        );
        let totals = month_totals(&daily);
        assert_eq!(totals.minutes, 3 + 2 + 1);
        assert_eq!(totals.days, 3);
    }

    #[test]
    fn a_day_adds_up_per_book_the_most_read_first() {
        let records = sample();
        let day = utc(NOV5);
        let books = day_book_seconds(&records, day, &utc);
        assert_eq!(books.len(), 2);
        assert_eq!(books[0], ("a".to_string(), 60 + 30 + 40));
        assert_eq!(books[1], ("b".to_string(), 20));
        // Another day holds only what was read then.
        let books = day_book_seconds(&records, utc(NOV5 + 86_400), &utc);
        assert_eq!(books, vec![("a".to_string(), 70)]);
        // A day without reading holds nothing.
        assert!(day_book_seconds(&records, utc(NOV5 + 5 * 86_400), &utc).is_empty());
    }

    #[test]
    fn the_first_month_with_a_stay_is_the_earliest_one() {
        assert_eq!(earliest_month(&sample(), &utc), Some((2025, 11)));
        // The stays are not in order in the file.
        let mut records = sample();
        records.reverse();
        assert_eq!(earliest_month(&records, &utc), Some((2025, 11)));
        // No stays, no month.
        assert_eq!(earliest_month(&[], &utc), None);
    }

    #[test]
    fn the_month_grid_follows_the_calendar() {
        // November 2026 has 30 days and starts on a Sunday, so six cells
        // come before the first in a week that starts on Monday.
        assert_eq!(days_in_month(2026, 11), 30);
        assert_eq!(month_lead(2026, 11), 6);
        // A month that starts on a Monday has no empty cell before it.
        assert_eq!(month_lead(2026, 6), 0);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    #[test]
    fn stepping_a_month_turns_the_year() {
        assert_eq!(month_step(2026, 11, 1), (2026, 12));
        assert_eq!(month_step(2026, 12, 1), (2027, 1));
        assert_eq!(month_step(2026, 1, -1), (2025, 12));
        assert_eq!(month_step(2026, 11, 0), (2026, 11));
    }

    #[test]
    fn truncated_last_line_is_skipped() {
        let dir = crate::testdir("stats-truncated");
        let path = dir.join("stats-2026-11.jsonl");
        let good = serde_json::to_string(&stay("a", 1, 10, NOV5, 30)).unwrap();
        let mut text = good.clone();
        text.push('\n');
        text.push_str(r#"{"book_hash":"a","page":2,"start_time":"#);
        fs::write(&path, text).unwrap();
        let records = Store::new(&dir).records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], stay("a", 1, 10, NOV5, 30));
    }

    #[test]
    fn writer_appends_month_files_and_flushes_on_request() {
        let dir = crate::testdir("stats-writer");
        let stats = Stats::new(&dir);
        let nov = stay("a", 1, 10, NOV5, 30);
        let dec = stay("a", 2, 10, NOV5 + 40 * 86_400, 30);
        stats.record_at(nov.clone());
        stats.record_at(nov.clone());
        stats.flush();
        stats.record_at(dec.clone());
        stats.flush();
        let files: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            files.contains(&"stats-2025-11.jsonl".to_string()),
            "{files:?}"
        );
        assert!(
            files.contains(&"stats-2025-12.jsonl".to_string()),
            "{files:?}"
        );
        let mut records = Store::new(&dir).records();
        records.sort_by_key(|s| s.start_time);
        assert_eq!(records, vec![nov, dec]);
    }

    #[test]
    fn snapshot_and_flush_keep_the_measured_stay() {
        let dir = crate::testdir("stats-snapshot");
        let mut stats = Stats::new(&dir);
        let t0 = Instant::now();
        stats.page_shown("a", 3, 10, t0, NOV5);
        stats.record_page("a", 4, 10, t0 + Duration::from_secs(30), NOV5 + 30);
        stats.finish_page(t0 + Duration::from_secs(50));
        stats.flush();
        let records = Store::new(&dir).records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].page, 3);
        assert_eq!(records[0].duration, 30);
        assert_eq!(records[0].start_time, NOV5);
        assert_eq!(records[1].page, 4);
        assert_eq!(records[1].duration, 20);
        assert_eq!(records[1].start_time, NOV5 + 30);
    }

    #[test]
    fn book_start_date_finds_earliest_day() {
        let of = |ts| date_at(ts, 0);
        let records = vec![
            stay("book-1", 10, 100, NOV5 + 2 * 86_400, 30),
            stay("book-1", 5, 100, NOV5, 30),
            stay("book-2", 1, 50, NOV5 - 86_400, 30),
            stay("book-1", 8, 100, NOV5 + 86_400, 30),
        ];
        assert_eq!(
            book_start_date(&records, "book-1", &of),
            Some(LocalDate {
                year: 2025,
                month: 11,
                day: 5
            })
        );
        assert_eq!(book_start_date(&records, "unknown", &of), None);
    }

    #[test]
    fn book_avg_seconds_per_page_calculates_minutes_and_seconds() {
        // 38 hours 20 mins = 138,000 secs. 450 pages -> 306.67s = 5 min 7 sec.
        let totals = BookTotals {
            duration: 38 * 3600 + 20 * 60,
            days: 96,
            pages: 450,
        };
        assert_eq!(book_avg_seconds_per_page(&totals), Some((5, 7)));

        let empty = BookTotals::default();
        assert_eq!(book_avg_seconds_per_page(&empty), None);

        let fast = BookTotals {
            duration: 45,
            days: 1,
            pages: 1,
        };
        assert_eq!(book_avg_seconds_per_page(&fast), Some((0, 45)));
    }

    #[test]
    fn estimate_minutes_left_uses_the_readers_own_pace() {
        // Ten distinct pages, one minute each: a minute per page. Half of a
        // hundred-page book is fifty pages, so fifty minutes.
        let records: Vec<PageStay> = (1..=10)
            .map(|page| stay("book-1", page, 100, NOV5, 60))
            .collect();
        assert_eq!(estimate_minutes_left(&records, "book-1", 0.5), Some(50));
        // A finished book says nothing, nor does one without stays, nor a
        // book whose remaining part is under a minute.
        assert_eq!(estimate_minutes_left(&records, "book-1", 1.0), None);
        assert_eq!(estimate_minutes_left(&records, "book-2", 0.5), None);
        assert_eq!(estimate_minutes_left(&[], "book-1", 0.5), None);
        let almost_done = estimate_minutes_left(&records, "book-1", 0.999);
        assert_eq!(almost_done, None);
    }

    #[test]
    fn book_recent_daily_seconds_returns_last_seven_days() {
        let of = |ts| date_at(ts, 0);
        let today = LocalDate {
            year: 2025,
            month: 11,
            day: 5,
        };
        let records = vec![
            stay("book-1", 1, 100, NOV5, 120),
            stay("book-1", 2, 100, NOV5 - 86_400, 60),
            stay("book-2", 1, 100, NOV5 - 86_400, 300),
            stay("book-1", 3, 100, NOV5 - 6 * 86_400, 90),
            stay("book-1", 4, 100, NOV5 - 7 * 86_400, 500), // outside 7 days
        ];
        let daily = book_recent_daily_seconds(&records, "book-1", today, &of);
        assert_eq!(daily.len(), 7);
        assert_eq!(daily[6].0, today);
        assert_eq!(daily[6].1, 120);
        assert_eq!(
            daily[5].0,
            LocalDate {
                year: 2025,
                month: 11,
                day: 4
            }
        );
        assert_eq!(daily[5].1, 60);
        assert_eq!(
            daily[0].0,
            LocalDate {
                year: 2025,
                month: 10,
                day: 30
            }
        );
        assert_eq!(daily[0].1, 90);
    }

    #[test]
    fn book_estimate_completion_projects_from_recent_pace() {
        let of = |ts| date_at(ts, 0);
        let today = LocalDate {
            year: 2026,
            month: 11,
            day: 4,
        };
        let today_days = days_from_civil(today);
        let today_ts = today_days * 86_400;
        let mut records = Vec::new();
        for p in 1..=190 {
            let day_offset = (p % 14) as i64;
            records.push(stay(
                "hongloumeng",
                p,
                1046,
                today_ts - day_offset * 86_400,
                30,
            ));
        }
        let est = book_estimate_completion(&records, "hongloumeng", today, 596, &of);
        assert_eq!(
            est,
            Some((
                LocalDate {
                    year: 2026,
                    month: 12,
                    day: 18
                },
                44
            ))
        );

        assert_eq!(
            book_estimate_completion(&records, "hongloumeng", today, 0, &of),
            None
        );
        assert_eq!(
            book_estimate_completion(&records, "other", today, 100, &of),
            None
        );
    }

    impl Store {
        /// In-memory records for the pure aggregation tests.
        fn records_from(records: Vec<PageStay>) -> Vec<PageStay> {
            let mut seen = HashSet::new();
            records
                .into_iter()
                .filter(|s| seen.insert((s.book_hash.clone(), s.page, s.start_time)))
                .collect()
        }
    }
}
