// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aristotle-observe bench anim`: page-turn animation feasibility spike (ANIM-001 / T0).
//!
//! # Objective
//!
//! Measures whether multi-frame wipe animations ("擦除+揭示", right-to-left)
//! are feasible on Kindle e-ink hardware (Oasis 2 / Zelda, PW4 / Rex) when
//! submitted through the display thread (`aristotle_hal::display::Display`).
//!
//! On the PW4/Oasis 2 EPDC, earlier benchmarks showed the driver essentially
//! serializes updates: submits block behind earlier in-progress updates, even
//! for non-overlapping regions. The T0 spike measures:
//! 1. Baseline: no animation (0 frames, 1 full-mode turn update).
//! 2. 4-frame, 6-frame, and 8-frame wipe animations (right-to-left).
//! 3. 20 iterations per group on the exact same two pages.
//! 4. Latency from trigger to last frame on screen (Average and P95).
//! 5. Per-frame queueing, queue delays, and coalescing behavior.
//! 6. Summary and frame-level CSV written to `/var/tmp/`.
//!
//! Pass threshold (Leader / Human signoff):
//! The optimal animated group duration must not exceed 1.5x baseline,
//! and residual ghosting must be judged acceptable by eye.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use aristotle_hal::display::Display;
use aristotle_hal::framebuffer::{Framebuffer, Rect, Waveform};
use aristotle_hal::input::monotonic_us;

/// Configuration for the animation benchmark.
#[derive(Clone, Debug)]
pub struct Config {
    /// Number of turns per group (default: 20).
    pub turns: usize,
    /// Pause in milliseconds between turns for human visual inspection (default: 200).
    pub pause_ms: u64,
    /// Delay in milliseconds between submitting consecutive frames of a wipe (default: 25).
    pub frame_delay_ms: u64,
    /// If true, wait for display thread to submit before queueing next frame.
    pub sync_mode: bool,
    /// Optional path to raw 8-bit page dump.
    pub page_path: Option<String>,
    /// Turn summary CSV output path (default: `/var/tmp/anim_benchmark.csv`).
    pub csv_path: String,
    /// Per-frame queueing details CSV output path (default: `/var/tmp/anim_frames.csv`).
    pub frames_csv_path: String,
    /// Frame counts to test (0 = baseline, then e.g. 4, 6, 8).
    pub groups: Vec<usize>,
    /// Waveform for animation wipe slices (default: A2 = mode 6).
    pub anim_waveform: Waveform,
    /// Waveform for baseline page turns (default: GLKW16 = mode 9).
    pub baseline_waveform: Waveform,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            turns: 20,
            pause_ms: 200,
            frame_delay_ms: 25,
            sync_mode: false,
            page_path: None,
            csv_path: "/var/tmp/anim_benchmark.csv".into(),
            frames_csv_path: "/var/tmp/anim_frames.csv".into(),
            groups: vec![0, 4, 6, 8],
            anim_waveform: Waveform::A2,
            baseline_waveform: Waveform::GLKW16,
        }
    }
}

/// Record of one submitted animation frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameRecord {
    pub group: String,
    pub turn: usize,
    pub frame_index: usize,
    pub total_frames: usize,
    pub rect_left: u32,
    pub rect_width: u32,
    pub waveform_mode: u32,
    pub submit_us: u64,
    pub was_busy: bool,
    pub shown_us: Option<u64>,
    pub coalesced_count: usize,
}

/// Record of one page turn.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnRecord {
    pub group: String,
    pub turn: usize,
    pub frames_count: usize,
    pub trigger_us: u64,
    pub last_shown_us: u64,
    pub duration_ms: f64,
    pub actual_updates: usize,
    pub coalesced_frames: usize,
}

/// Aggregate summary for one test group.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupSummary {
    pub group: String,
    pub frames_count: usize,
    pub avg_ms: f64,
    pub p95_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub avg_actual_updates: f64,
    pub total_coalesced_frames: usize,
    pub ratio_vs_baseline: f64,
}

/// Compute the slice rectangle for frame `k` of `total_frames` (1-indexed),
/// wiping from right to left across screen width `w` and height `h`.
///
/// Frame 1 starts at the rightmost edge (`w`) moving leftwards.
/// Frame `total_frames` ends at `left = 0`.
pub fn wipe_slice_rect(k: usize, total_frames: usize, w: u32, h: u32) -> Rect {
    assert!(total_frames > 0, "total_frames must be > 0");
    assert!(
        k >= 1 && k <= total_frames,
        "frame index k must be in 1..=total_frames"
    );
    let k = k as u64;
    let n = total_frames as u64;
    let w = w as u64;

    let x_end = w - (k - 1) * w / n;
    let x_start = if k == n { 0 } else { w - k * w / n };
    let slice_width = x_end - x_start;

    Rect {
        top: 0,
        left: x_start as u32,
        width: slice_width as u32,
        height: h,
    }
}

/// Calculate the arithmetic mean of a slice of samples.
pub fn average(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        0.0
    } else {
        samples.iter().sum::<f64>() / samples.len() as f64
    }
}

/// Calculate the 95th percentile using nearest-rank index.
pub fn percentile_p95(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

/// Format the turn records as CSV.
pub fn format_turns_csv(turns: &[TurnRecord]) -> String {
    let mut out = String::new();
    out.push_str(
        "group,turn,frames,trigger_us,last_shown_us,duration_ms,actual_updates,coalesced_frames\n",
    );
    for t in turns {
        out.push_str(&format!(
            "{},{},{},{},{},{:.2},{},{}\n",
            t.group,
            t.turn,
            t.frames_count,
            t.trigger_us,
            t.last_shown_us,
            t.duration_ms,
            t.actual_updates,
            t.coalesced_frames
        ));
    }
    out
}

/// Format the per-frame records as CSV.
pub fn format_frames_csv(frames: &[FrameRecord], turns: &[TurnRecord]) -> String {
    let mut out = String::new();
    out.push_str("group,turn,frame,total_frames,rect_left,rect_width,waveform_mode,submit_us,was_busy,shown_us,duration_from_trigger_ms,queue_latency_ms,coalesced_count\n");

    // Map (group, turn) -> trigger_us for fast lookup
    let triggers: std::collections::HashMap<(&str, usize), u64> = turns
        .iter()
        .map(|t| ((t.group.as_str(), t.turn), t.trigger_us))
        .collect();

    for f in frames {
        let trigger_us = triggers
            .get(&(f.group.as_str(), f.turn))
            .copied()
            .unwrap_or(f.submit_us);
        let duration_ms = f
            .shown_us
            .map(|s| (s.saturating_sub(trigger_us)) as f64 / 1000.0)
            .unwrap_or(0.0);
        let queue_latency_ms = f
            .shown_us
            .map(|s| (s.saturating_sub(f.submit_us)) as f64 / 1000.0)
            .unwrap_or(0.0);

        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{:.2},{:.2},{}\n",
            f.group,
            f.turn,
            f.frame_index,
            f.total_frames,
            f.rect_left,
            f.rect_width,
            f.waveform_mode,
            f.submit_us,
            if f.was_busy { 1 } else { 0 },
            f.shown_us.unwrap_or(0),
            duration_ms,
            queue_latency_ms,
            f.coalesced_count
        ));
    }
    out
}

/// Helper to draw a horizontal block on a framebuffer page.
fn draw_bar(
    buf: &mut [u8],
    stride: usize,
    y: usize,
    height: usize,
    x: usize,
    width: usize,
    gray: u8,
) {
    for row in y..y + height {
        let row_start = row * stride + x;
        let row_end = row_start + width;
        if row_end <= buf.len() {
            buf[row_start..row_end].fill(gray);
        }
    }
}

/// Generate two distinct synthetic book pages (Page A and Page B) with
/// realistic layout: header rule, distinct paragraph lines, margins, and footer.
pub fn generate_synthetic_pages(w: u32, h: u32, stride: u32) -> (Vec<u8>, Vec<u8>) {
    let len = stride as usize * h as usize;
    let mut page_a = vec![0xff; len];
    let mut page_b = vec![0xff; len];

    let w = w as usize;
    let h = h as usize;
    let stride = stride as usize;

    let margin_x = (w * 5) / 100;
    let content_w = w.saturating_sub(margin_x * 2);
    let line_h = 6;
    let step_y = 44;

    // Header line at y = 60
    draw_bar(&mut page_a, stride, 60, 2, margin_x, content_w, 0x80);
    draw_bar(&mut page_b, stride, 60, 2, margin_x, content_w, 0x80);

    // Footer line at y = h - 80
    draw_bar(&mut page_a, stride, h - 80, 2, margin_x, content_w, 0x80);
    draw_bar(&mut page_b, stride, h - 80, 2, margin_x, content_w, 0x80);

    // Page A paragraphs (Simulated chapter text)
    // Paragraph 1: 5 lines
    let mut curr_y = 120;
    for i in 0..5 {
        let indent = if i == 0 { 40 } else { 0 };
        let lw = if i == 4 {
            content_w * 6 / 10
        } else {
            content_w - indent
        };
        draw_bar(
            &mut page_a,
            stride,
            curr_y,
            line_h,
            margin_x + indent,
            lw,
            0x10,
        );
        curr_y += step_y;
    }
    // Paragraph 2: 7 lines
    curr_y += 20;
    for i in 0..7 {
        let indent = if i == 0 { 40 } else { 0 };
        let lw = if i == 6 {
            content_w * 7 / 10
        } else {
            content_w - indent
        };
        draw_bar(
            &mut page_a,
            stride,
            curr_y,
            line_h,
            margin_x + indent,
            lw,
            0x10,
        );
        curr_y += step_y;
    }
    // Paragraph 3: 8 lines
    curr_y += 20;
    for i in 0..8 {
        let indent = if i == 0 { 40 } else { 0 };
        let lw = if i == 7 {
            content_w * 5 / 10
        } else {
            content_w - indent
        };
        draw_bar(
            &mut page_a,
            stride,
            curr_y,
            line_h,
            margin_x + indent,
            lw,
            0x10,
        );
        curr_y += step_y;
    }
    // Page A footer tick (Page 1)
    draw_bar(&mut page_a, stride, h - 60, 8, margin_x + 20, 30, 0x20);

    // Page B paragraphs (Different paragraph shapes and line counts)
    curr_y = 120;
    // Paragraph 1: 8 lines
    for i in 0..8 {
        let indent = if i == 0 { 40 } else { 0 };
        let lw = if i == 7 {
            content_w * 4 / 10
        } else {
            content_w - indent
        };
        draw_bar(
            &mut page_b,
            stride,
            curr_y,
            line_h,
            margin_x + indent,
            lw,
            0x10,
        );
        curr_y += step_y;
    }
    // Paragraph 2: 5 lines
    curr_y += 20;
    for i in 0..5 {
        let indent = if i == 0 { 40 } else { 0 };
        let lw = if i == 4 {
            content_w * 8 / 10
        } else {
            content_w - indent
        };
        draw_bar(
            &mut page_b,
            stride,
            curr_y,
            line_h,
            margin_x + indent,
            lw,
            0x10,
        );
        curr_y += step_y;
    }
    // Paragraph 3: 7 lines
    curr_y += 20;
    for i in 0..7 {
        let indent = if i == 0 { 40 } else { 0 };
        let lw = if i == 6 {
            content_w * 3 / 10
        } else {
            content_w - indent
        };
        draw_bar(
            &mut page_b,
            stride,
            curr_y,
            line_h,
            margin_x + indent,
            lw,
            0x10,
        );
        curr_y += step_y;
    }
    // Page B footer tick (Page 2)
    draw_bar(&mut page_b, stride, h - 60, 8, margin_x + 20, 70, 0x20);

    (page_a, page_b)
}

/// Load raw page file from disk or synthesize pages if file is absent.
fn load_or_synthesize_pages(
    cfg_path: Option<&str>,
    w: u32,
    h: u32,
    stride: u32,
) -> (Vec<u8>, Vec<u8>) {
    let candidate = cfg_path.map(Path::new).or_else(|| {
        let p = Path::new("/var/tmp/page.raw");
        if p.exists() {
            Some(p)
        } else {
            None
        }
    });

    if let Some(path) = candidate {
        if let Ok(mut page_a) = fs::read(path) {
            let expected_len = stride as usize * h as usize;
            page_a.resize(expected_len, 0xff);
            // Shift page_a by 37 rows to generate page_b with real glyph displacement
            let mut page_b = vec![0xff; expected_len];
            let shift_rows = 37;
            let shift_bytes = shift_rows * stride as usize;
            if shift_bytes < expected_len {
                page_b[shift_bytes..].copy_from_slice(&page_a[..(expected_len - shift_bytes)]);
            }
            println!(
                "Loaded page fixture from {} (shifted 37 rows for Page B)",
                path.display()
            );
            return (page_a, page_b);
        }
    }

    println!(
        "Synthesizing 2 realistic book pages ({}x{}, stride {})",
        w, h, stride
    );
    generate_synthetic_pages(w, h, stride)
}

/// Parse command line arguments into Config.
pub fn parse_args(args: &[String]) -> Config {
    let mut cfg = Config::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--turns" => {
                if let Some(val) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                    cfg.turns = val;
                    i += 1;
                }
            }
            "--pause" => {
                if let Some(val) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                    cfg.pause_ms = val;
                    i += 1;
                }
            }
            "--delay" => {
                if let Some(val) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                    cfg.frame_delay_ms = val;
                    i += 1;
                }
            }
            "--sync" => {
                cfg.sync_mode = true;
            }
            "--page" => {
                if let Some(val) = args.get(i + 1) {
                    cfg.page_path = Some(val.clone());
                    i += 1;
                }
            }
            "--csv" => {
                if let Some(val) = args.get(i + 1) {
                    cfg.csv_path = val.clone();
                    i += 1;
                }
            }
            "--frames-csv" => {
                if let Some(val) = args.get(i + 1) {
                    cfg.frames_csv_path = val.clone();
                    i += 1;
                }
            }
            "--waveform" => {
                if let Some(val) = args.get(i + 1) {
                    cfg.anim_waveform = match val.to_lowercase().as_str() {
                        "gl16" => Waveform::GL16,
                        "du" => Waveform::DU,
                        _ => Waveform::A2,
                    };
                    i += 1;
                }
            }
            "--groups" => {
                if let Some(val) = args.get(i + 1) {
                    let parsed: Vec<usize> = val
                        .split(',')
                        .filter_map(|s| match s.trim().to_lowercase().as_str() {
                            "baseline" | "base" | "0" => Some(0),
                            other => other.parse().ok(),
                        })
                        .collect();
                    if !parsed.is_empty() {
                        cfg.groups = parsed;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    cfg
}

/// Wait until display is completely idle, collecting any finished updates.
fn wait_display_idle(
    display: &Display,
    frames: &mut [FrameRecord],
    timeout: Duration,
) -> Vec<(u64, Vec<u64>)> {
    let start = Instant::now();
    let mut completions = Vec::new();

    while display.busy() {
        for (completed_at_us, tags) in display.drain_shown() {
            for &tag in &tags {
                let idx = tag as usize;
                if idx < frames.len() {
                    frames[idx].shown_us = Some(completed_at_us);
                    frames[idx].coalesced_count = tags.len();
                }
            }
            completions.push((completed_at_us, tags));
        }
        thread::sleep(Duration::from_millis(2));
        if start.elapsed() >= timeout {
            eprintln!("warning: timed out waiting for display completions");
            break;
        }
    }

    // Drain any remaining items after busy became false
    for (completed_at_us, tags) in display.drain_shown() {
        for &tag in &tags {
            let idx = tag as usize;
            if idx < frames.len() {
                frames[idx].shown_us = Some(completed_at_us);
                frames[idx].coalesced_count = tags.len();
            }
        }
        completions.push((completed_at_us, tags));
    }

    completions
}

/// Main entry point for `aristotle-observe bench anim`.
pub fn run(args: &[String]) -> io::Result<()> {
    let cfg = parse_args(args);

    let mut fb = Framebuffer::open("/dev/fb0")?;
    let g = fb.geometry();
    let w = g.width;
    let h = g.height;
    let stride = g.stride;
    let screen_rect = fb.full_screen();

    println!("================================================================================");
    println!("ANIM-001 Page Turn Animation Feasibility Spike (T0)");
    println!(
        "Platform: {:?}, Screen: {}x{}, Stride: {}",
        fb.platform(),
        w,
        h,
        stride
    );
    println!(
        "Config: turns={}, pause={}ms, frame_delay={}ms, sync={}, anim_wf=mode {}",
        cfg.turns, cfg.pause_ms, cfg.frame_delay_ms, cfg.sync_mode, cfg.anim_waveform.0
    );
    println!("CSV targets: {} and {}", cfg.csv_path, cfg.frames_csv_path);
    println!("================================================================================");

    let (page_a, page_b) = load_or_synthesize_pages(cfg.page_path.as_deref(), w, h, stride);

    let display = Display::new(&fb)?;
    let mut all_frames: Vec<FrameRecord> = Vec::new();
    let mut all_turns: Vec<TurnRecord> = Vec::new();
    let mut summaries: Vec<GroupSummary> = Vec::new();

    // Baseline average duration for ratio calculation
    let mut baseline_avg_ms: Option<f64> = None;

    for &frames_count in &cfg.groups {
        let group_name = if frames_count == 0 {
            "baseline".to_string()
        } else {
            format!("wipe_{}", frames_count)
        };

        println!(
            "\n== Running group: {} ({} turns) ...",
            group_name, cfg.turns
        );

        // Flash clean screen before starting this group
        fb.blit(&page_a, stride as usize, screen_rect);
        let m = fb.update(screen_rect, Waveform::GC16, true)?;
        fb.wait(m)?;
        thread::sleep(Duration::from_millis(300));

        let mut current_is_a = true;
        let mut group_durations: Vec<f64> = Vec::new();
        let mut group_actual_updates = 0usize;
        let mut group_coalesced_frames = 0usize;

        for turn in 1..=cfg.turns {
            let target_page = if current_is_a { &page_b } else { &page_a };
            let turn_frames_start_idx = all_frames.len();

            let trigger_us = monotonic_us();

            if frames_count == 0 {
                // Baseline: 1 full-mode turn update
                fb.blit(target_page, stride as usize, screen_rect);
                let was_busy = display.busy();
                let submit_us = monotonic_us();
                let frame_idx = all_frames.len();
                all_frames.push(FrameRecord {
                    group: group_name.clone(),
                    turn,
                    frame_index: 1,
                    total_frames: 1,
                    rect_left: screen_rect.left,
                    rect_width: screen_rect.width,
                    waveform_mode: cfg.baseline_waveform.0,
                    submit_us,
                    was_busy,
                    shown_us: None,
                    coalesced_count: 1,
                });

                display.damage_full(screen_rect, cfg.baseline_waveform, &[frame_idx as u64]);
            } else {
                // Animated wipe from right to left in `frames_count` slices
                for k in 1..=frames_count {
                    let rect = wipe_slice_rect(k, frames_count, w, h);

                    // Copy the vertical slice from target page into framebuffer
                    fb.blit(&target_page[rect.left as usize..], stride as usize, rect);

                    let was_busy = display.busy();
                    let submit_us = monotonic_us();
                    let frame_idx = all_frames.len();
                    all_frames.push(FrameRecord {
                        group: group_name.clone(),
                        turn,
                        frame_index: k,
                        total_frames: frames_count,
                        rect_left: rect.left,
                        rect_width: rect.width,
                        waveform_mode: cfg.anim_waveform.0,
                        submit_us,
                        was_busy,
                        shown_us: None,
                        coalesced_count: 1,
                    });

                    display.damage(rect, cfg.anim_waveform, &[frame_idx as u64]);

                    if cfg.frame_delay_ms > 0 && k < frames_count {
                        thread::sleep(Duration::from_millis(cfg.frame_delay_ms));
                    }
                    if cfg.sync_mode {
                        display.sync();
                    }
                }
            }

            // Wait until this turn settles on panel
            let completions = wait_display_idle(&display, &mut all_frames, Duration::from_secs(5));

            let turn_frames = &all_frames[turn_frames_start_idx..];
            let last_shown_us = turn_frames
                .iter()
                .filter_map(|f| f.shown_us)
                .max()
                .unwrap_or_else(monotonic_us);

            let turn_duration_ms = (last_shown_us.saturating_sub(trigger_us)) as f64 / 1000.0;
            let actual_updates = completions.len();
            let coalesced_in_turn = turn_frames.iter().filter(|f| f.coalesced_count > 1).count();

            group_durations.push(turn_duration_ms);
            group_actual_updates += actual_updates;
            group_coalesced_frames += coalesced_in_turn;

            all_turns.push(TurnRecord {
                group: group_name.clone(),
                turn,
                frames_count,
                trigger_us,
                last_shown_us,
                duration_ms: turn_duration_ms,
                actual_updates,
                coalesced_frames: coalesced_in_turn,
            });

            print!(".");
            let _ = io::stdout().flush();

            current_is_a = !current_is_a;
            if cfg.pause_ms > 0 {
                thread::sleep(Duration::from_millis(cfg.pause_ms));
            }
        }
        println!();

        let avg = average(&group_durations);
        let p95 = percentile_p95(&group_durations);
        let min = group_durations
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        let max = group_durations
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let avg_upd = group_actual_updates as f64 / cfg.turns as f64;

        if frames_count == 0 {
            baseline_avg_ms = Some(avg);
        }
        let ratio = match baseline_avg_ms {
            Some(base) if base > 0.0 => avg / base,
            _ => 1.0,
        };

        summaries.push(GroupSummary {
            group: group_name,
            frames_count,
            avg_ms: avg,
            p95_ms: p95,
            min_ms: min,
            max_ms: max,
            avg_actual_updates: avg_upd,
            total_coalesced_frames: group_coalesced_frames,
            ratio_vs_baseline: ratio,
        });
    }

    // Restore clean screen
    fb.blit(&page_a, stride as usize, screen_rect);
    let m = fb.update(screen_rect, Waveform::GC16, true)?;
    fb.wait(m)?;

    // Write CSV files to /var/tmp/
    if let Ok(mut f) = File::create(&cfg.csv_path) {
        let csv_data = format_turns_csv(&all_turns);
        let _ = f.write_all(csv_data.as_bytes());
        println!("Turn summary written to {}", cfg.csv_path);
    } else {
        eprintln!("failed to write {}", cfg.csv_path);
    }

    if let Ok(mut f) = File::create(&cfg.frames_csv_path) {
        let csv_data = format_frames_csv(&all_frames, &all_turns);
        let _ = f.write_all(csv_data.as_bytes());
        println!("Per-frame details written to {}", cfg.frames_csv_path);
    } else {
        eprintln!("failed to write {}", cfg.frames_csv_path);
    }

    // Print final summary table
    println!("\n================================================================================");
    println!("ANIM-001 Benchmark Results ({} turns per group)", cfg.turns);
    println!("================================================================================");
    println!(
        "{:<12} {:>6} {:>10} {:>10} {:>9} {:>9} {:>10} {:>10}",
        "Group", "Frames", "Avg (ms)", "P95 (ms)", "Min (ms)", "Max (ms)", "Avg Upd", "Ratio"
    );
    println!("--------------------------------------------------------------------------------");

    for s in &summaries {
        println!(
            "{:<12} {:>6} {:>10.1} {:>10.1} {:>9.1} {:>9.1} {:>10.1} {:>9.2}x",
            s.group,
            s.frames_count,
            s.avg_ms,
            s.p95_ms,
            s.min_ms,
            s.max_ms,
            s.avg_actual_updates,
            s.ratio_vs_baseline
        );
    }
    println!("--------------------------------------------------------------------------------");

    // Evaluate pass threshold
    let animated: Vec<&GroupSummary> = summaries.iter().filter(|s| s.frames_count > 0).collect();
    if let Some(best) = animated.iter().min_by(|a, b| {
        a.avg_ms
            .partial_cmp(&b.avg_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    }) {
        let passed = best.ratio_vs_baseline <= 1.5;
        println!(
            "Optimal animated group: {} (avg {:.1} ms, {:.2}x baseline)",
            best.group, best.avg_ms, best.ratio_vs_baseline
        );
        println!(
            "T0 Spike criteria (duration <= 1.5x baseline): {}",
            if passed {
                "PASS (within 1.5x)"
            } else {
                "EXCEEDS 1.5x"
            }
        );
    }
    println!("================================================================================");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wipe_slice_rect_4_frames() {
        let w = 1264;
        let h = 1680;

        let r1 = wipe_slice_rect(1, 4, w, h);
        let r2 = wipe_slice_rect(2, 4, w, h);
        let r3 = wipe_slice_rect(3, 4, w, h);
        let r4 = wipe_slice_rect(4, 4, w, h);

        assert_eq!(r1.top, 0);
        assert_eq!(r1.height, h);
        assert_eq!(r1.left, 948);
        assert_eq!(r1.width, 316);

        assert_eq!(r2.left, 632);
        assert_eq!(r2.width, 316);

        assert_eq!(r3.left, 316);
        assert_eq!(r3.width, 316);

        assert_eq!(r4.left, 0);
        assert_eq!(r4.width, 316);

        assert_eq!(r1.width + r2.width + r3.width + r4.width, w);
    }

    #[test]
    fn test_wipe_slice_rect_covers_exact_width_with_odd_numbers() {
        let w = 1072;
        let h = 1448;
        let n = 6;

        let mut covered_width = 0;
        let mut expected_right = w;

        for k in 1..=n {
            let r = wipe_slice_rect(k, n, w, h);
            assert_eq!(r.left + r.width, expected_right);
            covered_width += r.width;
            expected_right = r.left;
        }

        assert_eq!(expected_right, 0);
        assert_eq!(covered_width, w);
    }

    #[test]
    fn test_average_and_p95() {
        let samples = vec![
            100.0, 105.0, 110.0, 115.0, 120.0, 125.0, 130.0, 135.0, 140.0, 200.0,
        ];
        assert!((average(&samples) - 128.0).abs() < 1e-6);

        let p95 = percentile_p95(&samples);
        assert_eq!(p95, 200.0);

        let samples20: Vec<f64> = (1..=20).map(|i| i as f64 * 10.0).collect();
        // 95% of 20 = sample index 18 (value 190.0)
        assert_eq!(percentile_p95(&samples20), 190.0);
    }

    #[test]
    fn test_format_csv() {
        let turns = vec![TurnRecord {
            group: "baseline".into(),
            turn: 1,
            frames_count: 0,
            trigger_us: 1000,
            last_shown_us: 2000,
            duration_ms: 1.0,
            actual_updates: 1,
            coalesced_frames: 0,
        }];

        let frames = vec![FrameRecord {
            group: "baseline".into(),
            turn: 1,
            frame_index: 1,
            total_frames: 1,
            rect_left: 0,
            rect_width: 100,
            waveform_mode: 9,
            submit_us: 1000,
            was_busy: false,
            shown_us: Some(2000),
            coalesced_count: 1,
        }];

        let turns_csv = format_turns_csv(&turns);
        assert!(turns_csv.contains("baseline,1,0,1000,2000,1.00,1,0"));

        let frames_csv = format_frames_csv(&frames, &turns);
        assert!(frames_csv.contains("baseline,1,1,1,0,100,9,1000,0,2000,1.00,1.00,1"));
    }

    #[test]
    fn test_synthetic_pages_dimensions() {
        let (page_a, page_b) = generate_synthetic_pages(1072, 1448, 1088);
        assert_eq!(page_a.len(), 1088 * 1448);
        assert_eq!(page_b.len(), 1088 * 1448);
        // Pages should have some black/gray pixels and differ from each other
        assert_ne!(page_a, page_b);
        assert!(page_a.iter().any(|&p| p < 0xff));
        assert!(page_b.iter().any(|&p| p < 0xff));
    }
}
