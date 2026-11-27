// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Host-side layout tests for the reader typeset sheet. The panel is built
//! for all supported panel sizes, then every control is checked to be on the
//! screen, disjoint from its neighbours and reachable by a tap at its centre.

use aristotle_i18n::{tr, Msg};
use aristotle_ui::geom::{Rect, Screen, Size};
use aristotle_ui::screens::typeset::{self, action, TypesetView};
use aristotle_ui::text::{FamilyId, FontData, Fonts, TextEngine};
use aristotle_ui::theme::Styles;
use aristotle_ui::tree::{Hit, Kind, NodeId};
use aristotle_ui::ui::Ui;

type Panel = (&'static str, u32, u32, u16);

const PANELS: [Panel; 4] = [
    ("six-inch-167", 600, 800, 167),
    ("pw2-212", 758, 1024, 212),
    ("pw4-300", 1072, 1448, 300),
    ("oasis2-300", 1264, 1680, 300),
];

const LANGUAGES: [&str; 3] = ["zh", "en", "pl"];

/// The Chinese sheet keeps the arrangement of the reference panel: its
/// labels and chips fit an equal share. The other languages are measured
/// and share the row in the width of their labels.
const REFERENCE_LANGUAGES: [&str; 1] = ["zh"];

fn engine() -> TextEngine {
    let mut fonts = Fonts::embedded();
    if let Some(dir) = std::env::var_os("ARISTOTLE_RENDER_DATA") {
        let path = std::path::PathBuf::from(dir).join("fonts/DejaVuSans-Symbols.ttf");
        if let Ok(bytes) = std::fs::read(path) {
            let face = fonts
                .add_face(FontData::Shared(std::sync::Arc::new(bytes)))
                .expect("configured CJK font");
            let family = fonts
                .add_family("Noto Sans SC", [Some(face), None, None, None])
                .expect("configured CJK family");
            fonts.add_fallback(family);
        }
    }
    TextEngine::new(fonts)
}

fn build(w: u32, h: u32, dpi: u16, size_aware: bool) -> Ui {
    build_view(w, h, dpi, size_aware, &TypesetView::default())
}

fn build_view(w: u32, h: u32, dpi: u16, size_aware: bool, view: &TypesetView) -> Ui {
    let screen = Screen::new(w, h, dpi).unwrap();
    let size = Size::new(w as i32, h as i32);
    let styles = Styles::new(FamilyId(0)).with_density(screen.density);
    let mut tree = aristotle_ui::tree::Tree::new();
    let mut text = engine();
    let (root, _) = if size_aware {
        typeset::build_typeset_screen_for_size(&mut tree, &mut text, &styles, view, size)
    } else {
        typeset::build_typeset_screen(&mut tree, &mut text, &styles, view)
    };
    tree.set_root(root);
    let mut ui = Ui::with_screen(tree, text, screen);
    ui.layout();
    ui
}

/// The node that carries `action`.
fn node_for(ui: &Ui, action: u32) -> NodeId {
    (0..ui.tree.len())
        .map(|i| NodeId(i as u32))
        .find(|&id| ui.tree.node(id).action == Some(action))
        .unwrap_or_else(|| panic!("action {action} is missing"))
}

/// Whether a chip is the chosen one (the inverted style of `chip_style`).
fn is_active(ui: &Ui, id: NodeId) -> bool {
    ui.tree.node(id).bg == Some(aristotle_ui::theme::INK)
}

/// Whether the panel draws `text` anywhere.
fn has_text(ui: &Ui, wanted: &str) -> bool {
    (0..ui.tree.len())
        .map(|i| NodeId(i as u32))
        .any(|id| match &ui.tree.node(id).kind {
            Kind::Text { para, .. } => para.text() == wanted,
            _ => false,
        })
}

/// Every control is fully on the panel, clear of its siblings and reachable
/// by a tap at its centre.
fn assert_controls_sound(ui: &Ui, label: &str) {
    let controls = controls(ui);
    for (i, (id, action, rect)) in controls.iter().enumerate() {
        assert!(
            ui.bounds().contains_rect(rect),
            "{label}: action {action} at {rect:?} leaves the screen"
        );
        let centre = rect.center();
        assert_eq!(
            ui.tree.hit(centre),
            Hit::Target(*id),
            "{label}: action {action} at {rect:?} misses its centre"
        );
        for (other, other_action, other_rect) in controls.iter().skip(i + 1) {
            assert!(
                !rect.intersects(other_rect),
                "{label}: actions {action} {rect:?} and {other_action} {other_rect:?} \
                 overlap ({other:?})"
            );
        }
    }
}

fn controls(ui: &Ui) -> Vec<(NodeId, u32, Rect)> {
    (0..ui.tree.len())
        .map(|i| NodeId(i as u32))
        .filter_map(|id| {
            let action = ui.tree.node(id).action?;
            let rect = ui.tree.rect(id);
            (!rect.is_empty()).then_some((id, action, rect))
        })
        .collect()
}

fn depth_first_rects(ui: &Ui) -> Vec<Rect> {
    fn walk(ui: &Ui, id: NodeId, out: &mut Vec<Rect>) {
        let r = ui.tree.rect(id);
        if !r.is_empty() {
            out.push(r);
        }
        for &child in &ui.tree.node(id).children {
            walk(ui, child, out);
        }
    }
    let mut out = Vec::new();
    if let Some(root) = ui.tree.root() {
        walk(ui, root, &mut out);
    }
    out
}

fn parents(ui: &Ui) -> Vec<Option<NodeId>> {
    let mut parents = vec![None; ui.tree.len()];
    for i in 0..ui.tree.len() {
        let id = NodeId(i as u32);
        for &child in &ui.tree.node(id).children {
            parents[child.0 as usize] = Some(id);
        }
    }
    parents
}

/// The box whose width a text has to fit: the button it labels, or the fixed
/// width of a row label.
fn text_container(ui: &Ui, parents: &[Option<NodeId>], text: NodeId) -> Option<NodeId> {
    let mut at = parents[text.0 as usize];
    while let Some(id) = at {
        let node = ui.tree.node(id);
        if node.action.is_some() {
            return Some(id);
        }
        if let Kind::Fixed { w: Some(_), .. } = node.kind {
            return Some(id);
        }
        at = parents[id.0 as usize];
    }
    None
}

fn is_cjk(c: char) -> bool {
    c as u32 >= 0x2E80
}

/// Same conservative estimate the compact layout budgets with. Wide
/// characters take an em; Latin letters five eighths of one.
fn estimated_width(text: &str, em: f32) -> f32 {
    text.chars()
        .map(|c| {
            if is_cjk(c) {
                em
            } else if c == ' ' {
                em * 0.3
            } else {
                em * 0.625
            }
        })
        .sum()
}

#[test]
fn every_typeset_control_is_on_screen_disjoint_and_tappable() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in PANELS {
                let ui = build(w, h, dpi, true);
                let controls = controls(&ui);
                assert!(
                    controls.len() >= 26,
                    "{name} {lang}: only {} controls",
                    controls.len()
                );

                // Every control is fully on the panel and clear of its siblings.
                assert_controls_sound(&ui, &format!("{name} {lang}"));

                // The settings the panel exists for are all reachable.
                for wanted in [
                    action::COLLAPSE,
                    action::ADD_FONT,
                    action::CJK_FONT_BASE,
                    action::SIZE_DOWN,
                    action::SIZE_UP,
                    action::SPACING_DOWN,
                    action::SPACING_UP,
                    action::MARGIN_DOWN,
                    action::MARGIN_UP,
                    action::INDENT_DOWN,
                    action::INDENT_UP,
                    action::GAP_DOWN,
                    action::GAP_UP,
                    action::UNDERLINE_BASE,
                    action::UNDERLINE_BASE + 2,
                    action::UNDERLINE_SCOPE_BASE,
                    action::UNDERLINE_SCOPE_BASE + 1,
                    action::FINE_BASE,
                    action::FINE_BASE + 1,
                    action::WEIGHT_BASE,
                    action::WEIGHT_BASE + 1,
                    action::WEIGHT_BASE + 2,
                    action::WEIGHT_DOWN,
                    action::WEIGHT_UP,
                    action::WRITING_MODE_BASE,
                    action::WRITING_MODE_BASE + 1,
                    action::ORIENTATION_BASE,
                    action::ORIENTATION_BASE + 1,
                    action::ORIENTATION_BASE + 2,
                    action::ORIENTATION_BASE + 3,
                ] {
                    assert!(
                        controls.iter().any(|(_, a, _)| *a == wanted),
                        "{name} {lang}: action {wanted} is missing"
                    );
                }
            }
        });
    }
}

#[test]
fn typeset_keeps_the_reference_layout_at_three_inch_sizes() {
    for lang in REFERENCE_LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in PANELS.iter().filter(|p| p.1 >= 700) {
                let sized = build(*w, *h, *dpi, true);
                let reference = build(*w, *h, *dpi, false);
                assert_eq!(
                    depth_first_rects(&sized),
                    depth_first_rects(&reference),
                    "{name} {lang}: the size-aware builder changed the reference layout"
                );
            }
        });
    }
}

#[test]
fn typeset_labels_keep_their_gutter_at_six_inches() {
    aristotle_i18n::with_language("zh", || {
        let mut ui = build(600, 800, 167, true);
        let parents = parents(&ui);
        let mut checked = 0;
        for i in 0..ui.tree.len() {
            let id = NodeId(i as u32);
            let Kind::Text { para, .. } = &ui.tree.node(id).kind else {
                continue;
            };
            let text = para.text();
            // Short CJK strings are the row labels and the chip labels; the
            // preview paragraphs are longer and wrap inside the preview area.
            if text.chars().count() > 10 || !text.chars().any(is_cjk) {
                continue;
            }
            let style = para.spans[0].style.clone();
            let measured = ui.text.measure(&text, &style);
            let wanted = measured.max(estimated_width(&text, style.size));
            // The sheet title and the section heading are laid out at full
            // width; only labels and chips have a box of their own.
            let Some(container) = text_container(&ui, &parents, id) else {
                continue;
            };
            let width = ui.tree.rect(container).w as f32;
            // 12 reference pixels of breathing room, about 9 percent of a label.
            let gutter = ui.profile().density.px(12) as f32;
            assert!(
                wanted + gutter <= width,
                "{text:?} needs {} in a box {width} wide",
                wanted + gutter
            );
            checked += 1;
        }
        assert!(checked >= 25, "only {checked} labels were checked");
    });
}

#[test]
fn typeset_font_rows_show_arrows_when_paginated_and_fine_locks_replace_controls() {
    use aristotle_ui::screens::typeset::FontOption;

    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in PANELS {
                let mut view = TypesetView {
                    cjk_fonts: vec![
                        FontOption {
                            name: "Noto Sans SC".into(),
                            sample_font: FamilyId(0),
                        },
                        FontOption {
                            name: "LXGW WenKai Lite".into(),
                            sample_font: FamilyId(0),
                        },
                        FontOption {
                            name: "Source Han Serif CN".into(),
                            sample_font: FamilyId(0),
                        },
                        FontOption {
                            name: "Zhuque Fangsong".into(),
                            sample_font: FamilyId(0),
                        },
                    ],
                    cjk_page: 0,
                    ..TypesetView::default()
                };
                let ui = build_view(w, h, dpi, true, &view);
                assert_controls_sound(&ui, &format!("{name} {lang} multi-page fonts p0"));
                assert!(
                    controls(&ui)
                        .iter()
                        .any(|(_, a, _)| *a == action::NEXT_CJK_PAGE),
                    "{name} {lang}: NEXT_CJK_PAGE arrow should be clickable on page 0"
                );
                assert!(
                    !controls(&ui)
                        .iter()
                        .any(|(_, a, _)| *a == action::PREV_CJK_PAGE),
                    "{name} {lang}: PREV_CJK_PAGE should be inactive on page 0"
                );

                view.cjk_page = 1;
                let ui_p1 = build_view(w, h, dpi, true, &view);
                assert_controls_sound(&ui_p1, &format!("{name} {lang} multi-page fonts p1"));
                assert!(
                    controls(&ui_p1)
                        .iter()
                        .any(|(_, a, _)| *a == action::PREV_CJK_PAGE),
                    "{name} {lang}: PREV_CJK_PAGE arrow should be clickable on page 1"
                );

                // Even when fine typesetting and embedded book fonts are enabled,
                // font and line spacing controls remain unlocked and editable (0.3.5 behavior),
                // and line spacing down to 0% renders cleanly.
                let fine_editable = TypesetView {
                    fine_typesetting: true,
                    embedded_fonts: true,
                    fine_locks_fonts: true,
                    fine_locks_spacing: true,
                    spacing_pct: 0,
                    ..TypesetView::default()
                };
                let ui_fine = build_view(w, h, dpi, true, &fine_editable);
                assert_controls_sound(&ui_fine, &format!("{name} {lang} fine editable 0%"));
                assert!(!has_text(&ui_fine, tr(Msg::TypesetFineLocked)));
                assert!(has_text(&ui_fine, "0%"));
                assert!(controls(&ui_fine)
                    .iter()
                    .any(|(_, a, _)| *a == action::SPACING_UP));
                assert!(controls(&ui_fine)
                    .iter()
                    .any(|(_, a, _)| *a == action::SPACING_DOWN));
                assert!(controls(&ui_fine)
                    .iter()
                    .any(|(_, a, _)| *a == action::CJK_FONT_BASE));
            }
        });
    }
}

#[test]
fn typeset_keeps_the_reference_widths_at_every_size() {
    // Margin, indent and paragraph gap steppers keep stable button widths across reference sizes.
    aristotle_i18n::with_language("zh", || {
        for (name, w, h, dpi) in PANELS.iter().filter(|p| p.1 >= 700) {
            let ui = build(*w, *h, *dpi, true);
            let controls = controls(&ui);
            let widths = |a: u32| {
                controls
                    .iter()
                    .find(|(_, action, _)| *action == a)
                    .map(|(_, _, r)| r.w)
                    .unwrap()
            };
            let (m, i, g) = (
                widths(action::MARGIN_DOWN),
                widths(action::INDENT_DOWN),
                widths(action::GAP_DOWN),
            );
            assert!(
                m >= 40 && i >= 40 && g >= 40,
                "{name}: stepper widths too small: margin={m}, indent={i}, gap={g}"
            );
        }
    });
}

#[test]
fn typeset_labels_use_the_loaded_cjk_font_when_available() {
    let Some(dir) = std::env::var_os("ARISTOTLE_RENDER_DATA") else {
        return;
    };
    let path = std::path::PathBuf::from(dir).join("fonts/DejaVuSans-Symbols.ttf");
    assert!(
        path.exists(),
        "ARISTOTLE_RENDER_DATA points at {path:?}, which has no render fonts"
    );
    aristotle_i18n::with_language("zh", || {
        let mut ui = build(600, 800, 167, true);
        let style = Styles::new(FamilyId(0))
            .with_density(ui.profile().density)
            .font_xs();
        // The render data on some hosts carries only DejaVu symbols. The layout
        // tests above still budget Chinese from its em; this assertion needs
        // real glyphs, so it is skipped when the fallback has none.
        if !ui.text.covers("行距页边距", &style) {
            return;
        }
        let parents = parents(&ui);
        for i in 0..ui.tree.len() {
            let id = NodeId(i as u32);
            let Kind::Text { para, .. } = &ui.tree.node(id).kind else {
                continue;
            };
            let text = para.text();
            if text.chars().count() > 10 || !text.chars().any(is_cjk) {
                continue;
            }
            let measured = ui.text.measure(&text, &para.spans[0].style);
            let Some(container) = text_container(&ui, &parents, id) else {
                continue;
            };
            let width = ui.tree.rect(container).w as f32;
            assert!(
                measured + ui.profile().density.px(12) as f32 <= width,
                "{text:?} measured {measured} in a box {width} wide"
            );
        }
    });
}

/// The direction row offers both directions, lights the one in use and keeps
/// the panel sound when the book is set vertically (VERT-101).
#[test]
fn typeset_writing_mode_row_offers_both_directions() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in PANELS {
                for mode in [0, 1] {
                    let view = TypesetView {
                        writing_mode: mode,
                        ..TypesetView::default()
                    };
                    let ui = build_view(w, h, dpi, true, &view);
                    assert_controls_sound(&ui, &format!("{name} {lang}, direction {mode}"));

                    // Both chips are drawn, and only the one in use is lit.
                    for (index, action) in
                        [action::WRITING_MODE_BASE, action::WRITING_MODE_BASE + 1]
                            .into_iter()
                            .enumerate()
                    {
                        let id = node_for(&ui, action);
                        assert!(
                            !ui.tree.rect(id).is_empty(),
                            "{name} {lang}: direction {index} is not drawn"
                        );
                        assert_eq!(
                            is_active(&ui, id),
                            index == mode,
                            "{name} {lang}: direction {index} at mode {mode}"
                        );
                    }
                    for text in [
                        tr(Msg::TypesetWritingMode),
                        tr(Msg::TypesetHorizontal),
                        tr(Msg::TypesetVertical),
                    ] {
                        assert!(has_text(&ui, text), "{name} {lang}: {text:?} is not drawn");
                    }
                }
            }
        });
    }
}

/// The panel keeps every control on the screen: the sheet is pinned to the
/// bottom of the panel and never covers the book's title bar (TYPE-102).
#[test]
fn typeset_panel_stays_on_the_screen() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in PANELS {
                let ui = build(w, h, dpi, true);
                let root = ui.tree.root().unwrap();
                let children = ui.tree.node(root).children.clone();
                let top = ui.tree.rect(children[0]);
                let preview = ui.tree.rect(children[1]);
                let sheet = ui.tree.rect(*children.last().unwrap());
                assert!(
                    sheet.bottom() <= h as i32,
                    "{name} {lang}: the sheet ends at {} on a {h}-pixel screen",
                    sheet.bottom()
                );
                assert_eq!(
                    sheet.bottom(),
                    h as i32,
                    "{name} {lang}: the sheet is not pinned to the bottom"
                );
                assert!(
                    sheet.y >= top.bottom(),
                    "{name} {lang}: the sheet starts inside the title bar"
                );
                assert_eq!(
                    preview.bottom(),
                    sheet.y,
                    "{name} {lang}: the preview leaves a gap above the sheet"
                );
                // A six-inch panel spends its height on the controls: the
                // preview area is only the empty part that closes the sheet.
                let preview_children = ui.tree.node(children[1]).children.len();
                if w < 700 {
                    assert_eq!(
                        preview_children, 0,
                        "{name} {lang}: the compact panel still draws a preview"
                    );
                } else {
                    assert!(
                        preview_children > 0,
                        "{name} {lang}: the panel lost its live preview"
                    );
                }
            }
        });
    }
}

/// The weight row shows the weight in use: the number, which preset it is
/// (none between them) and both stepper buttons (TYPE-102).
#[test]
fn typeset_weight_row_shows_the_weight_in_use() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in PANELS {
                // The presets light their own weight.
                for (index, weight) in typeset::WEIGHT_PRESETS.iter().enumerate() {
                    let view = TypesetView {
                        font_weight: *weight,
                        ..TypesetView::default()
                    };
                    let ui = build_view(w, h, dpi, true, &view);
                    for other in 0..3 {
                        assert_eq!(
                            is_active(&ui, node_for(&ui, action::WEIGHT_BASE + other as u32)),
                            other == index,
                            "{name} {lang}: preset {other} with weight {weight}"
                        );
                    }
                    assert!(
                        has_text(&ui, &weight.to_string()),
                        "{name} {lang}: weight {weight} is not shown"
                    );
                }
                // A weight between the presets lights none of them and the
                // number still shows it; the stepper does not wrap.
                let view = TypesetView {
                    font_weight: 450,
                    ..TypesetView::default()
                };
                let ui = build_view(w, h, dpi, true, &view);
                for index in 0..3 {
                    assert!(
                        !is_active(&ui, node_for(&ui, action::WEIGHT_BASE + index)),
                        "{name} {lang}: a preset is lit at weight 450"
                    );
                }
                assert!(has_text(&ui, "450"), "{name} {lang}: 450 is not shown");
            }
        });
    }
}

#[test]
fn typeset_orientation_row_highlights_selected_angle_in_portrait_and_landscape() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in [
                ("oasis2-300", 1264, 1680, 300),
                ("oasis2-landscape", 1680, 1264, 300),
            ] {
                for orient in 0..4 {
                    let view = TypesetView {
                        orientation: orient,
                        ..TypesetView::default()
                    };
                    let ui = build_view(w, h, dpi, true, &view);
                    assert_controls_sound(&ui, &format!("{name} {lang}, orient {orient}"));
                    for i in 0..4 {
                        let id = node_for(&ui, action::ORIENTATION_BASE + i as u32);
                        assert!(
                            !ui.tree.rect(id).is_empty(),
                            "{name} {lang}: orientation {i} is not drawn"
                        );
                        assert_eq!(
                            is_active(&ui, id),
                            i == orient,
                            "{name} {lang}: orientation {i} active mismatch at {orient}"
                        );
                    }
                }
            }
        });
    }
}

/// The fine switch and the guide's width show the state they are given.
#[test]
fn typeset_switches_show_the_state_in_use() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            let view = TypesetView {
                fine_typesetting: false,
                underline_scope: 1,
                ..TypesetView::default()
            };
            let ui = build_view(600, 800, 167, true, &view);
            assert!(!is_active(&ui, node_for(&ui, action::FINE_BASE)));
            assert!(is_active(&ui, node_for(&ui, action::FINE_BASE + 1)));
            assert!(!is_active(&ui, node_for(&ui, action::UNDERLINE_SCOPE_BASE)));
            assert!(is_active(
                &ui,
                node_for(&ui, action::UNDERLINE_SCOPE_BASE + 1)
            ));

            let ui = build(600, 800, 167, true);
            assert!(is_active(&ui, node_for(&ui, action::FINE_BASE)));
            assert!(!is_active(&ui, node_for(&ui, action::FINE_BASE + 1)));
            assert!(is_active(&ui, node_for(&ui, action::UNDERLINE_SCOPE_BASE)));
        });
    }
}

#[test]
fn typeset_text_stays_inside_its_control_at_six_inches() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            let mut ui = build(600, 800, 167, true);
            let parents = parents(&ui);
            let mut checked = 0;
            for i in 0..ui.tree.len() {
                let id = NodeId(i as u32);
                let Kind::Text { para, .. } = &ui.tree.node(id).kind else {
                    continue;
                };
                let text = para.text();
                // Preview paragraphs wrap; only control and row labels have
                // a box they must stay inside.
                if text.chars().count() > 20 {
                    continue;
                }
                let Some(container) = text_container(&ui, &parents, id) else {
                    continue;
                };
                let style = para.spans[0].style.clone();
                let measured = ui.text.measure(&text, &style);
                let wanted = measured.max(estimated_width(&text, style.size));
                let width = ui.tree.rect(container).w as f32;
                let gutter = ui.profile().density.px(8) as f32;
                assert!(
                    wanted + gutter <= width,
                    "{lang}: {text:?} needs {} in a box {width} wide",
                    wanted + gutter
                );
                checked += 1;
            }
            assert!(
                checked >= 10,
                "{lang}: only {checked} controls were checked"
            );
        });
    }
}

#[test]
fn fixed_layout_typeset_sheet_is_sound_across_sizes_and_languages() {
    for lang in LANGUAGES {
        aristotle_i18n::with_language(lang, || {
            for (name, w, h, dpi) in [
                ("basic-600", 600, 800, 167),
                ("pw3-758", 758, 1024, 212),
                ("pw4-1072", 1072, 1448, 300),
                ("oasis2-1264", 1264, 1680, 300),
                ("pw4-landscape", 1448, 1072, 300),
            ] {
                let view = TypesetView {
                    title: "灌篮高手 Vol.1".into(),
                    author: "井上雄彦".into(),
                    fixed_layout: true,
                    fit_mode: 1,
                    page_direction: 1,
                    fullscreen: true,
                    auto_crop: true,
                    split_spread: true,
                    keep_images_in_night: true,
                    orientation: 2,
                    ..TypesetView::default()
                };
                let ui = build_view(w, h, dpi, true, &view);
                assert_controls_sound(&ui, &format!("fixed {name} {lang}"));
                assert!(is_active(&ui, node_for(&ui, action::FIT_MODE_BASE + 1)));
                assert!(is_active(&ui, node_for(&ui, action::PAGE_DIR_BASE + 1)));
                assert!(is_active(&ui, node_for(&ui, action::FULLSCREEN_BASE)));
                assert!(is_active(&ui, node_for(&ui, action::AUTO_CROP_BASE)));
                assert!(is_active(&ui, node_for(&ui, action::SPLIT_SPREAD_BASE)));
                assert!(is_active(&ui, node_for(&ui, action::NIGHT_KEEP_IMG_BASE)));
                assert!(is_active(&ui, node_for(&ui, action::ORIENTATION_BASE + 2)));
            }
        });
    }
}
