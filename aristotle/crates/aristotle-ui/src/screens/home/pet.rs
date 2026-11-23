// SPDX-License-Identifier: AGPL-3.0-or-later
//! The desk pet: a cat in a conical hat sits on the reading card with a
//! fishing rod, the line hangs down the right margin, and a fish bites the
//! hook in the navigation bar.
//!
//! The cat and the fish are 1-bit pictures, compiled into the binary as
//! coverage masks so the device draws them without a rasterizer. The line is
//! the only part whose length changes, so it is a plain 2 px bar.
//!
//! The pictures stand where the prototype puts them, in the pixels of the
//! reference screen (1264 x 1680 at 300 DPI). On a narrower screen the
//! offsets are scaled with the content area, which keeps the pet inside the
//! screen; the layout is not the prototype's to the pixel there.

use crate::geom::{Rect, Size};
use crate::theme::INK;
use crate::tree::{Constraints, PaintCx, Widget};

/// The cat, as a coverage mask of 287 x 280 pixels.
pub static CAT_MASK: &[u8] = include_bytes!("../../../assets/cat.mask");
/// The fish, as a coverage mask of 79 x 150 pixels.
static FISH_MASK: &[u8] = include_bytes!("../../../assets/fish.mask");

/// The size of the cat's picture.
pub const CAT: (i32, i32) = (287, 280);
/// The size of the fish's picture.
pub const FISH: (i32, i32) = (79, 150);

/// The width of the screen the prototype was drawn for.
const REF_WIDTH: i32 = 1264;
/// The width of the prototype's content area: 1264 less two 88 px margins.
const REF_CONTENT: i32 = 1088;
/// The width of the fishing line.
const LINE_W: i32 = 2;
/// The left of the cat's picture, in the content area's pixels.
const CAT_LEFT: i32 = 835;
/// The top of the cat's picture, in the content area's pixels.
const CAT_TOP: i32 = 44;
/// The left of the fishing line, in the content area's pixels. It hangs
/// from the tip of the rod, which the picture puts at 285.9 px.
const LINE_LEFT: i32 = 1120;
/// The top of the fishing line, in the content area's pixels: the tip of
/// the rod, 9.5 px below the top of the picture.
const LINE_TOP: i32 = 53;
/// The left of the fish's picture, in the screen's pixels.
const FISH_LEFT: i32 = 1169;
/// The top of the fish's picture, above the top of the navigation bar.
const FISH_TOP: i32 = -2;

/// Where the pieces of the pet stand.
pub struct Geometry {
    /// The cat's picture.
    pub cat: Rect,
    /// The fishing line, from the tip of the rod to the hook.
    pub line: Rect,
    /// The fish's picture.
    pub fish: Rect,
}

impl Geometry {
    /// The geometry for a content area and a navigation bar.
    ///
    /// `content` is the area the clock card and the reading card live in;
    /// `nav_top` is the top of the navigation bar, where the fish bites.
    pub fn new(content: Rect, screen_wide: i32, nav_top: i32) -> Self {
        // The offsets are the prototype's, scaled with the content area so
        // the pet stays on the screen when it is narrower.
        let k = content.w as f32 / REF_CONTENT as f32;
        let x = |v: i32| content.x + (v as f32 * k).round() as i32;
        let cat = Rect::new(x(CAT_LEFT), content.y + CAT_TOP, CAT.0, CAT.1);
        let line_top = content.y + LINE_TOP;
        let line = Rect::new(x(LINE_LEFT), line_top, LINE_W, (nav_top - line_top).max(0));
        let fish = Rect::new(
            (FISH_LEFT as f32 * screen_wide as f32 / REF_WIDTH as f32).round() as i32,
            nav_top + FISH_TOP,
            FISH.0,
            FISH.1,
        );
        Geometry { cat, line, fish }
    }

    /// Places and scales the pet to fit `card` cleanly on compact six-inch
    /// and low-DPI panels without overlapping the resume button, while keeping
    /// the exact prototype coordinates on seven-inch 300 DPI panels.
    pub fn for_card(
        content: Rect,
        card: Rect,
        screen_wide: i32,
        nav_top: i32,
        density: crate::geom::Density,
    ) -> Self {
        if density.dpi() == 300 && card.h >= 400 {
            return Self::new(content, screen_wide, nav_top);
        }
        let dpi_scale = density.dpi() as f32 / 300.0;
        let max_cat_h = (card.h as f32 * 0.58).max(32.0);
        let scale = (max_cat_h / CAT.1 as f32).min(dpi_scale);
        let cat_w = (CAT.0 as f32 * scale).round().max(1.0) as i32;
        let cat_h = (CAT.1 as f32 * scale).round().max(1.0) as i32;
        let fish_w = (FISH.0 as f32 * scale).round().max(1.0) as i32;
        let fish_h = (FISH.1 as f32 * scale).round().max(1.0) as i32;
        let line_w = (LINE_W as f32 * dpi_scale).round().max(1.0) as i32;
        // Put the rod tip in the right margin without clipping the cat off-screen.
        let rod_x =
            (content.right() + (screen_wide - content.right()) / 3).min(screen_wide - line_w - 2);
        let cat_x = (rod_x - cat_w).max(card.x);
        let cat_y = card.y + (card.h as f32 * 0.05).round() as i32;
        let line_top = cat_y + (9.5 * scale).round() as i32;
        let hook_x = (40.2 * scale).round() as i32;
        let fish_x = (rod_x - hook_x).clamp(0, (screen_wide - fish_w).max(0));
        let fish_y = nav_top + (FISH_TOP as f32 * scale).round() as i32;
        Geometry {
            cat: Rect::new(cat_x, cat_y, cat_w, cat_h),
            line: Rect::new(rod_x, line_top, line_w, (nav_top - line_top).max(0)),
            fish: Rect::new(fish_x, fish_y, fish_w, fish_h),
        }
    }
}

/// The desk pet, drawn over the home screen.
pub struct Pet {
    /// Where the pieces stand, once the screen has been laid out.
    pub geometry: Geometry,
    /// False when the setting turns the pet off.
    pub active: bool,
}

impl Pet {
    /// A pet with no place yet; the screen fills it in after the layout.
    pub fn new() -> Self {
        Pet {
            geometry: Geometry {
                cat: Rect::default(),
                line: Rect::default(),
                fish: Rect::default(),
            },
            active: true,
        }
    }
}

impl Default for Pet {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Pet {
    fn measure(&mut self, c: Constraints, _text: &mut crate::text::TextEngine) -> Size {
        c.constrain(c.max)
    }

    fn paint(&self, _rect: Rect, cx: &mut PaintCx) {
        if !self.active {
            return;
        }
        let p = &mut *cx.painter;
        // The line first, the cat over it: the rod hides the top of the
        // line, the way the prototype stacks them.
        if self.geometry.line.h > 0 {
            p.fill_rect(self.geometry.line, INK);
        }
        if !self.geometry.cat.is_empty() {
            if self.geometry.cat.w == CAT.0 && self.geometry.cat.h == CAT.1 {
                p.blend_mask(
                    self.geometry.cat.x,
                    self.geometry.cat.y,
                    CAT.0 as u32,
                    CAT.1 as u32,
                    CAT_MASK,
                    INK,
                );
            } else {
                let scaled = crate::status::scale_mask(
                    CAT_MASK,
                    CAT.0 as usize,
                    CAT.1 as usize,
                    self.geometry.cat.w as usize,
                    self.geometry.cat.h as usize,
                );
                p.blend_mask(
                    self.geometry.cat.x,
                    self.geometry.cat.y,
                    self.geometry.cat.w as u32,
                    self.geometry.cat.h as u32,
                    &scaled,
                    INK,
                );
            }
        }
        if !self.geometry.fish.is_empty() {
            if self.geometry.fish.w == FISH.0 && self.geometry.fish.h == FISH.1 {
                p.blend_mask(
                    self.geometry.fish.x,
                    self.geometry.fish.y,
                    FISH.0 as u32,
                    FISH.1 as u32,
                    FISH_MASK,
                    INK,
                );
            } else {
                let scaled = crate::status::scale_mask(
                    FISH_MASK,
                    FISH.0 as usize,
                    FISH.1 as usize,
                    self.geometry.fish.w as usize,
                    self.geometry.fish.h as usize,
                );
                p.blend_mask(
                    self.geometry.fish.x,
                    self.geometry.fish.y,
                    self.geometry.fish.w as u32,
                    self.geometry.fish.h as u32,
                    &scaled,
                    INK,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference screen: 1264 px wide, the content area between the
    /// 88 px margins, the navigation bar 152 px tall.
    const W: i32 = 1264;
    const H: i32 = 1680;
    const MARGIN: i32 = 88;
    const NAV: i32 = 152;

    fn geometry() -> Geometry {
        // The content area starts below the 88 px status bar and the 28 px
        // of padding, and the clock card is 194 px tall.
        let content = Rect::new(MARGIN, 88 + 28 + 194, W - 2 * MARGIN, 0);
        Geometry::new(content, W, H - NAV)
    }

    #[test]
    fn the_cat_sits_where_the_prototype_puts_it() {
        let g = geometry();
        assert_eq!(g.cat, Rect::new(923, 354, 287, 280));
    }

    #[test]
    fn the_line_hangs_from_the_tip_of_the_rod_to_the_hook() {
        let g = geometry();
        assert_eq!(g.line.x, 1208);
        assert_eq!(g.line.y, 363);
        assert_eq!(g.line.w, 2);
        assert_eq!(g.line.bottom(), 1528);
    }

    #[test]
    fn the_fish_bites_the_hook_at_the_top_of_the_navigation_bar() {
        let g = geometry();
        assert_eq!(g.fish, Rect::new(1169, 1526, 79, 150));
    }

    #[test]
    fn the_line_meets_the_hook_of_the_fish() {
        let g = geometry();
        // The hook is 40.2 px into the fish's picture.
        let hook = g.fish.x + 40;
        assert!(g.line.x <= hook && hook < g.line.right());
    }

    #[test]
    fn the_pet_stays_on_a_narrower_screen() {
        // The Paperwhite 4: 1072 px wide.
        let margin = 71;
        let content = Rect::new(margin, 116, 1072 - 2 * margin, 0);
        let g = Geometry::new(content, 1072, 1448 - NAV);
        assert!(g.cat.x >= 0 && g.cat.right() <= 1072);
        assert!(g.line.x < 1072);
        assert!(g.fish.right() <= 1072);
    }
}
