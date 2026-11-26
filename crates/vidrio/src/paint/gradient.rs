//! Gradients (css-images-3 §3, css-images-4 §3): linear, radial and
//! conic gradients and their `repeating-` forms resolved for one tile
//! size, with the color stop fixup, double-position stops, color hints
//! and color interpolation in the gradient's `<color-interpolation-method>`
//! (by default sRGB when every stop is a legacy color, Oklab otherwise),
//! premultiplied. Each segment between two stops is tabulated in 256
//! steps, so hard stops stay exact.

use std::f64::consts::PI;

use vgfx::image::Image;

use super::color;
use crate::css::style::ComputedStyle;
use crate::css::values::Value;
use crate::css::values::color::{Absolute, HueMethod, Space, interpolate};
use crate::css::values::image::{Direction, Image as CssGradient, Kind, Stop};
use crate::layout::values::value_lp;

const STEPS: usize = 256;

#[derive(Clone, Debug)]
enum Geometry {
    /// The direction (sin, −cos of the CSS angle), the center and the
    /// gradient line's length.
    Linear {
        dx: f64,
        dy: f64,
        cx: f64,
        cy: f64,
        len: f64,
    },
    Radial {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
    },
    /// The start angle in turns.
    Conic {
        cx: f64,
        cy: f64,
        from: f64,
    },
}

#[derive(Clone, Debug)]
struct Segment {
    start: f64,
    end: f64,
    /// The hint as a fraction of the segment, if any.
    hint: Option<f64>,
    /// Premultiplied colors from start to end.
    lut: Vec<[u8; 4]>,
}

/// A gradient resolved for a tile of `width` × `height`.
#[derive(Clone, Debug)]
pub struct Gradient {
    pub width: f64,
    pub height: f64,
    geometry: Geometry,
    repeating: bool,
    segments: Vec<Segment>,
    first: [u8; 4],
    last: [u8; 4],
}

/// A position (`<position>`) inside a box of `w` × `h`: one to four
/// values of keywords and lengths (offsets follow their edge keyword
/// in the three- and four-value forms).
pub fn position(values: &[Value], w: f64, h: f64) -> (f64, f64) {
    let mut x = None;
    let mut y = None;
    let mut index = 0;
    let mut i = 0;
    while i < values.len() {
        let v = &values[i];
        match v.keyword() {
            Some(k) => {
                let offset = if values.len() > 2 {
                    values.get(i + 1).and_then(value_lp)
                } else {
                    None
                };
                if offset.is_some() {
                    i += 1;
                }
                let off = |basis: f64| offset.as_ref().map_or(0.0, |l| l.resolve(basis));
                match k {
                    "left" | "x-start" => x = Some(off(w)),
                    "right" | "x-end" => x = Some(w - off(w)),
                    "top" | "y-start" => y = Some(off(h)),
                    "bottom" | "y-end" => y = Some(h - off(h)),
                    _ => {}
                }
            }
            None => {
                if let Some(l) = value_lp(v) {
                    if index == 0 {
                        x = Some(l.resolve(w));
                    } else {
                        y = Some(l.resolve(h));
                    }
                }
            }
        }
        index += 1;
        i += 1;
    }
    // `center` and missing axes are centered.
    (x.unwrap_or(w / 2.0), y.unwrap_or(h / 2.0))
}

fn angle_deg(v: &Value) -> Option<f64> {
    match v {
        Value::Dimension(n, u) => match u.as_str() {
            "deg" => Some(*n),
            "rad" => Some(n * 180.0 / PI),
            "grad" => Some(n * 0.9),
            "turn" => Some(n * 360.0),
            _ => None,
        },
        Value::Number(n) | Value::Integer(n) if *n == 0.0 => Some(0.0),
        _ => None,
    }
}

/// The CSS angle of `to <side or corner>` in a box of `w` × `h`.
fn to_angle(sides: &[String], w: f64, h: f64) -> f64 {
    let has = |s: &str| sides.iter().any(|k| k == s);
    let a = h.atan2(w) * 180.0 / PI;
    match (has("top"), has("bottom"), has("left"), has("right")) {
        (true, _, false, true) => a,
        (_, true, false, true) => 180.0 - a,
        (_, true, true, _) => 180.0 + a,
        (true, _, true, _) => 360.0 - a,
        (true, ..) => 0.0,
        (_, true, ..) => 180.0,
        (.., true, _) => 270.0,
        _ => 90.0,
    }
}

impl Gradient {
    /// Resolves a computed gradient for a tile of `w` × `h`; `style`
    /// resolves `currentcolor`.
    pub fn new(g: &CssGradient, w: f64, h: f64, style: &ComputedStyle) -> Gradient {
        let (geometry, line) = match &g.kind {
            Kind::Linear(dir) => {
                let deg = match dir {
                    None => 180.0,
                    Some(Direction::Angle(v)) => angle_deg(v).unwrap_or(180.0),
                    Some(Direction::To(sides)) => to_angle(sides, w, h),
                };
                let rad = deg * PI / 180.0;
                let (s, c) = (rad.sin(), rad.cos());
                let len = (w * s).abs() + (h * c).abs();
                (
                    Geometry::Linear {
                        dx: s,
                        dy: -c,
                        cx: w / 2.0,
                        cy: h / 2.0,
                        len,
                    },
                    len,
                )
            }
            Kind::Radial {
                shape,
                size,
                position: pos,
            } => {
                let (cx, cy) = pos
                    .as_ref()
                    .map_or((w / 2.0, h / 2.0), |p| position(p, w, h));
                let (rx, ry) = radial_size(shape.as_deref(), size, cx, cy, w, h);
                (
                    Geometry::Radial {
                        cx,
                        cy,
                        rx: rx.max(1e-6),
                        ry: ry.max(1e-6),
                    },
                    rx.max(1e-6),
                )
            }
            Kind::Conic {
                from,
                position: pos,
            } => {
                let (cx, cy) = pos
                    .as_ref()
                    .map_or((w / 2.0, h / 2.0), |p| position(p, w, h));
                let from = from.as_ref().and_then(angle_deg).unwrap_or(0.0) / 360.0;
                (Geometry::Conic { cx, cy, from }, 1.0)
            }
        };
        let conic = matches!(geometry, Geometry::Conic { .. });
        let resolve_pos = |v: &Value| -> Option<f64> {
            if conic {
                if let Value::Percentage(p) = v {
                    return Some(p / 100.0);
                }
                return angle_deg(v).map(|d| d / 360.0);
            }
            value_lp(v).map(|l| {
                if line > 0.0 {
                    l.resolve(line) / line
                } else {
                    0.0
                }
            })
        };
        // Color stops (with their optional positions) and hints.
        let mut colors: Vec<(Absolute, Option<f64>)> = Vec::new();
        let mut hints: Vec<(usize, f64)> = Vec::new();
        let mut all_legacy = true;
        for s in &g.stops {
            match s {
                Stop::Color(c, positions) => {
                    let a = c
                        .resolve(style.color, false)
                        .unwrap_or(Absolute::TRANSPARENT);
                    all_legacy &= a.legacy;
                    if positions.is_empty() {
                        colors.push((a, None));
                    }
                    for p in positions {
                        colors.push((a, resolve_pos(p)));
                    }
                }
                Stop::Hint(v) => {
                    if let Some(p) = resolve_pos(v) {
                        hints.push((colors.len(), p));
                    }
                }
            }
        }
        if colors.is_empty() {
            colors.push((Absolute::TRANSPARENT, None));
        }
        let positions = fixup(&colors.iter().map(|c| c.1).collect::<Vec<_>>());
        let (space, hue) = match &g.interpolation {
            Some(m) => (m.space, m.hue),
            None if all_legacy => (Space::Srgb, HueMethod::Shorter),
            None => (Space::Oklab, HueMethod::Shorter),
        };
        let pm = |a: &Absolute| color::rgba(a).premultiplied();
        let mut segments = Vec::new();
        for i in 0..colors.len().saturating_sub(1) {
            let (start, end) = (positions[i], positions[i + 1]);
            if end <= start {
                continue;
            }
            let hint = hints
                .iter()
                .find(|(at, _)| *at == i + 1)
                .map(|(_, p)| ((p.clamp(start, end) - start) / (end - start)));
            let (a, b) = (&colors[i].0, &colors[i + 1].0);
            let lut = (0..STEPS)
                .map(|k| {
                    let t = k as f64 / (STEPS - 1) as f64;
                    pm(&interpolate(space, hue, a, b, t))
                })
                .collect();
            segments.push(Segment {
                start,
                end,
                hint,
                lut,
            });
        }
        let first = pm(&colors[0].0);
        let last = pm(&colors[colors.len() - 1].0);
        let (p0, p1) = (positions[0], positions[positions.len() - 1]);
        let mut repeating = g.repeating;
        let mut segs = segments;
        if repeating && p1 - p0 <= 1e-9 {
            // A repeating gradient of zero length paints its average
            // color; vidrio uses the last one.
            repeating = false;
            segs.clear();
        }
        Gradient {
            width: w,
            height: h,
            geometry,
            repeating,
            segments: segs,
            first,
            last,
        }
    }

    fn range(&self) -> (f64, f64) {
        match (self.segments.first(), self.segments.last()) {
            (Some(a), Some(b)) => (a.start, b.end),
            _ => (0.0, 0.0),
        }
    }

    /// The position on the gradient line of the point (`x`, `y`) of the
    /// tile.
    fn t(&self, x: f64, y: f64) -> f64 {
        match self.geometry {
            Geometry::Linear {
                dx,
                dy,
                cx,
                cy,
                len,
            } => {
                if len <= 0.0 {
                    return 0.0;
                }
                ((x - cx) * dx + (y - cy) * dy) / len + 0.5
            }
            Geometry::Radial { cx, cy, rx, ry } => {
                let (u, v) = ((x - cx) / rx, (y - cy) / ry);
                (u * u + v * v).sqrt()
            }
            Geometry::Conic { cx, cy, from } => {
                let a = (x - cx).atan2(-(y - cy)) / (2.0 * PI);
                (a - from).rem_euclid(1.0)
            }
        }
    }

    /// The premultiplied color at (`x`, `y`) of the tile.
    pub fn color_at(&self, x: f64, y: f64) -> [u8; 4] {
        let mut t = self.t(x, y);
        if self.segments.is_empty() {
            return self.last;
        }
        let (p0, p1) = self.range();
        if self.repeating {
            t = p0 + (t - p0).rem_euclid(p1 - p0);
        }
        if t < p0 {
            return self.first;
        }
        if t >= p1 {
            return self.last;
        }
        let i = self
            .segments
            .partition_point(|s| s.end <= t)
            .min(self.segments.len() - 1);
        let s = &self.segments[i];
        if t < s.start {
            // In a hard stop's gap: the color before it.
            return s.lut[0];
        }
        let mut u = (t - s.start) / (s.end - s.start);
        if let Some(h) = s.hint {
            u = if h <= 0.0 {
                1.0
            } else if h >= 1.0 {
                0.0
            } else {
                u.powf(0.5f64.ln() / h.ln())
            };
        }
        s.lut[((u * (STEPS - 1) as f64).round() as usize).min(STEPS - 1)]
    }

    /// The gradient drawn into an image of the tile size (rounded up).
    pub fn rasterize(&self) -> Image {
        let w = self.width.ceil().max(1.0) as u32;
        let h = self.height.ceil().max(1.0) as u32;
        let mut img = Image::transparent(w, h);
        for y in 0..h {
            for x in 0..w {
                let c = self.color_at(x as f64 + 0.5, y as f64 + 0.5);
                let i = (y as usize * w as usize + x as usize) * 4;
                img.data[i..i + 4].copy_from_slice(&c);
            }
        }
        img
    }
}

/// The color stop fixup (css-images-3 §3.5.3): a missing first position
/// is 0, a missing last one 1, positions never go back, and runs of
/// missing positions are spread evenly.
fn fixup(p: &[Option<f64>]) -> Vec<f64> {
    let n = p.len();
    let mut out: Vec<Option<f64>> = p.to_vec();
    if out[0].is_none() {
        out[0] = Some(0.0);
    }
    if n > 1 && out[n - 1].is_none() {
        out[n - 1] = Some(1.0);
    }
    let mut max = f64::NEG_INFINITY;
    for v in out.iter_mut().flatten() {
        if *v < max {
            *v = max;
        }
        max = *v;
    }
    let mut i = 0;
    while i < n {
        if out[i].is_some() {
            i += 1;
            continue;
        }
        let start = i - 1;
        let mut end = i;
        while out[end].is_none() {
            end += 1;
        }
        let (a, b) = (out[start].unwrap_or(0.0), out[end].unwrap_or(1.0));
        let steps = (end - start) as f64;
        for (k, slot) in out.iter_mut().enumerate().take(end).skip(start + 1) {
            *slot = Some(a + (b - a) * (k - start) as f64 / steps);
        }
        i = end;
    }
    out.into_iter().map(|v| v.unwrap_or(0.0)).collect()
}

/// The radii of a radial gradient's ending shape.
fn radial_size(
    shape: Option<&str>,
    size: &[Value],
    cx: f64,
    cy: f64,
    w: f64,
    h: f64,
) -> (f64, f64) {
    let circle = match shape {
        Some("circle") => true,
        Some("ellipse") => false,
        _ => size.len() == 1 && size[0].keyword().is_none(),
    };
    let (dl, dr, dt, db) = (cx.abs(), (w - cx).abs(), cy.abs(), (h - cy).abs());
    let corner = |far: bool| {
        let pick = |a: f64, b: f64| if far { a.max(b) } else { a.min(b) };
        (pick(dl, dr), pick(dt, db))
    };
    let keyword = size
        .iter()
        .find_map(|v| v.keyword())
        .unwrap_or("farthest-corner");
    let lengths: Vec<f64> = size
        .iter()
        .filter_map(value_lp)
        .enumerate()
        .map(|(i, l)| l.resolve(if i == 0 { w } else { h }))
        .collect();
    if circle {
        if let Some(&r) = lengths.first() {
            return (r, r);
        }
        let r = match keyword {
            "closest-side" => dl.min(dr).min(dt).min(db),
            "farthest-side" => dl.max(dr).max(dt).max(db),
            "closest-corner" => {
                let (x, y) = corner(false);
                x.hypot(y)
            }
            _ => {
                let (x, y) = corner(true);
                x.hypot(y)
            }
        };
        return (r, r);
    }
    if lengths.len() == 2 {
        return (lengths[0], lengths[1]);
    }
    match keyword {
        "closest-side" => corner(false),
        "farthest-side" => corner(true),
        "closest-corner" => {
            let (x, y) = corner(false);
            (x * 2f64.sqrt(), y * 2f64.sqrt())
        }
        _ => {
            let (x, y) = corner(true);
            (x * 2f64.sqrt(), y * 2f64.sqrt())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_fixup_spreads_and_clamps() {
        assert_eq!(fixup(&[None, None, None]), vec![0.0, 0.5, 1.0]);
        assert_eq!(
            fixup(&[None, Some(0.8), Some(0.2), None]),
            vec![0.0, 0.8, 0.8, 1.0]
        );
    }

    #[test]
    fn corner_angles_follow_the_box() {
        let to = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!((to_angle(&to(&["top", "right"]), 100.0, 100.0) - 45.0).abs() < 1e-9);
        assert!((to_angle(&to(&["bottom", "left"]), 100.0, 100.0) - 225.0).abs() < 1e-9);
        assert_eq!(to_angle(&to(&["left"]), 10.0, 20.0), 270.0);
    }

    #[test]
    fn positions_take_keywords_and_offsets() {
        let px = |v: f64| Value::Dimension(v, "px".to_string());
        let k = |s: &str| Value::Keyword(s.to_string());
        assert_eq!(position(&[k("center")], 100.0, 50.0), (50.0, 25.0));
        assert_eq!(position(&[k("right"), k("top")], 100.0, 50.0), (100.0, 0.0));
        assert_eq!(position(&[px(10.0), px(20.0)], 100.0, 50.0), (10.0, 20.0));
        assert_eq!(
            position(&[k("right"), px(10.0), k("bottom"), px(5.0)], 100.0, 50.0),
            (90.0, 45.0)
        );
        assert_eq!(
            position(&[Value::Percentage(25.0)], 100.0, 50.0),
            (25.0, 25.0)
        );
    }
}
