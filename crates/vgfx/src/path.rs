//! Paths in device pixels (y down) for the shapes of box painting:
//! rectangles, rectangles with elliptical corners and ellipses, all as
//! [`Outline`]s for [`crate::raster::fill`]. Quarter ellipses are cubic
//! Béziers with the usual control distance `κ = 4(√2 − 1)/3` of the radius.

use crate::outline::Outline;

/// The control point distance of a quarter circle of radius 1.
const KAPPA: f32 = 0.552_284_8;

/// The radii of the four corners in the order top left, top right,
/// bottom right, bottom left, each `(horizontal, vertical)`.
pub type Radii = [(f32, f32); 4];

/// True if every corner is square.
pub fn is_square(r: &Radii) -> bool {
    r.iter().all(|&(x, y)| x <= 0.0 || y <= 0.0)
}

/// A clockwise rectangle.
pub fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Outline {
    let mut o = Outline::new();
    push_rect(&mut o, x0, y0, x1, y1, false);
    o
}

/// Appends a rectangle, counterclockwise if `reverse`.
pub fn push_rect(o: &mut Outline, x0: f32, y0: f32, x1: f32, y1: f32, reverse: bool) {
    o.move_to(x0, y0);
    if reverse {
        o.line_to(x0, y1);
        o.line_to(x1, y1);
        o.line_to(x1, y0);
    } else {
        o.line_to(x1, y0);
        o.line_to(x1, y1);
        o.line_to(x0, y1);
    }
    o.close();
}

/// A clockwise rectangle with elliptical corners (the radii must
/// already fit, see [`fit_radii`]).
pub fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: &Radii) -> Outline {
    let mut o = Outline::new();
    push_rounded_rect(&mut o, x0, y0, x1, y1, r);
    o
}

/// Appends a clockwise rectangle with elliptical corners.
pub fn push_rounded_rect(o: &mut Outline, x0: f32, y0: f32, x1: f32, y1: f32, r: &Radii) {
    if is_square(r) {
        push_rect(o, x0, y0, x1, y1, false);
        return;
    }
    let pos = |v: (f32, f32)| {
        if v.0 > 0.0 && v.1 > 0.0 {
            v
        } else {
            (0.0, 0.0)
        }
    };
    let [tl, tr, br, bl] = [pos(r[0]), pos(r[1]), pos(r[2]), pos(r[3])];
    o.move_to(x0 + tl.0, y0);
    o.line_to(x1 - tr.0, y0);
    if tr.0 > 0.0 {
        o.cubic_to(
            x1 - tr.0 + tr.0 * KAPPA,
            y0,
            x1,
            y0 + tr.1 - tr.1 * KAPPA,
            x1,
            y0 + tr.1,
        );
    }
    o.line_to(x1, y1 - br.1);
    if br.0 > 0.0 {
        o.cubic_to(
            x1,
            y1 - br.1 + br.1 * KAPPA,
            x1 - br.0 + br.0 * KAPPA,
            y1,
            x1 - br.0,
            y1,
        );
    }
    o.line_to(x0 + bl.0, y1);
    if bl.0 > 0.0 {
        o.cubic_to(
            x0 + bl.0 - bl.0 * KAPPA,
            y1,
            x0,
            y1 - bl.1 + bl.1 * KAPPA,
            x0,
            y1 - bl.1,
        );
    }
    o.line_to(x0, y0 + tl.1);
    if tl.0 > 0.0 {
        o.cubic_to(
            x0,
            y0 + tl.1 - tl.1 * KAPPA,
            x0 + tl.0 - tl.0 * KAPPA,
            y0,
            x0 + tl.0,
            y0,
        );
    }
    o.close();
}

/// A clockwise ellipse centered at (`cx`, `cy`).
pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Outline {
    let mut o = Outline::new();
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    o.move_to(cx, cy - ry);
    o.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
    o.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
    o.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
    o.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
    o.close();
    o
}

/// A closed polygon through `points`.
pub fn polygon(points: &[(f32, f32)]) -> Outline {
    let mut o = Outline::new();
    if let Some(&(x, y)) = points.first() {
        o.move_to(x, y);
        for &(x, y) in &points[1..] {
            o.line_to(x, y);
        }
        o.close();
    }
    o
}

/// Scales all radii down by the same factor so that the radii of
/// adjacent corners do not overlap on a box of `width` × `height`
/// (css-backgrounds-3 §5.5, "corner overlap").
pub fn fit_radii(r: &Radii, width: f32, height: f32) -> Radii {
    let mut f: f32 = 1.0;
    let mut check = |sum: f32, side: f32| {
        if sum > 0.0 && sum > side {
            f = f.min(side / sum);
        }
    };
    check(r[0].0 + r[1].0, width);
    check(r[3].0 + r[2].0, width);
    check(r[0].1 + r[3].1, height);
    check(r[1].1 + r[2].1, height);
    r.map(|(x, y)| ((x * f).max(0.0), (y * f).max(0.0)))
}

/// The radii of a curve `d` inside (positive) or outside (negative) of
/// one with radii `r`, with per-side distances (top, right, bottom,
/// left): each radius shrinks by the adjacent side's distance, down to
/// zero (css-backgrounds-3 §5.2, the padding and content edge curves).
pub fn inset_radii(r: &Radii, (top, right, bottom, left): (f32, f32, f32, f32)) -> Radii {
    let sub = |(x, y): (f32, f32), dx: f32, dy: f32| ((x - dx).max(0.0), (y - dy).max(0.0));
    [
        sub(r[0], left, top),
        sub(r[1], right, top),
        sub(r[2], right, bottom),
        sub(r[3], left, bottom),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::{FillRule, fill};

    #[test]
    fn radii_scale_together_when_they_overlap() {
        let r = fit_radii(
            &[(60.0, 10.0), (60.0, 10.0), (0.0, 0.0), (0.0, 0.0)],
            100.0,
            50.0,
        );
        assert!((r[0].0 - 50.0).abs() < 1e-4 && (r[0].1 - 50.0 / 6.0).abs() < 1e-4);
    }

    #[test]
    fn rounded_corner_leaves_the_corner_pixel_empty() {
        let r = [(10.0, 10.0); 4];
        let m = fill(
            &rounded_rect(0.0, 0.0, 20.0, 20.0, &r),
            FillRule::NonZero,
            (0, 0, 20, 20),
        );
        assert_eq!(m.get(0, 0), 0);
        assert_eq!(m.get(10, 10), 255);
        assert_eq!(m.get(10, 1), 255);
    }
}
