// SPDX-License-Identifier: AGPL-3.0-or-later

use aristotle_i18n::{tr, Msg};
use aristotle_ui::canvas::Canvas;
use aristotle_ui::geom::Screen as UiScreen;
use aristotle_ui::screens::mine::{Account, Entries, Mine, MineUi, Stats, Streak};
use aristotle_ui::screens::readest_login::{
    self as ui_readest_login, ReadestLoginField, ReadestLoginState,
};
use aristotle_ui::screens::readest_status::{self as ui_readest_status, ReadestStatusView};
use aristotle_ui::status::StatusLine;
use aristotle_ui::text::{FamilyId, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::Tree;
use aristotle_ui::ui::Ui;
use std::path::{Path, PathBuf};

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

#[test]
fn generate_sync004_task_screenshots() {
    // The committed shots are rewritten only on request (`UPDATE_SHOTS=1`);
    // an ordinary test run writes to the temp directory so it leaves the tree clean.
    let shots_dir = if std::env::var_os("UPDATE_SHOTS").is_some() {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../docs/tasks/SYNC-004/shots")
    } else {
        std::env::temp_dir().join("aristotle-sync004-shots")
    };
    let _ = std::fs::create_dir_all(&shots_dir);

    let screen_profile = UiScreen::new(1072, 1448, 300).unwrap();
    let styles = Styles::new(FamilyId(0)).with_density(screen_profile.density);

    let dummy_status = StatusLine {
        clock: "14:30",
        battery: Some(0.88),
        charging: false,
    };

    // 1. login-empty.png
    {
        let state = ReadestLoginState::new();
        let text = TextEngine::new(Fonts::embedded());
        let mut tree = Tree::new();
        let root =
            ui_readest_login::build_readest_login_screen(&mut tree, &styles, &state, &dummy_status);
        tree.set_root(root);
        let mut ui = Ui::with_screen(tree, text, screen_profile);
        ui.layout();
        ui.render();
        save_png(ui.screen(), &shots_dir.join("login-empty.png"));
    }

    // 2. login-filled.png
    {
        let mut state = ReadestLoginState::new();
        state.email = "reader@example.com".to_string();
        state.password = "secretpass123".to_string();
        state.active_field = ReadestLoginField::Password;
        state.keyboard.committed = state.password.clone();
        let text = TextEngine::new(Fonts::embedded());
        let mut tree = Tree::new();
        let root =
            ui_readest_login::build_readest_login_screen(&mut tree, &styles, &state, &dummy_status);
        tree.set_root(root);
        let mut ui = Ui::with_screen(tree, text, screen_profile);
        ui.layout();
        ui.render();
        save_png(ui.screen(), &shots_dir.join("login-filled.png"));
    }

    // 3. login-error.png
    {
        let mut state = ReadestLoginState::new();
        state.email = "reader@example.com".to_string();
        state.error_message = Some(tr(Msg::ReadestErrLoginFailed).to_string());
        let text = TextEngine::new(Fonts::embedded());
        let mut tree = Tree::new();
        let root =
            ui_readest_login::build_readest_login_screen(&mut tree, &styles, &state, &dummy_status);
        tree.set_root(root);
        let mut ui = Ui::with_screen(tree, text, screen_profile);
        ui.layout();
        ui.render();
        save_png(ui.screen(), &shots_dir.join("login-error.png"));
    }

    // 4. mine-signed-in.png
    {
        let mine = Mine {
            status: dummy_status,
            account: Account::SignedIn {
                name: "reader@example.com".to_string(),
                last_sync: "2026-11-09 14:30".to_string(),
            },
            stats: Stats {
                today_minutes: 45,
                goal_minutes: 60,
                week_hours: 4.5,
                total_hours: 120,
                finished: 12,
            },
            streak: Streak {
                days: 7,
                week: [true; 7],
            },
            entries: Entries {
                highlights: 34,
                notes: 8,
                version: "0.3.0".to_string(),
            },
        };
        let mut text = TextEngine::new(Fonts::embedded());
        let (tree, _) = MineUi::build(&mut text, &styles, &mine);
        let mut ui = Ui::with_screen(tree, text, screen_profile);
        ui.layout();
        ui.render();
        save_png(ui.screen(), &shots_dir.join("mine-signed-in.png"));
    }

    // 5. status-idle.png
    {
        let view = ReadestStatusView {
            status: dummy_status,
            email: Some("reader@example.com"),
            last_sync: Some("2026-11-09 14:30"),
            pending_count: 0,
            is_syncing: false,
            auto_push: true,
            message: None,
            api_base: "https://web.readest.com/api",
            supabase_url: "https://auth.readest.com",
            anon_key_masked: "••••••••",
            show_restore_default: false,
        };
        let text = TextEngine::new(Fonts::embedded());
        let mut tree = Tree::new();
        let root = ui_readest_status::build_readest_status_screen(&mut tree, &styles, &view);
        tree.set_root(root);
        let mut ui = Ui::with_screen(tree, text, screen_profile);
        ui.layout();
        ui.render();
        save_png(ui.screen(), &shots_dir.join("status-idle.png"));
    }

    // 6. status-syncing.png
    {
        let view = ReadestStatusView {
            status: dummy_status,
            email: Some("reader@example.com"),
            last_sync: Some("2026-11-09 14:30"),
            pending_count: 3,
            is_syncing: true,
            auto_push: true,
            message: Some(tr(Msg::ReadestSyncing)),
            api_base: "https://web.readest.com/api",
            supabase_url: "https://auth.readest.com",
            anon_key_masked: "••••••••",
            show_restore_default: false,
        };
        let text = TextEngine::new(Fonts::embedded());
        let mut tree = Tree::new();
        let root = ui_readest_status::build_readest_status_screen(&mut tree, &styles, &view);
        tree.set_root(root);
        let mut ui = Ui::with_screen(tree, text, screen_profile);
        ui.layout();
        ui.render();
        save_png(ui.screen(), &shots_dir.join("status-syncing.png"));
    }

    // 7. status-error.png
    {
        let view = ReadestStatusView {
            status: dummy_status,
            email: Some("reader@example.com"),
            last_sync: Some("2026-11-09 14:30"),
            pending_count: 2,
            is_syncing: false,
            auto_push: true,
            message: Some(tr(Msg::ReadestErrNetwork)),
            api_base: "https://web.readest.com/api",
            supabase_url: "https://auth.readest.com",
            anon_key_masked: "••••••••",
            show_restore_default: false,
        };
        let text = TextEngine::new(Fonts::embedded());
        let mut tree = Tree::new();
        let root = ui_readest_status::build_readest_status_screen(&mut tree, &styles, &view);
        tree.set_root(root);
        let mut ui = Ui::with_screen(tree, text, screen_profile);
        ui.layout();
        ui.render();
        save_png(ui.screen(), &shots_dir.join("status-error.png"));
    }

    for name in &[
        "login-empty.png",
        "login-filled.png",
        "login-error.png",
        "mine-signed-in.png",
        "status-idle.png",
        "status-syncing.png",
        "status-error.png",
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
