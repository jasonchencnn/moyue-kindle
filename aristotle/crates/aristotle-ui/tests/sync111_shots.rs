// SPDX-License-Identifier: AGPL-3.0-or-later

//! SYNC-111 shots: the two cloud-sync buttons and their one-line result in the
//! reader menu, and the automatic-push switch on the Readest status screen.
//!
//! English and Chinese, day mode. Chinese needs a CJK face that
//! `Fonts::embedded()` does not carry: point `ARISTOTLE_RENDER_DATA` at a
//! device build (`tools/build-render.sh`, `.../device/data`) and the run picks
//! up its fallback face; without it the Chinese shots are skipped and named so
//! in the test output (the English ones always run).

use aristotle_i18n::{tr, with_language, Msg};
use aristotle_ui::canvas::Canvas;
use aristotle_ui::geom::Screen as UiScreen;
use aristotle_ui::screens::reader::{build_menu, MenuSpec, Settings};
use aristotle_ui::screens::readest_status::{self as ui_readest_status, ReadestStatusView};
use aristotle_ui::status::StatusLine;
use aristotle_ui::text::{FaceId, FamilyId, FontData, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{Kind, Node, NodeId, Tree};
use aristotle_ui::ui::Ui;
use std::path::{Path, PathBuf};

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

/// The embedded fonts, plus the device build's Chinese fallback when its data
/// directory is pointed at (the same rule the preview uses).
fn fonts() -> Fonts {
    let mut fonts = Fonts::embedded();
    let Some(dir) = std::env::var_os("ARISTOTLE_RENDER_DATA") else {
        return fonts;
    };
    let path = PathBuf::from(dir).join("fonts/DejaVuSans-Symbols.ttf");
    let Ok(bytes) = std::fs::read(path) else {
        return fonts;
    };
    let Ok(face): Result<FaceId, _> = fonts.add_face(FontData::Shared(std::sync::Arc::new(bytes)))
    else {
        return fonts;
    };
    if let Some(family) = fonts.add_family("Noto Sans SC", [Some(face), None, None, None]) {
        fonts.add_fallback(family);
    }
    fonts
}

fn status() -> StatusLine<'static> {
    StatusLine {
        clock: "14:30",
        battery: Some(0.88),
        charging: false,
    }
}

fn shoot(shots_dir: &Path, name: &str, build: impl Fn(&mut Tree, &Styles) -> NodeId) {
    // The target device: Kindle Oasis 2, 1264 x 1680 at 300 dpi.
    let profile = UiScreen::new(1264, 1680, 300).unwrap();
    let styles = Styles::new(FamilyId(0)).with_density(profile.density);
    let mut tree = Tree::new();
    let root = build(&mut tree, &styles);
    tree.set_root(root);
    let text = TextEngine::new(fonts());
    let mut ui = Ui::with_screen(tree, text, profile);
    ui.layout();
    ui.render();
    save_png(ui.screen(), &shots_dir.join(name));
}

/// The reader menu over an empty page, with `message` on its sync line.
fn reader_menu(message: &str) -> impl Fn(&mut Tree, &Styles) -> NodeId {
    let message = message.to_string();
    move |t: &mut Tree, s: &Styles| {
        let page = t.spacer(1);
        let spec = MenuSpec {
            title: if aristotle_i18n::language() == "zh" {
                "红楼梦"
            } else {
                "Fedon"
            },
            families: None,
            font_list: true,
            night: true,
            sync: true,
            top_extra: 0,
            bottom_extra: 0,
        };
        let menu = build_menu(t, s, &spec, &Settings::default());
        if let Some(line) = menu.sync_status {
            t.set_text(line, &message);
            t.set_hidden(line, message.is_empty());
        }
        let root = t.add(Node::new(Kind::Stack), [page, menu.root]);
        t.set_hidden(menu.root, false);
        root
    }
}

fn status_screen(auto_push: bool, failed: bool) -> impl Fn(&mut Tree, &Styles) -> NodeId {
    let message = failed.then(|| {
        aristotle_i18n::tf!(
            Msg::ReadestSyncFailedStage,
            error = tr(Msg::ReadestErrNetwork),
            stage = tr(Msg::ReadestStageProgress)
        )
    });
    move |t: &mut Tree, s: &Styles| {
        let view = ReadestStatusView {
            status: status(),
            email: Some("reader@example.com"),
            last_sync: Some("21:30"),
            pending_count: 0,
            is_syncing: false,
            message: message.as_deref(),
            auto_push,
            api_base: DEFAULT_API_BASE,
            supabase_url: DEFAULT_SUPABASE_URL,
            anon_key_masked: "••••••••",
            show_restore_default: false,
        };
        ui_readest_status::build_readest_status_screen(t, s, &view)
    }
}

#[test]
fn generate_sync111_task_screenshots() {
    // The committed shots are rewritten only on request (`UPDATE_SHOTS=1`);
    // an ordinary run writes to the temp directory so it leaves the tree clean.
    let shots_dir = if std::env::var_os("UPDATE_SHOTS").is_some() {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../docs/tasks/SYNC-111/shots")
    } else {
        std::env::temp_dir().join("aristotle-sync111-shots")
    };
    let _ = std::fs::create_dir_all(&shots_dir);
    let has_cjk = fonts().family_by_name("Noto Sans SC").is_some();

    for (code, prefix, do_render) in [
        ("en", "en", true),
        ("zh", "zh", has_cjk),
        ("pl", "pl", has_cjk),
    ] {
        if !do_render {
            eprintln!("skipping {code} shots: no CJK fallback (set ARISTOTLE_RENDER_DATA)");
            continue;
        }
        with_language(code, || {
            shoot(
                &shots_dir,
                &format!("{prefix}-reader-menu-synced.png"),
                reader_menu(""),
            );
            shoot(
                &shots_dir,
                &format!("{prefix}-reader-menu-push-done.png"),
                reader_menu(tr(Msg::ReaderSyncPushDone)),
            );
            shoot(
                &shots_dir,
                &format!("{prefix}-reader-menu-push-failed.png"),
                reader_menu(&aristotle_i18n::tf!(
                    Msg::ReaderSyncFailed,
                    error = tr(Msg::ReadestErrNetwork)
                )),
            );
            shoot(
                &shots_dir,
                &format!("{prefix}-readest-status-auto-push-on.png"),
                status_screen(true, false),
            );
            shoot(
                &shots_dir,
                &format!("{prefix}-readest-status-auto-push-off.png"),
                status_screen(false, true),
            );
        });
        assert!(has_shots(&shots_dir, prefix), "{prefix} shots must exist");
    }
}

fn has_shots(dir: &Path, prefix: &str) -> bool {
    let mut found = 0;
    for name in [
        "reader-menu-synced.png",
        "reader-menu-push-done.png",
        "reader-menu-push-failed.png",
        "readest-status-auto-push-on.png",
        "readest-status-auto-push-off.png",
    ] {
        let p = dir.join(format!("{prefix}-{name}"));
        assert!(p.exists(), "screenshot {} must exist", p.display());
        assert!(std::fs::metadata(&p).unwrap().len() > 0, "{}", p.display());
        found += 1;
    }
    found == 5
}
