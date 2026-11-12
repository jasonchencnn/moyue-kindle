// SPDX-License-Identifier: AGPL-3.0-or-later
//! The page where the reader picks the screensaver's own picture.
//!
//! The pictures come from the `screensavers/` folder on the Kindle's
//! root: the reader puts them there over USB, or through the Wi-Fi
//! transfer page. This screen shows them as a grid of thumbnails, the
//! first cell of the first page being the rotation ("all pictures"),
//! and lets the reader pick one.
//!
//! The measurements are the prototype's, in reference pixels:
//! `docs/ui/prototype-v2/mcode/SleepImages.dc.html` and the reference
//! pictures `sleep-images.png` and `sleep-images-empty.png`.
//!
//! This module draws only: which pictures there are, which one is
//! picked and which page is on screen is decided by the app, which
//! knows the folder and the settings.

use crate::geom::{Insets, Point, Rect, Size};
use crate::icons::Icon;
use crate::screens::home::pet::CAT_MASK;
use crate::screens::library::status_line;
use crate::text::{Align, Paragraph, TextEngine};
use crate::theme::{home_margin, Styles, INK, INK_MID, PAPER};
use crate::tree::{
    ActionId, Constraints, Cross, Feedback, Kind, Node, NodeId, PaintCx, Tree, Widget,
};
use crate::widgets::GrayImage;
use aristotle_i18n::{tf, tr, Msg};

pub mod action {
    use crate::tree::ActionId;
    /// The back arrow of the header: back to the settings.
    pub const BACK: ActionId = 1;
    /// The previous page of the grid.
    pub const PREV_PAGE: ActionId = 2;
    /// The next page of the grid.
    pub const NEXT_PAGE: ActionId = 3;
    /// The "all pictures" cell: the screensaver walks through the folder.
    pub const ROTATE_ALL: ActionId = 4;
    /// `PICTURE + i` picks the i-th picture of the page.
    pub const PICTURE: ActionId = 10;
    /// The button of the empty state: the Wi-Fi transfer page.
    pub const WIFI_TRANSFER: ActionId = 30;
}

/// The width of one cell of the grid, in reference pixels.
pub const CELL_W: i32 = 270;
/// The height of one cell of the grid: 3:4, like the prototype.
pub const CELL_H: i32 = 360;
/// The round corners of a cell.
const CELL_RADIUS: f32 = 14.0;
/// The thin frame of a picture cell.
const CELL_BORDER: u8 = 0xDD;
/// The frame of the picked cell, in reference pixels.
const PICK_FRAME: i32 = 4;
/// The size of the round tick in the corner of the picked cell.
const TICK: i32 = 64;
/// The gap between the picture and the name under it.
const LABEL_GAP: i32 = 8;

/// One picture of the grid.
#[derive(Clone)]
pub struct Picture {
    /// The file name, shown under the thumbnail.
    pub name: String,
    /// The thumbnail, when the picture could be read.
    pub thumb: Option<GrayImage>,
    /// Whether this is the picture the screensaver shows.
    pub picked: bool,
}

/// The state of the page.
pub struct View<'a> {
    /// The status bar: the clock and the battery.
    pub status: crate::screens::library::StatusLine<'a>,
    /// The pictures of the page, in the order they are shown.
    pub pictures: &'a [Picture],
    /// Whether the rotation ("all pictures") cell is on this page: the
    /// first page of a folder that has pictures.
    pub rotate_all: bool,
    /// Whether the screensaver walks through the folder. The cell stays
    /// on the first page when a picture is picked; only the tick moves.
    pub rotation_picked: bool,
    /// The file name the screensaver shows, as the settings know it. The
    /// header uses it on every page, even when the picture is not on this
    /// one. `None` means the rotation.
    pub current: Option<&'a str>,
    /// The page on screen, counting from zero.
    pub page: usize,
    /// How many pages there are.
    pub pages: usize,
}

/// The nodes the app updates after a pick.
pub struct SleepImages {
    /// The grid of pictures.
    pub grid: NodeId,
    /// The label of the header that says what is picked now.
    pub current: NodeId,
    /// The label of the page number.
    pub pager_label: NodeId,
    /// The button of the previous page.
    pub prev: NodeId,
    /// The button of the next page.
    pub next: NodeId,
    /// The cell of the rotation, when the page has one.
    pub rotate: Option<NodeId>,
    /// The cells of the pictures, in the order they are shown.
    pub cells: Vec<NodeId>,
}

/// The header of the page: the back arrow, the title and what is picked.
///
/// The empty state has nothing picked, so it shows no "current" line.
/// The header keeps the page margins, like the list and the pager; a
/// long file name is cut off with an ellipsis instead of running off the
/// screen.
fn header(t: &mut Tree, s: &Styles, current: Option<&str>) -> (NodeId, Option<NodeId>) {
    let back = t.icon(Icon::ChevronLeft, 44, INK);
    let back = t.button(
        Node::new(Kind::MinSize(Size::new(72, 72))),
        back,
        action::BACK,
    );
    let title = t.label(tr(Msg::SleepImagesTitle), s.display());
    // The title keeps its natural width, so a longer translation (Polish,
    // English) does not wrap; what is picked now takes the room that is
    // left and is trimmed with an ellipsis when the file name is long.
    let left = t.add(Node::row(20).cross(Cross::Center), [back, title]);
    let current_label = current.map(|text| {
        let mut para = Paragraph::plain(
            text,
            s.font_px(32).gray(INK_MID).line_height(1.2),
            Align::Right,
        );
        para.style.max_lines = Some(1);
        t.text(para)
    });
    let row = match current_label {
        Some(label) => {
            let spacer = t.spacer(1);
            t.add(Node::row(12).cross(Cross::Center), [left, spacer, label])
        }
        None => t.add(Node::row(0).cross(Cross::Center), [left]),
    };
    let head = t.fixed(None, Some(112), Some(row));
    let head = t.pad(Insets::new(0, home_margin(), 0, home_margin()), head);
    (head, current_label)
}

/// The frame and the tick of the picked cell.
///
/// The tick is the prototype's: a black disc of 64 px in the corner and
/// a white check of 36 px on it.
fn draw_picked(p: &mut crate::canvas::Painter, r: Rect) {
    p.stroke_round_rect(r, CELL_RADIUS, PICK_FRAME as f32, INK);
    let c = Point {
        x: r.right() - 10 - TICK / 2,
        y: r.y + 10 + TICK / 2,
    };
    p.fill_circle(c, (TICK / 2) as f32, INK);
    // The check is 36 px wide: the path is 24 units long.
    let s = 36.0 / 24.0;
    let t = zeno::Transform::scale(s, s).then_translate(
        c.x as f32 - (TICK / 2) as f32 + (TICK as f32 - 36.0) / 2.0,
        c.y as f32 - (TICK / 2) as f32 + (TICK as f32 - 36.0) / 2.0,
    );
    p.stroke_path("M 5 12.5 L 9.5 17 L 19 7.5", 3.0, Some(t), PAPER);
}

/// The corners of a cell: how much of every pixel lies outside the
/// round rectangle.
///
/// The value is 255 outside the shape (the pixel is painted over) and 0
/// inside it; the edge in between is soft.
fn round_cover(w: i32, h: i32, radius: f32) -> Vec<u8> {
    let mut mask = vec![0u8; (w * h) as usize];
    let r = radius.min((w.min(h)) as f32 / 2.0);
    if r <= 0.0 {
        return mask;
    }
    // Only the four corners are rounded; the rest of the cell is whole.
    // Every corner gets its own centre, mirrored on both axes: using one
    // direction for all four paints the right corners from the wrong side
    // of the curve.
    for (ox, oy, cx, cy) in [
        (0, 0, r, r),
        (w - r as i32, 0, w as f32 - r, r),
        (0, h - r as i32, r, h as f32 - r),
        (w - r as i32, h - r as i32, w as f32 - r, h as f32 - r),
    ] {
        for dy in 0..r as i32 {
            for dx in 0..r as i32 {
                let px = ox + dx;
                let py = oy + dy;
                if px < 0 || py < 0 || px >= w || py >= h {
                    continue;
                }
                let d =
                    ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt() - r;
                let a = ((d + 0.5) * 255.0).clamp(0.0, 255.0) as u8;
                mask[py as usize * w as usize + px as usize] = a;
            }
        }
    }
    mask
}

/// The cell of the rotation: the two arrows of a circle.
struct RotateAll {
    picked: bool,
}

impl Widget for RotateAll {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(CELL_W, CELL_H))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        p.fill_round_rect(r, CELL_RADIUS, PAPER);
        // The prototype's sign: two arcs with an arrow head each, 120 px
        // wide, in a 24 unit grid.
        let s = 120.0 / 24.0;
        let x = r.x as f32 + (r.w as f32 - 120.0) / 2.0;
        let y = r.y as f32 + (r.h as f32 - 120.0) / 2.0;
        let t = zeno::Transform::scale(s, s).then_translate(x, y);
        p.stroke_path("M 20 11 A 8 8 0 0 0 5.7 6.1 L 4 8", 1.6, Some(t), INK);
        p.stroke_path("M 4 4 L 4 8 L 8 8", 1.6, Some(t), INK);
        p.stroke_path("M 4 13 A 8 8 0 0 0 18.3 17.9 L 20 16", 1.6, Some(t), INK);
        p.stroke_path("M 20 20 L 20 16 L 16 16", 1.6, Some(t), INK);
        if self.picked {
            draw_picked(p, r);
        } else {
            p.stroke_round_rect(r, CELL_RADIUS, 2.0, INK);
        }
    }
}

/// The thumbnail of a picture, with the frame and the tick of the picked
/// one.
struct Thumb {
    image: Option<GrayImage>,
    picked: bool,
}

impl Widget for Thumb {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(CELL_W, CELL_H))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let p = &mut *cx.painter;
        match &self.image {
            Some(img) => {
                // The picture fills the cell and is cropped in the
                // middle, like the prototype's `object-fit: cover`.
                let crop = cover_to(img, r.w as u32, r.h as u32);
                p.blit_gray(r.x, r.y, r.w as u32, r.h as u32, r.w as usize, &crop);
                // The corners of the cell are round: the picture is cut
                // off there, like the prototype's `overflow: hidden`.
                let mask = round_cover(r.w, r.h, CELL_RADIUS);
                p.blend_mask(r.x, r.y, r.w as u32, r.h as u32, &mask, PAPER);
            }
            None => p.fill_round_rect(r, CELL_RADIUS, PAPER),
        }
        if self.picked {
            draw_picked(p, r);
        } else {
            // The frame of the picture cell: a thin light line, the
            // prototype's `cardBorder`.
            p.stroke_round_rect(r, CELL_RADIUS, 2.0, CELL_BORDER);
        }
    }
}

/// The picture at `w` × `h`: scaled up until it covers the size, then
/// cut off in the middle.
fn cover_to(img: &GrayImage, w: u32, h: u32) -> Vec<u8> {
    if img.width == 0 || img.height == 0 || w == 0 || h == 0 {
        return Vec::new();
    }
    let scale = (w as f32 / img.width as f32).max(h as f32 / img.height as f32);
    let sw = (img.width as f32 * scale).ceil() as u32;
    let sh = (img.height as f32 * scale).ceil() as u32;
    let scaled = if (sw, sh) == (img.width, img.height) {
        img.clone()
    } else {
        img.resized(sw, sh)
    };
    let ox = (sw - w) / 2;
    let oy = (sh - h) / 2;
    let mut out = vec![0u8; (w * h) as usize];
    for row in 0..h {
        let s = ((row + oy) * sw + ox) as usize;
        let d = (row * w) as usize;
        let n = w as usize;
        out[d..d + n].copy_from_slice(&scaled.pixels[s..s + n]);
    }
    out
}

/// The fishing cat of the empty state: the cat, its line and the ripples.
struct EmptyCat;

impl Widget for EmptyCat {
    fn measure(&mut self, c: Constraints, _text: &mut TextEngine) -> Size {
        c.constrain(Size::new(322, 280))
    }

    fn paint(&self, r: Rect, cx: &mut PaintCx) {
        let x = r.x;
        let y = r.y;
        let p = &mut *cx.painter;
        p.fill_rect(Rect::new(x + 284, y + 11, 2, 189), INK);
        p.blend_mask(x, y, 287, 280, CAT_MASK, INK);
        p.stroke_path(
            "M 13 8 L 13 19 C 13 26 5 26 5 20 L 2 16",
            2.0,
            Some(zeno::Transform::translation(
                x as f32 + 272.0,
                y as f32 + 192.0,
            )),
            INK,
        );
        p.stroke_path(
            "M 2 10 C 8 5.33 14 5.33 20 10 C 26 14.67 32 14.67 38 10 C 44 5.33 50 5.33 56 10 C 62 14.67 67 14.67 72 10 \
             M 12 32 C 18 27.33 24 27.33 30 32 C 36 36.67 42 36.67 48 32 C 54 27.33 60 27.33 66 32",
            2.0,
            Some(zeno::Transform::translation(x as f32 + 248.0, y as f32 + 228.0)),
            INK,
        );
    }
}

/// One cell of the grid: the picture and the name under it.
fn cell(t: &mut Tree, s: &Styles, picture: NodeId, name: &str, picked: bool) -> NodeId {
    let mut label = Paragraph::plain(
        name,
        if picked {
            s.font_px(32).bold().line_height(1.2)
        } else {
            s.font_px(32).gray(INK_MID).line_height(1.2)
        },
        Align::Left,
    );
    // A long file name is cut off with an ellipsis, not wrapped.
    label.style.max_lines = Some(1);
    let label = t.text(label);
    let gap = t.gap(0, LABEL_GAP);
    t.add(Node::column(0), [picture, gap, label])
}

/// Builds the page.
///
/// `view.pictures` holds the pictures of the page; the rotation cell is
/// put before them when `view.rotate_all` is set, which is only the
/// case on the first page.
///
/// `width` is the width of the screen the page is laid out for: the
/// three columns take the same room as the prototype's, and the rest
/// of the width is left between them.
pub fn build(s: &Styles, view: &View<'_>, width: i32) -> (Tree, SleepImages) {
    let mut t = Tree::new();

    let status = status_line(&mut t, s, &view.status).root;
    // The empty state: no picture in the folder at all. It does not
    // depend on the rotation: the first page of an empty folder has no
    // rotation cell either.
    let empty = view.pictures.is_empty();
    // The header says what is picked now: the rotation, or the file name
    // of the picture. The app passes the name from the settings, so it is
    // right on every page, not only where the picture is shown. The empty
    // state has nothing to say.
    let current = if empty {
        None
    } else {
        Some(if view.rotation_picked {
            tf!(
                Msg::SleepImagesCurrent,
                name = tr(Msg::SleepImagesRotateAll)
            )
        } else if let Some(name) = view.current {
            tf!(Msg::SleepImagesCurrent, name = name)
        } else {
            tf!(
                Msg::SleepImagesCurrent,
                name = tr(Msg::SleepImagesRotateAll)
            )
        })
    };
    let (head, current_label) = header(&mut t, s, current.as_deref());

    if empty {
        let root = empty_state(&mut t, s);
        let col = t.add(Node::column(0).flex(1), [status, head, root]);
        t.set_root(col);
        return (
            t,
            SleepImages {
                grid: root,
                current: current_label.unwrap_or(root),
                pager_label: root,
                prev: root,
                next: root,
                rotate: None,
                cells: Vec::new(),
            },
        );
    }

    // The grid: three columns of cells, the pictures of the page.
    // The line of words stays on one line and is cut off with an
    // ellipsis, like the prototype's `text-overflow`.
    let mut hint_para = Paragraph::plain(
        tr(Msg::SleepImagesHint),
        s.font_px(32).gray(INK_MID).line_height(1.2),
        Align::Left,
    );
    hint_para.style.max_lines = Some(1);
    let hint = t.text(hint_para);

    // The columns are spread over the width of the page, like the
    // prototype's `justify-content: space-between`.
    let available = (width - 2 * home_margin()).max(1);
    let col_gap = ((available - 3 * CELL_W) / 2).max(1);

    let mut cells = Vec::new();
    let mut rotate = None;
    // The cells of the page, three to a row.
    let mut row_cells: Vec<NodeId> = Vec::new();

    if view.rotate_all {
        let picked = view.rotation_picked;
        let picture = t.custom(RotateAll { picked });
        let picture = t.fixed(Some(CELL_W), Some(CELL_H), Some(picture));
        let cell = cell(&mut t, s, picture, tr(Msg::SleepImagesRotateAll), picked);
        let cell = t.add(
            Node::new(Kind::Fixed {
                w: Some(CELL_W),
                h: None,
            })
            .action(action::ROTATE_ALL)
            .feedback(Feedback::Invert),
            [cell],
        );
        rotate = Some(cell);
        row_cells.push(cell);
    }

    for (i, picture) in view.pictures.iter().enumerate() {
        let thumb = t.custom(Thumb {
            image: picture.thumb.clone(),
            picked: picture.picked,
        });
        let thumb = t.fixed(Some(CELL_W), Some(CELL_H), Some(thumb));
        let cell = cell(&mut t, s, thumb, &picture.name, picture.picked);
        let cell = t.add(
            Node::new(Kind::Fixed {
                w: Some(CELL_W),
                h: None,
            })
            .action(action::PICTURE + i as ActionId)
            .feedback(Feedback::Invert),
            [cell],
        );
        cells.push(cell);
        row_cells.push(cell);
    }

    // Three cells to a row, the last row shorter when there are fewer.
    let mut row_nodes = Vec::new();
    for chunk in row_cells.chunks(3) {
        let mut children = Vec::new();
        for (i, c) in chunk.iter().enumerate() {
            if i > 0 {
                children.push(t.gap(col_gap, 1));
            }
            children.push(*c);
        }
        row_nodes.push(t.add(Node::row(0), children));
    }
    let mut grid_children = Vec::new();
    for (i, r) in row_nodes.iter().enumerate() {
        grid_children.push(*r);
        if i + 1 < row_nodes.len() {
            grid_children.push(t.gap(0, 24));
        }
    }
    let grid = t.add(Node::column(0), grid_children);

    // The list of cells scrolls; the pager sits at the bottom, where the
    // library page keeps it.
    let gap = t.gap(0, 12);
    // A padding node lays out its first child only, so the words and the
    // grid go into a column first.
    let content = t.add(Node::column(0), [hint, gap, grid]);
    let list = t.add(
        Node::new(Kind::Padding(Insets::new(
            16,
            home_margin(),
            0,
            home_margin(),
        )))
        .flex(1),
        [content],
    );

    // The pager: the page number in the middle, the arrows at the sides.
    let prev = t.icon(Icon::ChevronLeft, 44, INK);
    let prev = t.button(Node::plain(), prev, action::PREV_PAGE);
    let next = t.icon(Icon::ChevronRight, 44, INK);
    let next = t.button(Node::plain(), next, action::NEXT_PAGE);
    let pager_label = t.text(Paragraph::plain(
        tf!(
            Msg::LibraryPagerPageOf,
            page = view.page + 1,
            count = view.pages
        ),
        s.font_px(36).line_height(1.3),
        Align::Center,
    ));
    let pager_hint = t.text(Paragraph::plain(
        tr(Msg::SleepImagesPagerHint),
        s.font_px(32).gray(INK_MID).line_height(1.3),
        Align::Center,
    ));
    let center = t.add(
        Node::column(0).cross(Cross::Center).flex(1),
        [pager_label, pager_hint],
    );
    let pager_row = t.add(Node::row(0).cross(Cross::Center), [prev, center, next]);
    let pager = t.fixed(None, Some(104), Some(pager_row));
    let pager = t.pad(Insets::new(0, home_margin(), 0, home_margin()), pager);

    let col = t.add(Node::column(0).flex(1), [status, head, list, pager]);
    t.set_root(col);
    (
        t,
        SleepImages {
            grid,
            current: current_label.unwrap_or(pager_label),
            pager_label,
            prev,
            next,
            rotate,
            cells,
        },
    )
}

/// The empty state: the cat, the words and the way to the transfer page.
fn empty_state(t: &mut Tree, s: &Styles) -> NodeId {
    let cat = t.custom(EmptyCat);
    let cat = t.fixed(Some(322), Some(280), Some(cat));

    let title = t.text(Paragraph::plain(
        tr(Msg::SleepImagesEmptyTitle),
        s.display().line_height(1.3),
        Align::Center,
    ));
    let hint1 = t.text(Paragraph::plain(
        tr(Msg::SleepImagesEmptyHint1),
        s.font_px(36).gray(INK_MID).line_height(1.5),
        Align::Center,
    ));
    let hint2 = t.text(Paragraph::plain(
        tr(Msg::SleepImagesEmptyHint2),
        s.font_px(36).gray(INK_MID).line_height(1.5),
        Align::Center,
    ));
    let button_label = t.label(
        tr(Msg::SleepImagesWifiTransfer),
        s.font_px(40).bold().gray(PAPER).line_height(1.2),
    );
    let button_label = t.pad(Insets::new(0, 48, 0, 48), button_label);
    let button = t.add(
        Node::new(Kind::MinSize(Size::new(0, 96)))
            .border(0, INK, 20)
            .bg(INK)
            .action(action::WIFI_TRANSFER)
            .feedback(Feedback::Invert),
        [button_label],
    );
    let usb = t.text(Paragraph::plain(
        tr(Msg::SleepImagesUsbHint),
        s.font_px(32).gray(INK_MID).line_height(1.3),
        Align::Center,
    ));

    let gap_cat = t.gap(0, 40);
    let gap_title = t.gap(0, 20);
    let gap_button = t.gap(0, 40);
    let gap_usb = t.gap(0, 28);
    let col = t.add(
        Node::column(0).cross(Cross::Center),
        [
            cat, gap_cat, title, gap_title, hint1, hint2, gap_button, button, gap_usb, usb,
        ],
    );
    let pad = t.pad(Insets::new(24, home_margin(), 40, home_margin()), col);
    // The block sits in the middle of the free room, like the
    // prototype's empty page.
    t.add(Node::new(Kind::Align { x: 0.5, y: 0.5 }), [pad])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::StatusLine;
    use crate::text::{FamilyId, Fonts, TextEngine};
    use crate::ui::Ui;

    /// The size of the reference screen the prototypes are drawn for.
    const W: i32 = 1264;
    const H: i32 = 1680;

    fn status() -> StatusLine<'static> {
        StatusLine {
            clock: "21:38",
            battery: Some(0.82),
            charging: false,
        }
    }

    fn view<'a>(
        pictures: &'a [Picture],
        rotate_all: bool,
        rotation_picked: bool,
        current: Option<&'a str>,
    ) -> View<'a> {
        View {
            status: status(),
            pictures,
            rotate_all,
            rotation_picked,
            current,
            page: 0,
            pages: 1,
        }
    }

    fn picture(name: &str, picked: bool) -> Picture {
        Picture {
            name: name.to_string(),
            thumb: None,
            picked,
        }
    }

    /// Every text of the tree, in node order.
    fn texts(tree: &Tree) -> Vec<String> {
        (0..tree.len())
            .filter_map(|i| match &tree.node(NodeId(i as u32)).kind {
                Kind::Text { para, .. } => Some(para.text()),
                _ => None,
            })
            .collect()
    }

    /// The first node with this action.
    fn node_with_action(tree: &Tree, action: ActionId) -> Option<NodeId> {
        (0..tree.len())
            .map(|i| NodeId(i as u32))
            .find(|&id| tree.node(id).action == Some(action))
    }

    #[test]
    fn the_grid_shows_the_pictures_and_the_rotation() {
        let s = Styles::new(FamilyId(0));
        let pictures = vec![
            picture("a.jpg", false),
            picture("b.jpg", true),
            picture("c.jpg", false),
        ];
        let (tree, nodes) = build(&s, &view(&pictures, true, true, None), W);
        let mut ui = Ui::new(tree, TextEngine::new(Fonts::embedded()), W as u32, H as u32);
        ui.layout();
        ui.render();
        // The rotation cell, then one cell per picture.
        assert!(nodes.rotate.is_some());
        assert_eq!(nodes.cells.len(), 3);
        // Something is drawn: the title and the hint.
        let ink = (0..H)
            .filter(|&y| (0..W).any(|x| ui.screen().get(x, y) < 128))
            .count();
        assert!(ink > 100, "the page is blank: {ink}");
    }

    #[test]
    fn the_cells_are_of_the_size_of_the_prototype() {
        let s = Styles::new(FamilyId(0));
        let pictures = vec![picture("a.jpg", false), picture("b.jpg", false)];
        let (tree, nodes) = build(&s, &view(&pictures, true, true, None), W);
        let mut ui = Ui::new(tree, TextEngine::new(Fonts::embedded()), W as u32, H as u32);
        ui.layout();
        // The rotation cell is 270 px wide and stands at the left of the
        // page, on the margin.
        let rect = ui.tree.node(nodes.rotate.unwrap()).rect;
        assert_eq!(rect.w, CELL_W);
        assert_eq!(rect.x, home_margin());
        // The picture of the cell is 270 × 360.
        let picture = ui.tree.node(nodes.cells[0]).rect;
        assert_eq!(picture.w, CELL_W);
        // The three columns are spread over the width of the page, like
        // the prototype's `justify-content: space-between`: the rest of
        // the width is left between them.
        let gap = picture.x - (rect.x + rect.w);
        assert_eq!(gap, (W - 2 * home_margin() - 3 * CELL_W) / 2, "{gap}");
    }

    /// R1: the first page of an empty folder has the rotation flag set by
    /// the app, but the page must still show the empty state and the way
    /// to the Wi-Fi transfer page. Before the fix the two conditions were
    /// mutually exclusive, so this path could never be reached.
    #[test]
    fn the_empty_state_shows_the_words_and_the_button() {
        let s = Styles::new(FamilyId(0));
        let (tree, nodes) = build(&s, &view(&[], true, true, None), W);
        assert!(nodes.rotate.is_none());
        assert!(nodes.cells.is_empty());
        assert!(
            node_with_action(&tree, action::WIFI_TRANSFER).is_some(),
            "the empty state has no way to the transfer page"
        );
        assert!(texts(&tree)
            .iter()
            .any(|t| t == tr(Msg::SleepImagesEmptyTitle)));
        let mut ui = Ui::new(tree, TextEngine::new(Fonts::embedded()), W as u32, H as u32);
        ui.layout();
        ui.render();
        let ink = (0..H)
            .filter(|&y| (0..W).any(|x| ui.screen().get(x, y) < 128))
            .count();
        assert!(ink > 100, "the page is blank: {ink}");
    }

    /// R2: the rotation cell stays on the first page when a picture is
    /// picked. Its tick follows the settings, so the reader can go back to
    /// the rotation; before the fix the whole cell disappeared.
    #[test]
    fn a_picked_picture_is_marked_and_the_rotation_stays() {
        let s = Styles::new(FamilyId(0));
        let pictures = vec![picture("a.jpg", false), picture("sunset.jpg", true)];
        let (tree, nodes) = build(&s, &view(&pictures, true, false, Some("sunset.jpg")), W);
        assert!(nodes.rotate.is_some(), "the rotation cell is gone");
        assert_eq!(nodes.cells.len(), 2);
        let mut ui = Ui::new(tree, TextEngine::new(Fonts::embedded()), W as u32, H as u32);
        ui.layout();
        ui.render();
        // The tick of the picked cell: a black disc of 64 px, so a run of
        // dark pixels at least that wide.
        let screen = ui.screen();
        let mut widest = 0;
        for y in 0..H {
            let mut run = 0;
            for x in 0..W {
                if screen.get(x, y) < 40 {
                    run += 1;
                    widest = widest.max(run);
                } else {
                    run = 0;
                }
            }
        }
        assert!(widest >= 60, "the tick is not on the screen: {widest}");
        // The rotation cell itself is not ticked: the middle of its tick
        // area stays paper. Before the fix it was always drawn picked.
        let r = ui.tree.node(nodes.rotate.unwrap()).rect;
        let tick = screen.get(r.x + r.w - 42, r.y + 42 + 20);
        assert!(tick > 200, "the rotation still looks picked: {tick}");
    }

    /// R3: the header says what the settings picked, even when that
    /// picture is on another page; it must not fall back to the rotation
    /// just because the page does not hold the picture.
    #[test]
    fn the_header_names_the_picked_picture_on_another_page() {
        let s = Styles::new(FamilyId(0));
        let pictures = vec![picture("09.jpg", false), picture("10.jpg", false)];
        let mut v = view(&pictures, false, false, Some("00.jpg"));
        v.page = 1;
        v.pages = 2;
        let (tree, _) = build(&s, &v, W);
        let all = texts(&tree);
        assert!(
            all.iter()
                .any(|t| t == &tf!(Msg::SleepImagesCurrent, name = "00.jpg")),
            "the header does not name the picked picture: {all:?}"
        );
        assert!(
            !all.iter().any(|t| t
                == &tf!(
                    Msg::SleepImagesCurrent,
                    name = tr(Msg::SleepImagesRotateAll)
                )),
            "the header says all pictures while one is picked: {all:?}"
        );
    }

    /// R9: the header keeps the page margins, and a file name too long for
    /// the right side is trimmed to one line with an ellipsis instead of
    /// running past the edge.
    #[test]
    fn the_header_keeps_the_margins_and_trims_a_long_name() {
        let s = Styles::new(FamilyId(0));
        let long = "sunset-in-the-mountains-autumn-collection-2024.jpg";
        let pictures = vec![picture(long, true)];
        let (tree, _) = build(&s, &view(&pictures, true, false, Some(long)), W);
        let mut ui = Ui::new(tree, TextEngine::new(Fonts::embedded()), W as u32, H as u32);
        ui.layout();
        let tree = &ui.tree;
        let back = node_with_action(tree, action::BACK).expect("the header has a back button");
        assert_eq!(
            tree.node(back).rect.x,
            home_margin(),
            "the back arrow is off the margin"
        );
        let current = tf!(Msg::SleepImagesCurrent, name = long);
        let label = (0..tree.len())
            .map(|i| NodeId(i as u32))
            .find(|&id| match &tree.node(id).kind {
                Kind::Text { para, .. } => para.text() == current,
                _ => false,
            })
            .expect("the header names the picture");
        assert_eq!(
            match &tree.node(label).kind {
                Kind::Text { para, .. } => para.style.max_lines,
                _ => None,
            },
            Some(1),
            "the current line can wrap instead of being trimmed"
        );
        let rect = tree.node(label).rect;
        assert_eq!(
            rect.right(),
            W - home_margin(),
            "the current line does not end on the right margin: {rect:?}"
        );
    }

    /// R8: the rounded mask must be the same on all four corners. The old
    /// code used the same direction for every centre, so the right corners
    /// were painted with the wrong side of the curve.
    #[test]
    fn the_rounded_corners_are_symmetric() {
        let (w, h) = (CELL_W, CELL_H);
        let mask = round_cover(w, h, CELL_RADIUS);
        for y in 0..h {
            for x in 0..w {
                let here = mask[(y * w + x) as usize];
                assert_eq!(
                    here,
                    mask[(y * w + (w - 1 - x)) as usize],
                    "the mask is not left/right symmetric at ({x}, {y})"
                );
                assert_eq!(
                    here,
                    mask[((h - 1 - y) * w + x) as usize],
                    "the mask is not top/bottom symmetric at ({x}, {y})"
                );
            }
        }
        // The extreme corner is outside the round rectangle; the middle is
        // inside.
        assert_eq!(mask[0], 255);
        assert_eq!(mask[(h / 2 * w + w / 2) as usize], 0);
    }
}
