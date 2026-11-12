// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synthetic bold: fattening a glyph that has no bold face of its own.
//!
//! The Chinese fallback carries only a regular face, so a bold Chinese run
//! is fattened by pushing the outline outward. The push follows FreeType's
//! `FT_Outline_EmboldenXY` (the same algorithm `swash` uses), with one
//! change: the way the glyph turns is read off the contour that encloses
//! the most, instead of off the sum of the areas of all of them.
//!
//! `swash` reads the direction off the signed area of all the points of the
//! glyph taken as a single loop. For a glyph whose strokes balance out,
//! that sum is next to nothing and its sign is arbitrary. The direction
//! then comes out reversed, the push goes *inwards*, and the glyph gets
//! thinner instead of fatter: 「消」 bolded the old way lost 72% of its ink
//! while 「取」 next to it gained 66%.
//!
//! A font keeps to one convention, outer contours one way and the holes in
//! them the other, so the direction belongs to the glyph as a whole. The
//! contour that encloses the most is always an outer one, and its turn
//! gives that direction; every contour is then pushed the same way, which
//! grows the outer shape and closes the holes at the same time.

use swash::zeno::{Point, Verb};

/// Pushes every contour of an outline outward by `strength` pixels.
///
/// `points` are the outline's points and `verbs` its drawing commands; the
/// points of one contour are the ones between a `MoveTo` and the `Close`
/// that ends it. `strength` is how far the outline moves out, so the
/// strokes grow by that much on each side.
pub fn offset(points: &mut [Point], verbs: &[Verb], strength: f32) {
    let contours = contours_of(verbs, points.len());
    // The way the glyph turns, read off the contour that encloses the most.
    let mut largest = 0f64;
    for c in &contours {
        let area = area(&points[c.clone()]);
        if area.abs() > largest.abs() {
            largest = area;
        }
    }
    let winding = if largest > 0.0 { 1 } else { 0 };
    for c in contours {
        offset_contour(&mut points[c], winding, strength);
    }
}

/// The points of each contour, in the order the outline draws them.
fn contours_of(verbs: &[Verb], len: usize) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut pos = 0usize;
    for verb in verbs {
        match verb {
            Verb::MoveTo | Verb::Close => {
                if pos > start && start < len {
                    out.push(start..pos.min(len));
                }
                start = pos;
                if *verb == Verb::MoveTo {
                    pos += 1;
                }
            }
            Verb::LineTo => pos += 1,
            Verb::QuadTo => pos += 2,
            Verb::CurveTo => pos += 3,
        }
    }
    if pos > start && start < len {
        out.push(start..pos.min(len));
    }
    out
}

/// The area a contour encloses, signed: positive when it turns one way,
/// negative when it turns the other.
fn area(points: &[Point]) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let last = points.len() - 1;
    let mut area = 0f64;
    let mut prev = points[last];
    for cur in points {
        area += (cur.y - prev.y) as f64 * (cur.x + prev.x) as f64;
        prev = *cur;
    }
    area
}

/// Pushes one contour outward.
///
/// The points are moved along the average of the two lines that meet there,
/// so a corner moves out on the diagonal and a straight run moves out to
/// the side. Where the turn is too sharp for the offset to fit, the push is
/// shortened so the outline does not cross itself.
fn offset_contour(points: &mut [Point], winding: u8, strength: f32) {
    if points.is_empty() {
        return;
    }
    let last = points.len() - 1;
    // The point the walk is at, the point it is heading for, and the one
    // the walk came back from.
    let mut i = last;
    let mut j = 0;
    let mut k = !0;
    let mut out_len;
    // The direction of the line the walk arrived on, and its length.
    let mut in_len = 0.0f32;
    // The same for the line the walk left on.
    let mut anchor_len = 0.0f32;
    let mut anchor = Point::ZERO;
    let mut out;
    let mut in_ = Point::ZERO;
    while j != i && i != k {
        // The line from the point at `i` to the one at `j`.
        if j != k {
            out = points[j] - points[i];
            out_len = out.length();
            if out_len == 0.0 {
                j = if j < last { j + 1 } else { 0 };
                continue;
            } else {
                let s = 1.0 / out_len;
                out.x *= s;
                out.y *= s;
            }
        } else {
            // The walk has come round: the line it started on is the one it
            // ends on.
            out = anchor;
            out_len = anchor_len;
        }
        if in_len != 0.0 {
            if k == !0 {
                k = i;
                anchor = in_;
                anchor_len = in_len;
            }
            // The cosine of the turn at this point.
            let mut d = (in_.x * out.x) + (in_.y * out.y);
            let shift = if d > -0.9396 {
                d += 1.0;
                // The direction out of the corner, perpendicular to the
                // lines that meet there.
                let mut sx = in_.y + out.y;
                let mut sy = in_.x + out.x;
                if winding == 0 {
                    sx = -sx;
                } else {
                    sy = -sy;
                }
                let mut q = (out.x * in_.y) - (out.y * in_.x);
                if winding == 0 {
                    q = -q;
                }
                let l = in_len.min(out_len);
                if q != 0.0 {
                    if strength * q <= l * d {
                        sx = sx * strength / d;
                        sy = sy * strength / d;
                    } else {
                        sx = sx * l / q;
                        sy = sy * l / q;
                    }
                } else {
                    sx = 0.0;
                    sy = 0.0;
                }
                Point::new(sx, sy)
            } else {
                Point::ZERO
            };

            // The points between `i` and `j` share the corner's direction.
            while i != j {
                points[i].x += strength + shift.x;
                points[i].y += strength + shift.y;
                i = if i < last { i + 1 } else { 0 };
            }
        } else {
            i = j;
        }
        in_ = out;
        in_len = out_len;
        j = if j < last { j + 1 } else { 0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corners of a square, clockwise.
    fn square() -> [Point; 4] {
        [
            Point::new(0.0, 0.0),
            Point::new(0.0, 10.0),
            Point::new(10.0, 10.0),
            Point::new(10.0, 0.0),
        ]
    }

    /// The corners of a square, counter-clockwise.
    fn square_back() -> [Point; 4] {
        [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ]
    }

    /// The verbs of a contour of `n` points.
    fn verbs(n: usize) -> Vec<Verb> {
        let mut v = vec![Verb::MoveTo];
        for _ in 1..n {
            v.push(Verb::LineTo);
        }
        v.push(Verb::Close);
        v
    }

    #[test]
    fn a_contour_grows_on_every_side() {
        let mut points = square();
        let before = area(&points).abs();
        offset(&mut points, &verbs(4), 1.0);
        // The square took a half a pixel more on each side.
        assert!(area(&points).abs() > before * 1.2, "{:?}", points);
        assert!((points[1].y - points[0].y).abs() > 10.0, "{:?}", points);
    }

    #[test]
    fn a_contour_the_other_way_round_grows_too() {
        let mut points = square_back();
        let before = area(&points).abs();
        offset(&mut points, &verbs(4), 1.0);
        assert!(area(&points).abs() > before * 1.2, "{:?}", points);
        assert!((points[1].x - points[0].x).abs() > 10.0, "{:?}", points);
    }

    #[test]
    fn the_hole_in_a_glyph_closes_as_the_outline_grows() {
        // An outer square with a square hole in it, the hole turning the
        // way the font does: the outer one clockwise, the hole the other.
        let mut points = vec![
            Point::new(0.0, 0.0),
            Point::new(0.0, 10.0),
            Point::new(10.0, 10.0),
            Point::new(10.0, 0.0),
            Point::new(3.0, 3.0),
            Point::new(7.0, 3.0),
            Point::new(7.0, 7.0),
            Point::new(3.0, 7.0),
        ];
        let mut v = verbs(4);
        v.extend(verbs(4));
        let outer = area(&points[0..4]).abs();
        let hole = area(&points[4..8]).abs();
        offset(&mut points, &v, 1.0);
        // The outer square grew...
        assert!(area(&points[0..4]).abs() > outer * 1.2, "{:?}", points);
        // ...and the hole in it got smaller.
        assert!(area(&points[4..8]).abs() < hole * 0.8, "{:?}", points);
    }

    #[test]
    fn the_direction_comes_from_the_contour_that_encloses_the_most() {
        // A big square turning one way, and a lot of small ones turning the
        // other way. The areas of the small ones add up to more than the
        // big one, so the sum of all the areas says the opposite of the
        // contour that encloses the most. The big one has the say.
        let mut points = vec![
            Point::new(0.0, 0.0),
            Point::new(0.0, 10.0),
            Point::new(10.0, 10.0),
            Point::new(10.0, 0.0),
        ];
        let mut v = verbs(4);
        for i in 0..13 {
            let x = 20.0 + (i % 5) as f32 * 6.0;
            let y = 20.0 + (i / 5) as f32 * 6.0;
            points.extend([
                Point::new(x, y),
                Point::new(x + 4.0, y),
                Point::new(x + 4.0, y + 4.0),
                Point::new(x, y + 4.0),
            ]);
            v.extend(verbs(4));
        }
        offset(&mut points, &v, 1.0);
        // The big square grew the way a clockwise contour grows.
        assert!((points[3].x - 12.0).abs() < 0.01, "{:?}", points);
        assert!((points[1].y - 12.0).abs() < 0.01, "{:?}", points);
    }
}
