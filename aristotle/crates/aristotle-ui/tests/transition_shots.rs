// SPDX-License-Identifier: AGPL-3.0-or-later

use aristotle_ui::geom::Screen;
use aristotle_ui::screens::transition::{self, Stage};
use aristotle_ui::text::{FamilyId, FontData, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{Kind, NodeId};
use aristotle_ui::ui::Ui;

#[test]
fn transition_stages_fit_both_devices_and_keep_the_logo_still() {
    for lang in ["zh", "en"] {
        assert!(aristotle_i18n::set_language(lang));
        for (name, width, height) in [("pw4", 1072, 1448), ("ko2", 1264, 1680)] {
            let profile = Screen::new(width, height, 300).unwrap();
            let styles = Styles::new(FamilyId(0));
            let mut fonts = Fonts::embedded();
            if let Some(data) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
                let bytes = std::fs::read(
                    std::path::PathBuf::from(data).join("fonts/DejaVuSans-Symbols.ttf"),
                )
                .unwrap();
                let face = fonts
                    .add_face(FontData::Shared(std::sync::Arc::new(bytes)))
                    .unwrap();
                let family = fonts
                    .add_family("Noto Sans SC", [Some(face), None, None, None])
                    .unwrap();
                fonts.add_fallback(family);
            }
            let mut ui = Ui::with_screen(
                transition::build(&styles, Stage::Prepare),
                TextEngine::new(fonts),
                profile,
            );
            ui.render();
            let first = ui.screen().clone();
            for stage in [
                Stage::Prepare,
                Stage::Loading,
                Stage::Ready,
                Stage::Saving,
                Stage::Restoring,
                Stage::Waiting,
                Stage::Slow,
            ] {
                ui.set_tree(transition::build(&styles, stage));
                ui.render();
                for index in 0..ui.tree.len() {
                    let id = NodeId(index as u32);
                    let node = ui.tree.node(id);
                    let rect = ui.tree.rect(id);
                    assert!(
                        ui.bounds().contains_rect(&rect),
                        "{lang}/{name}/{stage:?}: {:?}",
                        rect
                    );
                    if let Kind::Text {
                        cache: Some((_, layout)),
                        ..
                    } = &node.kind
                    {
                        assert!(layout.height <= rect.h as f32, "transition text is clipped");
                    }
                }
                let progress = (0..ui.tree.len())
                    .map(|index| NodeId(index as u32))
                    .find(|&id| matches!(ui.tree.node(id).kind, Kind::Row { .. }))
                    .unwrap();
                assert_eq!(
                    ui.tree.rect(progress).h,
                    12,
                    "progress indicator lost its height"
                );
                for &segment in &ui.tree.node(progress).children {
                    let rect = ui.tree.rect(segment);
                    assert!(
                        rect.h == 12 && rect.w > 0,
                        "progress segment collapsed: {rect:?}"
                    );
                    assert!(
                        ui.screen().get(rect.x + rect.w / 2, rect.y + rect.h / 2) < 255,
                        "progress segment was not painted"
                    );
                }
                // Changing the stage only alters content below the brand mark.
                let mark = (0..ui.tree.len())
                    .map(|index| NodeId(index as u32))
                    .find(|&id| matches!(ui.tree.node(id).kind, Kind::Custom(_)))
                    .unwrap();
                let rect = ui.tree.rect(mark);
                assert!(
                    first.diff_bounds(ui.screen(), rect).is_none(),
                    "logo moved or changed"
                );
                if let Some(out) = std::env::var_os("TRANSITION_SHOTS") {
                    let out = std::path::PathBuf::from(out);
                    std::fs::create_dir_all(&out).unwrap();
                    let file =
                        std::fs::File::create(out.join(format!("{name}-{lang}-{stage:?}.png")))
                            .unwrap();
                    let mut encoder = png::Encoder::new(file, width, height);
                    encoder.set_color(png::ColorType::Grayscale);
                    encoder.set_depth(png::BitDepth::Eight);
                    let mut canvas = ui.screen().clone();
                    canvas.quantize16();
                    encoder
                        .write_header()
                        .unwrap()
                        .write_image_data(&canvas.packed())
                        .unwrap();
                }
            }
        }
    }
}
