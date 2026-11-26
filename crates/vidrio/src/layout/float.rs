//! The floats of one block formatting context (CSS 2 §9.5): their margin
//! boxes in the coordinates of the formatting context root, the placement
//! rules of §9.5.1, the space they leave to line boxes and to boxes that
//! establish formatting contexts, and the clearance of §9.5.2.

use super::values::Float;

const EPS: f64 = 1e-7;

#[derive(Clone, Copy, Debug)]
struct Placed {
    side: Float,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

/// The floats placed so far in a block formatting context.
#[derive(Clone, Debug, Default)]
pub(crate) struct FloatContext {
    floats: Vec<Placed>,
    /// The highest outer top a later float may have (rule 5: not above
    /// any earlier float), and the bottom of the latest float (rule 6 for
    /// the later float of each side is the same constraint).
    min_top: f64,
}

impl FloatContext {
    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    /// The free interval `[left, right]` between the floats that
    /// intersect the band `[top, top + height)`, within `[cb_left,
    /// cb_right]`. A zero height band still sees the floats at `top`.
    pub fn available(&self, top: f64, height: f64, cb_left: f64, cb_right: f64) -> (f64, f64) {
        let bottom = top + height.max(0.0);
        let mut left = cb_left;
        let mut right = cb_right;
        for f in &self.floats {
            let hit = if height > 0.0 {
                f.top < bottom - EPS && f.bottom > top + EPS
            } else {
                f.top <= top + EPS && f.bottom > top + EPS
            };
            if !hit {
                continue;
            }
            match f.side {
                Float::Left => left = left.max(f.right),
                Float::Right => right = right.min(f.left),
                Float::None => {}
            }
        }
        (left, right)
    }

    /// True if some float intersects the band.
    fn any_in(&self, top: f64, height: f64) -> bool {
        let bottom = top + height.max(0.0);
        self.floats.iter().any(|f| {
            if height > 0.0 {
                f.top < bottom - EPS && f.bottom > top + EPS
            } else {
                f.top <= top + EPS && f.bottom > top + EPS
            }
        })
    }

    /// The lowest float bottom below `y` among the floats that intersect
    /// the band (where to try next).
    fn next_bottom(&self, top: f64, height: f64) -> Option<f64> {
        let bottom = top + height.max(0.0);
        self.floats
            .iter()
            .filter(|f| {
                if height > 0.0 {
                    f.top < bottom - EPS && f.bottom > top + EPS
                } else {
                    f.top <= top + EPS && f.bottom > top + EPS
                }
            })
            .map(|f| f.bottom)
            .fold(None, |acc: Option<f64>, b| {
                Some(acc.map_or(b, |a| a.min(b)))
            })
    }

    /// The first position at or below `y` where a box of `width` and
    /// `height` fits between the floats within `[cb_left, cb_right]`:
    /// (y, left edge, right edge of the free interval). A box wider than
    /// any free space goes below every float in its way.
    pub fn find_space(
        &self,
        y: f64,
        width: f64,
        height: f64,
        cb_left: f64,
        cb_right: f64,
    ) -> (f64, f64, f64) {
        let mut y = y;
        for _ in 0..=self.floats.len() {
            let (l, r) = self.available(y, height, cb_left, cb_right);
            if r - l + EPS >= width || !self.any_in(y, height) {
                return (y, l, r);
            }
            match self.next_bottom(y, height) {
                Some(b) if b > y => y = b,
                _ => return (y, l, r),
            }
        }
        let (l, r) = self.available(y, height, cb_left, cb_right);
        (y, l, r)
    }

    /// Places a float of margin box `width` × `height` no higher than
    /// `y` inside `[cb_left, cb_right]` (rules 1 to 9 of §9.5.1); returns
    /// its margin box's top left corner.
    pub fn place(
        &mut self,
        side: Float,
        width: f64,
        height: f64,
        y: f64,
        cb_left: f64,
        cb_right: f64,
    ) -> (f64, f64) {
        let y = y.max(self.min_top);
        let (top, l, r) = self.find_space(y, width, height, cb_left, cb_right);
        let left = match side {
            Float::Right => r - width,
            _ => l,
        };
        self.floats.push(Placed {
            side,
            left,
            right: left + width,
            top,
            bottom: top + height,
        });
        self.min_top = self.min_top.max(top);
        (left, top)
    }

    /// Records a float placed elsewhere (a float of an inline formatting
    /// context placed after its line).
    pub fn raise_min_top(&mut self, y: f64) {
        self.min_top = self.min_top.max(y);
    }

    /// The bottom of the lowest float on the cleared sides.
    pub fn clearance_y(&self, left: bool, right: bool) -> Option<f64> {
        self.floats
            .iter()
            .filter(|f| (left && f.side == Float::Left) || (right && f.side == Float::Right))
            .map(|f| f.bottom)
            .fold(None, |acc: Option<f64>, b| {
                Some(acc.map_or(b, |a| a.max(b)))
            })
    }

    /// The bottom of the lowest float (a formatting context root's auto
    /// height includes its floats, §10.6.7).
    pub fn bottom(&self) -> Option<f64> {
        self.clearance_y(true, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_stack_and_wrap() {
        let mut f = FloatContext::default();
        assert_eq!(
            f.place(Float::Left, 50.0, 20.0, 0.0, 0.0, 100.0),
            (0.0, 0.0)
        );
        assert_eq!(
            f.place(Float::Left, 40.0, 10.0, 0.0, 0.0, 100.0),
            (50.0, 0.0)
        );
        // No room beside them: below the shorter one.
        assert_eq!(
            f.place(Float::Left, 30.0, 10.0, 0.0, 0.0, 100.0),
            (50.0, 10.0)
        );
        // Not above an earlier float (rule 5).
        assert_eq!(
            f.place(Float::Right, 20.0, 5.0, 0.0, 0.0, 100.0),
            (80.0, 10.0)
        );
        assert_eq!(f.available(0.0, 1.0, 0.0, 100.0), (90.0, 100.0));
        assert_eq!(f.available(12.0, 1.0, 0.0, 100.0), (80.0, 80.0));
        assert_eq!(f.clearance_y(true, false), Some(20.0));
        assert_eq!(f.clearance_y(false, true), Some(15.0));
    }
}
