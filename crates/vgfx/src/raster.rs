//! Antialiased rasterization of outlines by signed area accumulation.
//!
//! Each line segment adds, to every pixel it crosses, the signed area it
//! covers to its right (and the remainder to the next pixel), so a prefix
//! sum along each row gives the winding coverage. Curves are flattened to
//! lines first. Coverage is `min(|winding|, 1)`: the non-zero rule for
//! glyphs, whose contours do not overlap with opposite directions. Edges
//! on pixel boundaries give exact 0 or 1 coverage. [`fill`] rasterizes
//! any path in device pixels with the non-zero or the even-odd rule.

use crate::outline::{Outline, Point, Segment};

/// An 8-bit coverage mask placed relative to an origin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    /// Position of the top-left pixel relative to the origin (y down).
    pub left: i32,
    pub top: i32,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn get(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.data[(y * self.width + x) as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// An accumulation buffer of `width` × `height` pixels.
pub struct Rasterizer {
    w: usize,
    h: usize,
    stride: usize,
    acc: Vec<f32>,
    start: Point,
    cur: Point,
    open: bool,
}

impl Rasterizer {
    pub fn new(width: u32, height: u32) -> Rasterizer {
        let w = width as usize;
        let h = height as usize;
        let stride = w + 2;
        Rasterizer {
            w,
            h,
            stride,
            acc: vec![0.0; stride * h],
            start: Point::default(),
            cur: Point::default(),
            open: false,
        }
    }

    pub fn move_to(&mut self, p: Point) {
        self.close();
        self.start = p;
        self.cur = p;
        self.open = true;
    }

    pub fn line_to(&mut self, p: Point) {
        self.line(self.cur, p);
        self.cur = p;
    }

    pub fn quad_to(&mut self, c: Point, p: Point) {
        let p0 = self.cur;
        let dx = p0.x - 2.0 * c.x + p.x;
        let dy = p0.y - 2.0 * c.y + p.y;
        let dd = dx * dx + dy * dy;
        if dd < 0.333 {
            self.line_to(p);
            return;
        }
        let n = 1 + (3.0 * dd).sqrt().sqrt().floor() as usize;
        let n = n.min(256);
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let q = if i == n {
                p
            } else {
                p0.lerp(c, t).lerp(c.lerp(p, t), t)
            };
            self.line(prev, q);
            prev = q;
        }
        self.cur = p;
    }

    pub fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) {
        let p0 = self.cur;
        let dev = |a: Point, b: Point, c: Point| {
            let dx = a.x - 2.0 * b.x + c.x;
            let dy = a.y - 2.0 * b.y + c.y;
            dx * dx + dy * dy
        };
        let dd = dev(p0, c1, c2).max(dev(c1, c2, p));
        if dd < 0.333 {
            self.line_to(p);
            return;
        }
        let n = (1 + (3.0 * dd).sqrt().sqrt().floor() as usize).min(256);
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let q = if i == n {
                p
            } else {
                let a = p0.lerp(c1, t);
                let b = c1.lerp(c2, t);
                let c = c2.lerp(p, t);
                a.lerp(b, t).lerp(b.lerp(c, t), t)
            };
            self.line(prev, q);
            prev = q;
        }
        self.cur = p;
    }

    /// Closes the open contour with a line back to its start.
    pub fn close(&mut self) {
        if self.open {
            if self.cur != self.start {
                self.line(self.cur, self.start);
            }
            self.cur = self.start;
            self.open = false;
        }
    }

    /// Adds a line, splitting it where it crosses x = 0 and x = width so
    /// that the parts outside act as vertical edges on the border.
    pub fn line(&mut self, p0: Point, p1: Point) {
        if p0.y == p1.y {
            return;
        }
        let w = self.w as f32;
        let inside = |x: f32| (0.0..=w).contains(&x);
        if inside(p0.x) && inside(p1.x) {
            self.raw_line(p0, p1);
            return;
        }
        let mut ts = [0.0f32, 1.0, 1.0, 1.0];
        let mut n = 1;
        for edge in [0.0, w] {
            let dx = p1.x - p0.x;
            if dx != 0.0 {
                let t = (edge - p0.x) / dx;
                if t > 0.0 && t < 1.0 {
                    ts[n] = t;
                    n += 1;
                }
            }
        }
        ts[n] = 1.0;
        ts[1..n].sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for k in 0..n {
            let a = p0.lerp(p1, ts[k]);
            let b = p0.lerp(p1, ts[k + 1]);
            self.raw_line(
                Point::new(a.x.clamp(0.0, w), a.y),
                Point::new(b.x.clamp(0.0, w), b.y),
            );
        }
    }

    fn raw_line(&mut self, p0: Point, p1: Point) {
        if p0.y == p1.y {
            return;
        }
        let (dir, a, b) = if p0.y < p1.y {
            (1.0f32, p0, p1)
        } else {
            (-1.0f32, p1, p0)
        };
        let dxdy = (b.x - a.x) / (b.y - a.y);
        let y_top = a.y.max(0.0);
        let y_bottom = b.y.min(self.h as f32);
        if y_top >= y_bottom {
            return;
        }
        let wf = self.w as f32;
        let mut x = (a.x + (y_top - a.y) * dxdy).clamp(0.0, wf);
        let row_end = y_bottom.ceil() as usize;
        for y in (y_top as usize)..row_end.min(self.h) {
            let line = y * self.stride;
            let dy = ((y + 1) as f32).min(b.y) - (y as f32).max(a.y);
            if dy <= 0.0 {
                continue;
            }
            let xnext = (x + dxdy * dy).clamp(0.0, wf);
            let d = dy * dir;
            let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let x0f = x0.floor();
            let x0i = x0f as usize;
            let x1c = x1.ceil();
            let x1i = x1c as usize;
            if x1i <= x0i + 1 {
                // The segment stays in one pixel column.
                let xm = 0.5 * (x + xnext) - x0f;
                self.acc[line + x0i] += d - d * xm;
                self.acc[line + x0i + 1] += d * xm;
            } else {
                let s = 1.0 / (x1 - x0);
                let x0frac = x0 - x0f;
                let a0 = 0.5 * s * (1.0 - x0frac) * (1.0 - x0frac);
                let x1frac = x1 - x1c + 1.0;
                let am = 0.5 * s * x1frac * x1frac;
                self.acc[line + x0i] += d * a0;
                if x1i == x0i + 2 {
                    self.acc[line + x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0frac);
                    self.acc[line + x0i + 1] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.acc[line + xi] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.acc[line + x1i - 1] += d * (1.0 - a2 - am);
                }
                self.acc[line + x1i] += d * am;
            }
            x = xnext;
        }
    }

    /// Accumulates rows into coverage, one byte per pixel.
    pub fn coverage(self) -> Vec<u8> {
        self.coverage_with(FillRule::NonZero)
    }

    /// Accumulates rows into coverage with `rule`: the non-zero rule
    /// takes `min(|winding|, 1)`, the even-odd rule folds the winding
    /// area into [0, 1] with period 2.
    pub fn coverage_with(mut self, rule: FillRule) -> Vec<u8> {
        self.close();
        let mut out = vec![0u8; self.w * self.h];
        for y in 0..self.h {
            let mut acc = 0.0f32;
            for x in 0..self.w {
                acc += self.acc[y * self.stride + x];
                let c = match rule {
                    FillRule::NonZero => acc.abs().min(1.0),
                    FillRule::EvenOdd => {
                        let v = acc.abs() % 2.0;
                        if v > 1.0 { 2.0 - v } else { v }
                    }
                };
                out[y * self.w + x] = (c * 255.0 + 0.5) as u8;
            }
        }
        out
    }

    /// Adds every segment of `outline` mapped by `map`.
    pub fn add_outline(&mut self, outline: &Outline, map: impl Fn(Point) -> Point) {
        for s in &outline.segments {
            match *s {
                Segment::MoveTo(p) => self.move_to(map(p)),
                Segment::LineTo(p) => self.line_to(map(p)),
                Segment::QuadTo(c, p) => self.quad_to(map(c), map(p)),
                Segment::CubicTo(c1, c2, p) => self.cubic_to(map(c1), map(c2), map(p)),
                Segment::Close => self.close(),
            }
        }
        self.close();
    }
}

/// Rasterizes a glyph outline (font units, y up) at `scale` pixels per
/// unit with the origin at (`dx`, `dy`) inside its pixel (subpixel offset).
pub fn rasterize(outline: &Outline, scale: f32, dx: f32, dy: f32) -> Mask {
    let to_device = |p: Point| Point::new(p.x * scale + dx, -p.y * scale + dy);
    let Some((x0, y0, x1, y1)) = outline.control_bounds() else {
        return Mask::default();
    };
    let a = to_device(Point::new(x0, y1));
    let b = to_device(Point::new(x1, y0));
    let left = a.x.floor() as i32;
    let top = a.y.floor() as i32;
    let right = b.x.ceil() as i32;
    let bottom = b.y.ceil() as i32;
    let width = (right - left).max(0) as u32;
    let height = (bottom - top).max(0) as u32;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Mask {
            left,
            top,
            ..Mask::default()
        };
    }
    let mut r = Rasterizer::new(width, height);
    r.add_outline(outline, |p| {
        let q = to_device(p);
        Point::new(q.x - left as f32, q.y - top as f32)
    });
    Mask {
        width,
        height,
        left,
        top,
        data: r.coverage(),
    }
}

/// How the winding of a path decides what is inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// Rasterizes `path` (device pixels, y down) with `rule` into a mask
/// over its bounds intersected with the pixel rectangle `[x0, x1) ×
/// [y0, y1)`; `left` and `top` of the mask are device coordinates.
pub fn fill(path: &Outline, rule: FillRule, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> Mask {
    let Some((bx0, by0, bx1, by1)) = path.control_bounds() else {
        return Mask::default();
    };
    let left = (bx0.floor() as i32).max(x0);
    let top = (by0.floor() as i32).max(y0);
    let right = (bx1.ceil() as i32).min(x1);
    let bottom = (by1.ceil() as i32).min(y1);
    if right <= left || bottom <= top {
        return Mask {
            left,
            top,
            ..Mask::default()
        };
    }
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    let mut r = Rasterizer::new(width, height);
    let (dx, dy) = (left as f32, top as f32);
    r.add_outline(path, |p| Point::new(p.x - dx, p.y - dy));
    Mask {
        width,
        height,
        left,
        top,
        data: r.coverage_with(rule),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn even_odd_leaves_a_hole() {
        let mut o = rect(0.0, 0.0, 4.0, 4.0);
        o.append(
            &rect(1.0, 1.0, 3.0, 3.0),
            &crate::outline::Transform::IDENTITY,
        );
        let m = fill(&o, FillRule::EvenOdd, (0, 0, 10, 10));
        assert_eq!((m.width, m.height, m.left, m.top), (4, 4, 0, 0));
        assert_eq!(m.get(0, 0), 255);
        assert_eq!(m.get(1, 1), 0);
        let m = fill(&o, FillRule::NonZero, (0, 0, 10, 10));
        assert_eq!(m.get(1, 1), 255);
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Outline {
        let mut o = Outline::new();
        o.move_to(x0, y0);
        o.line_to(x0, y1);
        o.line_to(x1, y1);
        o.line_to(x1, y0);
        o.close();
        o
    }

    #[test]
    fn pixel_aligned_square_is_exact() {
        let m = rasterize(&rect(0.0, 0.0, 4.0, 4.0), 1.0, 0.0, 0.0);
        assert_eq!((m.width, m.height, m.left, m.top), (4, 4, 0, -4));
        assert!(m.data.iter().all(|&c| c == 255));
    }

    #[test]
    fn half_pixel_edge_is_half_covered() {
        let m = rasterize(&rect(0.0, 0.0, 2.5, 1.0), 1.0, 0.0, 0.0);
        assert_eq!(m.width, 3);
        assert_eq!(m.data, vec![255, 255, 128]);
    }

    #[test]
    fn diagonal_triangle_area() {
        // Right triangle covering half of a 1×1 pixel.
        let mut o = Outline::new();
        o.move_to(0.0, 0.0);
        o.line_to(1.0, 1.0);
        o.line_to(1.0, 0.0);
        o.close();
        let m = rasterize(&o, 1.0, 0.0, 0.0);
        assert_eq!(m.data, vec![128]);
    }

    #[test]
    fn clipping_keeps_coverage_exact() {
        let mut r = Rasterizer::new(2, 1);
        let o = rect(-3.0, 0.0, 1.0, 1.0);
        r.add_outline(&o, |p| Point::new(p.x, 1.0 - p.y));
        assert_eq!(r.coverage(), vec![255, 0]);
    }
}
