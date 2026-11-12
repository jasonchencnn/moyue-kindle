// SPDX-License-Identifier: AGPL-3.0-or-later

//! SYNC-010 shots: the server settings entry on the login screen, the status
//! screen with default and custom endpoints, and the editor prefilled with
//! the official defaults. English only, like `sync004_shots`: the host test
//! environment has no CJK fallback font, so Chinese text would render as
//! placeholders.

use aristotle_i18n::{tr, Msg};
use aristotle_ui::canvas::Canvas;
use aristotle_ui::geom::Screen as UiScreen;
use aristotle_ui::screens::readest_edit::{self as ui_readest_edit, ReadestEditState};
use aristotle_ui::screens::readest_login::{self as ui_readest_login, ReadestLoginState};
use aristotle_ui::screens::readest_status::{self as ui_readest_status, ReadestStatusView};
use aristotle_ui::status::StatusLine;
use aristotle_ui::text::{FamilyId, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{NodeId, Tree};
use aristotle_ui::ui::Ui;
use std::path::{Path, PathBuf};

/// The official endpoints from `SyncConfig::default()`; `aristotle-ui` does
/// not depend on the core crate, so they are written out here.
const DEFAULT_API_BASE: &str = "https://web.readest.com/api";
const DEFAULT_SUPABASE_URL: &str = "https://auth-cf.readest.com";

fn save_png(c: &Canvas, path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut q = c.clone();
    q.quantize16();
    let f = std::fs::File::create(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), q.width, q.height);
    e.set_color(png::ColorType::Grayscale);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .and_then(|mut w| w.write_image_data(&q.packed()))
        .expect("png encoding");
}

fn status() -> StatusLine<'static> {
    StatusLine {
        clock: "14:30",
        battery: Some(0.88),
        charging: false,
    }
}

fn shoot(shots_dir: &Path, name: &str, build: impl Fn(&mut Tree, &Styles) -> NodeId) {
    let screen_profile = UiScreen::new(1072, 1448, 300).unwrap();
    let styles = Styles::new(FamilyId(0)).with_density(screen_profile.density);
    let mut tree = Tree::new();
    let root = build(&mut tree, &styles);
    tree.set_root(root);
    let text = TextEngine::new(Fonts::embedded());
    let mut ui = Ui::with_screen(tree, text, screen_profile);
    ui.layout();
    ui.render();
    save_png(ui.screen(), &shots_dir.join(name));
}

#[test]
fn generate_sync010_task_screenshots() {
    // The committed shots are rewritten only on request (`UPDATE_SHOTS=1`);
    // an ordinary test run writes to the temp directory so it leaves the tree clean.
    let shots_dir = if std::env::var_os("UPDATE_SHOTS").is_some() {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../docs/tasks/SYNC-010/shots")
    } else {
        std::env::temp_dir().join("aristotle-sync010-shots")
    };
    let _ = std::fs::create_dir_all(&shots_dir);

    shoot(&shots_dir, "login-server-settings.png", |t, s| {
        let state = ReadestLoginState::new();
        ui_readest_login::build_readest_login_screen(t, s, &state, &status())
    });

    shoot(&shots_dir, "status-defaults.png", |t, s| {
        let view = ReadestStatusView {
            status: status(),
            email: None,
            last_sync: None,
            pending_count: 0,
            is_syncing: false,
            auto_push: true,
            message: None,
            api_base: DEFAULT_API_BASE,
            supabase_url: DEFAULT_SUPABASE_URL,
            anon_key_masked: "••••••••",
            show_restore_default: false,
        };
        ui_readest_status::build_readest_status_screen(t, s, &view)
    });

    shoot(&shots_dir, "status-custom-endpoints.png", |t, s| {
        let view = ReadestStatusView {
            status: status(),
            email: None,
            last_sync: None,
            pending_count: 0,
            is_syncing: false,
            auto_push: true,
            message: None,
            api_base: "http://192.168.1.5:8765/api",
            supabase_url: "http://192.168.1.5:8765",
            anon_key_masked: "••••••••",
            show_restore_default: true,
        };
        ui_readest_status::build_readest_status_screen(t, s, &view)
    });

    shoot(&shots_dir, "edit-defaults.png", |t, s| {
        let state =
            ReadestEditState::new(tr(Msg::ReadestApiBase).to_string(), DEFAULT_API_BASE, false);
        ui_readest_edit::build_readest_edit_screen(t, s, &state, &status())
    });

    for name in [
        "login-server-settings.png",
        "status-defaults.png",
        "status-custom-endpoints.png",
        "edit-defaults.png",
    ] {
        let p = shots_dir.join(name);
        assert!(p.exists(), "screenshot {} must exist", p.display());
        let meta = std::fs::metadata(&p).unwrap();
        assert!(
            meta.len() > 0,
            "screenshot {} must be non-empty",
            p.display()
        );
    }
}
