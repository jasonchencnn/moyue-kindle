// SPDX-License-Identifier: AGPL-3.0-or-later
//! Transition frames outside the app's interactive lifetime.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use aristotle_hal::framebuffer::{Framebuffer, Waveform};
use aristotle_render::plato_core::anyhow::{format_err, Error};
use aristotle_ui::geom::Screen;
use aristotle_ui::screens::transition::{self, frame_state, FrameState as Frame, Stage};
use aristotle_ui::text::{FamilyId, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::Refresh;
use aristotle_ui::ui::Ui;

use crate::panel::{Content, Panel};
use crate::Args;

/// Shows a transition using the current UI fonts and panel refresh policy.
pub fn show(panel: &mut Panel, ui: &mut Ui, stage: Stage, refresh: Refresh) -> Result<(), Error> {
    let styles = Styles::new(FamilyId(0)).with_density(ui.text.density());
    ui.set_tree_with(transition::build(&styles, stage), refresh);
    let damage = ui.render();
    panel.present(ui, &damage, Content::Chrome)?;
    Ok(())
}

fn property(service: &str, name: &str) -> Option<String> {
    let output = Command::new("timeout")
        .args(["-t", "1", "-s", "KILL", "lipc-get-prop", service, name])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn framework_running() -> bool {
    Command::new("timeout")
        .args(["-t", "1", "-s", "KILL", "initctl", "status", "framework"])
        .output()
        .ok()
        .is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("start/running")
        })
}

fn stock_ready() -> bool {
    let Some(pid) = property("com.lab126.appmgrd", "activeAppPid")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&pid| pid > 0)
    else {
        return false;
    };
    Path::new(&format!("/proc/{pid}")).exists()
        && property("com.lab126.appmgrd", "activeApp").is_some_and(|app| !app.is_empty())
}

fn paint(fb: &mut Framebuffer, ui: &mut Ui, full: bool) -> Result<(), Error> {
    let damage = ui.render();
    let Some(area) = damage
        .iter()
        .map(|change| change.rect)
        .reduce(|area, next| area.union(&next))
    else {
        return Ok(());
    };
    let canvas = ui.screen();
    let rect = if full {
        fb.full_screen()
    } else {
        aristotle_hal::framebuffer::Rect {
            top: area.y as u32,
            left: area.x as u32,
            width: area.w as u32,
            height: area.h as u32,
        }
    };
    fb.blit(&canvas.pixels, canvas.stride as usize, rect);
    let marker = fb.update(
        rect,
        if full {
            Waveform::GCK16
        } else {
            Waveform::GLKW16
        },
        true,
    )?;
    fb.wait(marker)?;
    Ok(())
}

fn owns_observer() -> bool {
    std::fs::read_to_string("/var/run/aristotle-transition.pid")
        .ok()
        .and_then(|value| value.lines().next()?.parse::<u32>().ok())
        == Some(std::process::id())
}

/// Draw once, or observe the stock handoff for at most two minutes.
pub fn run(args: &Args, value: &str) -> Result<(), Error> {
    let watch = value == "watch";
    let mut stage = if watch {
        Stage::Restoring
    } else {
        Stage::parse(value).ok_or_else(|| format_err!("unknown transition stage {value}"))?
    };
    let prefs = std::fs::read(args.library.join(crate::prefs::Prefs::FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let language = args
        .lang
        .as_deref()
        .or_else(|| prefs.as_ref()?.get("language")?.as_str())
        .unwrap_or("zh");
    aristotle_i18n::set_language(language);
    let mut fb = Framebuffer::open("/dev/fb0")?;
    let geometry = fb.geometry();
    let dpi = aristotle_hal::platform::detect_kobo()
        .ok()
        .flatten()
        .map(|id| id.model.dpi())
        .or_else(|| {
            aristotle_hal::platform::Platform::model()
                .ok()
                .map(|m| m.dpi())
        })
        .unwrap_or(300);
    let profile =
        Screen::new(geometry.width, geometry.height, dpi).map_err(|e| format_err!("{e}"))?;
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let fonts = crate::ui_fonts(&args.data.join("serif"), &args.data.join("fonts"))?;
    let mut ui = Ui::with_screen(
        transition::build(&styles, stage),
        TextEngine::new(fonts),
        profile,
    );
    if !watch {
        return paint(&mut fb, &mut ui, true);
    }
    // Compute the expected pixels without repainting a stock frame that arrived meanwhile.
    ui.render();
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(120)
        && !crate::STOP.load(std::sync::atomic::Ordering::Relaxed)
    {
        if !owns_observer() || Path::new("/var/run/aristotle-takeover").exists() || stock_ready() {
            break;
        }
        let state = frame_state(
            fb.rows(0, geometry.height),
            geometry.stride as usize,
            ui.screen(),
        );
        let next = if began.elapsed() >= Duration::from_secs(90) {
            Stage::Slow
        } else if framework_running() {
            Stage::Waiting
        } else {
            Stage::Restoring
        };
        if state != Frame::Other && (state == Frame::Blank || stage != next) {
            if !owns_observer()
                || Path::new("/var/run/aristotle-takeover").exists()
                || crate::STOP.load(std::sync::atomic::Ordering::Relaxed)
                || stock_ready()
            {
                break;
            }
            if frame_state(
                fb.rows(0, geometry.height),
                geometry.stride as usize,
                ui.screen(),
            ) == Frame::Other
            {
                continue;
            }
            stage = next;
            ui.set_tree_with(
                transition::build(&styles, stage),
                if state == Frame::Blank {
                    Refresh::Full
                } else {
                    Refresh::Auto
                },
            );
            paint(&mut fb, &mut ui, state == Frame::Blank)?;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if began.elapsed() >= Duration::from_secs(120) {
        eprintln!("aristotle: stock transition observer reached its 120 s limit");
    }
    let owner = Path::new("/var/run/aristotle-transition.pid");
    if owns_observer() {
        let _ = std::fs::remove_file(owner);
    }
    Ok(())
}
