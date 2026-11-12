//! Axis-aligned pixel rectangles and dirty-region merging.

/// An axis-aligned rectangle in pixel coordinates.
///
/// The origin is the top-left corner and the right/bottom edges are
/// exclusive. A rectangle with a zero side is *empty*: it covers no pixel.
///
/// Coordinates are signed so that a rectangle can express an area that starts
/// outside the frame buffer; the frame buffer clips it instead of panicking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// Left edge, inclusive.
    pub x: i32,
    /// Top edge, inclusive.
    pub y: i32,
    /// Width in pixels; a negative or zero width makes the rectangle empty.
    pub width: i32,
    /// Height in pixels; a negative or zero height makes the rectangle empty.
    pub height: i32,
}

impl Rect {
    /// Create a rectangle. Negative `width`/`height` are clamped to zero.
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width: if width < 0 { 0 } else { width },
            height: if height < 0 { 0 } else { height },
        }
    }

    /// Create a rectangle anchored at the origin.
    pub const fn from_size(width: i32, height: i32) -> Self {
        Self::new(0, 0, width, height)
    }

    /// Whether the rectangle covers no pixel.
    ///
    /// A negative width or height also covers no pixel: the fields are public,
    /// so callers can bypass the clamping done by [`Rect::new`].
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Exclusive right edge.
    pub const fn right(&self) -> i32 {
        self.x.saturating_add(self.width)
    }

    /// Exclusive bottom edge.
    pub const fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height)
    }

    /// Exclusive right edge as `i64`, so extreme coordinates do not saturate.
    const fn right_i64(&self) -> i64 {
        self.x as i64 + self.width as i64
    }

    /// Exclusive bottom edge as `i64`, so extreme coordinates do not saturate.
    const fn bottom_i64(&self) -> i64 {
        self.y as i64 + self.height as i64
    }

    /// Whether the two rectangles share at least one pixel.
    pub fn intersects(&self, other: &Rect) -> bool {
        !self.intersection(other).is_empty()
    }

    /// The overlapping area of the two rectangles, empty when they are apart.
    pub fn intersection(&self, other: &Rect) -> Rect {
        let x0 = self.x.max(other.x) as i64;
        let y0 = self.y.max(other.y) as i64;
        let x1 = self.right_i64().min(other.right_i64());
        let y1 = self.bottom_i64().min(other.bottom_i64());
        if x1 <= x0 || y1 <= y0 {
            return Rect::default();
        }
        Rect::new(
            x0 as i32,
            y0 as i32,
            (x1 - x0).min(i32::MAX as i64) as i32,
            (y1 - y0).min(i32::MAX as i64) as i32,
        )
    }

    /// Whether `other` lies entirely inside this rectangle.
    ///
    /// An empty rectangle is never contained.
    pub fn contains(&self, other: &Rect) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        other.x >= self.x
            && other.y >= self.y
            && other.right_i64() <= self.right_i64()
            && other.bottom_i64() <= self.bottom_i64()
    }

    /// The smallest rectangle covering both operands.
    ///
    /// An empty operand is ignored, so the union with an empty rectangle is
    /// the other rectangle itself.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x0 = self.x.min(other.x) as i64;
        let y0 = self.y.min(other.y) as i64;
        let x1 = (self.right() as i64).max(other.right() as i64);
        let y1 = (self.bottom() as i64).max(other.bottom() as i64);
        Rect::new(
            x0 as i32,
            y0 as i32,
            (x1 - x0).min(i32::MAX as i64) as i32,
            (y1 - y0).min(i32::MAX as i64) as i32,
        )
    }
}

/// Merge dirty rectangles into the single rectangle covering all of them.
///
/// Empty rectangles are skipped. Returns `None` when nothing is left to
/// merge, i.e. the input is empty or holds only empty rectangles.
pub fn merge_rects(rects: &[Rect]) -> Option<Rect> {
    let mut merged: Option<Rect> = None;
    for rect in rects {
        if rect.is_empty() {
            continue;
        }
        merged = Some(match merged {
            Some(current) => current.union(rect),
            None => *rect,
        });
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_size_is_clamped_to_empty() {
        let r = Rect::new(4, 4, -10, 10);
        assert!(r.is_empty());
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 10);
    }

    #[test]
    fn edges_are_exclusive() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!(r.right(), 40);
        assert_eq!(r.bottom(), 60);
    }

    #[test]
    fn merge_of_intersecting_rects_is_their_bounding_box() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        let merged = merge_rects(&[a, b]).expect("non-empty input merges");
        assert_eq!(merged, Rect::new(0, 0, 15, 15));
        assert!(merged.contains(&a));
        assert!(merged.contains(&b));
    }

    #[test]
    fn merge_of_disjoint_rects_still_covers_both() {
        let a = Rect::new(0, 0, 4, 4);
        let b = Rect::new(20, 30, 5, 5);
        let merged = merge_rects(&[a, b]).expect("non-empty input merges");
        assert_eq!(merged, Rect::new(0, 0, 25, 35));
        assert!(merged.contains(&a));
        assert!(merged.contains(&b));
    }

    #[test]
    fn merge_of_contained_rects_keeps_the_outer_one() {
        let outer = Rect::new(2, 3, 40, 50);
        let inner = Rect::new(10, 12, 5, 6);
        let merged = merge_rects(&[outer, inner]).expect("non-empty input merges");
        assert_eq!(merged, outer);
        assert_eq!(
            merge_rects(&[inner, outer]).expect("non-empty input merges"),
            outer
        );
    }

    #[test]
    fn merge_of_an_empty_set_is_none() {
        assert_eq!(merge_rects(&[]), None);
        assert_eq!(merge_rects(&[Rect::default()]), None);
        assert_eq!(
            merge_rects(&[Rect::new(1, 1, 0, 4), Rect::new(0, 0, 4, 0)]),
            None
        );
    }

    #[test]
    fn merge_skips_empty_rects() {
        let merged = merge_rects(&[Rect::default(), Rect::new(3, 4, 5, 6)]).expect("merges");
        assert_eq!(merged, Rect::new(3, 4, 5, 6));
    }

    #[test]
    fn merge_of_a_single_rect_is_that_rect() {
        let r = Rect::new(7, 8, 9, 10);
        assert_eq!(merge_rects(&[r]).expect("merges"), r);
    }

    #[test]
    fn intersection_and_containment() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        let far = Rect::new(50, 50, 10, 10);
        assert_eq!(a.intersection(&b), Rect::new(5, 5, 5, 5));
        assert!(a.intersects(&b));
        assert!(!a.intersects(&far));
        assert!(a.intersection(&far).is_empty());
        assert!(a.contains(&Rect::new(1, 1, 2, 2)));
        assert!(!a.contains(&b));
        assert!(!a.contains(&Rect::default()));
    }
}
