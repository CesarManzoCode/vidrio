//! Borders and outlines (css-backgrounds-3 §4–§6, CSS 2 §8.5.3,
//! css-ui-4 §3): the ring between a box's border edge and padding edge
//! (each with its elliptical corners), split between the four sides by
//! the lines that join the outer and inner corners, painted in every
//! line style; and `border-image` nine-slice drawing with `stretch`,
//! `repeat`, `round` and `space`.

use std::rc::Rc;

use vgfx::image::Image;
use vgfx::outline::Outline;
use vgfx::raster::FillRule;

use super::color::Rgba;
use super::list::{Op, RRect, Radii, Tiling, no_radii};
use crate::layout::fragment::Rect;

/// A border line style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineStyle {
    None,
    Hidden,
    Solid,
    Double,
    Dotted,
    Dashed,
    Groove,
    Ridge,
    Inset,
    Outset,
}

impl LineStyle {
    pub fn parse(s: &str) -> LineStyle {
        match s {
            "solid" => LineStyle::Solid,
            "double" => LineStyle::Double,
            "dotted" => LineStyle::Dotted,
            "dashed" => LineStyle::Dashed,
            "groove" => LineStyle::Groove,
            "ridge" => LineStyle::Ridge,
            "inset" => LineStyle::Inset,
            "outset" => LineStyle::Outset,
            "hidden" => LineStyle::Hidden,
            // `auto` (outlines) draws a solid line.
            "auto" => LineStyle::Solid,
            _ => LineStyle::None,
        }
    }

    pub fn visible(self) -> bool {
        !matches!(self, LineStyle::None | LineStyle::Hidden)
    }
}

/// One side of a border.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Side {
    pub width: f64,
    pub style: LineStyle,
    pub color: Rgba,
}

impl Side {
    fn shown(&self) -> bool {
        self.width > 0.0 && self.style.visible() && !self.color.is_transparent()
    }
}

/// A used border width: lengths between 0 and 1 device pixel become 1,
/// others are floored to whole pixels (css-values-4 §4.5, snapping).
pub fn snap_width(w: f64) -> f64 {
    if w <= 0.0 {
        0.0
    } else if w < 1.0 {
        1.0
    } else {
        w.floor()
    }
}

/// A rectangle with its edges rounded to whole pixels.
pub fn snap_rect(r: Rect) -> Rect {
    let (x0, y0) = (r.x.round(), r.y.round());
    let (x1, y1) = (r.right().round(), r.bottom().round());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

fn to_f32(r: &Radii) -> vgfx::path::Radii {
    r.map(|(x, y)| (x as f32, y as f32))
}

fn from_f32(r: &vgfx::path::Radii) -> Radii {
    r.map(|(x, y)| (f64::from(x), f64::from(y)))
}

/// Radii scaled down so adjacent corners fit in `rect`.
pub fn fit(r: &Radii, rect: &Rect) -> Radii {
    from_f32(&vgfx::path::fit_radii(
        &to_f32(r),
        rect.width as f32,
        rect.height as f32,
    ))
}

/// The radii of a curve inset by (top, right, bottom, left).
pub fn inset(r: &Radii, d: (f64, f64, f64, f64)) -> Radii {
    from_f32(&vgfx::path::inset_radii(
        &to_f32(r),
        (d.0 as f32, d.1 as f32, d.2 as f32, d.3 as f32),
    ))
}

fn inset_rect(r: &Rect, d: (f64, f64, f64, f64)) -> Rect {
    Rect::new(
        r.x + d.3,
        r.y + d.0,
        (r.width - d.1 - d.3).max(0.0),
        (r.height - d.0 - d.2).max(0.0),
    )
}

fn shape_inset(outer: &RRect, d: (f64, f64, f64, f64)) -> RRect {
    RRect {
        rect: inset_rect(&outer.rect, d),
        radii: inset(&outer.radii, d),
    }
}

/// A ring between two shapes (the even-odd rule leaves the hole).
fn ring(outer: &RRect, inner: &RRect) -> Outline {
    let mut o = outer.path();
    if inner.rect.width > 0.0 && inner.rect.height > 0.0 {
        let p = inner.path();
        o.segments.extend(p.segments);
    }
    o
}

/// The darker shade of a color for 3D border styles (the one Chromium
/// uses: black becomes rgb(84, 84, 84)).
pub fn dark(c: Rgba) -> Rgba {
    let v = c.r.max(c.g).max(c.b);
    if v <= 0.0 {
        return Rgba::new(84.0 / 255.0, 84.0 / 255.0, 84.0 / 255.0, c.a);
    }
    let m = ((v - 0.33) / v).max(0.0);
    Rgba::new(c.r * m, c.g * m, c.b * m, c.a)
}

/// The wedge of side `i` (top, right, bottom, left): the trapezoid from
/// the outer corners to the inner ones, extended outward so curves stay
/// inside.
fn wedge(o: &Rect, i: &Rect, side: usize) -> Outline {
    let (ox0, oy0, ox1, oy1) = (o.x, o.y, o.right(), o.bottom());
    let (ix0, iy0, ix1, iy1) = (i.x, i.y, i.right(), i.bottom());
    // Extend the corner lines past the outer box so antialiased edges
    // of the outer curve are not cut.
    let ext = |ax: f64, ay: f64, bx: f64, by: f64| {
        let (dx, dy) = (ax - bx, ay - by);
        (ax + dx * 4.0, ay + dy * 4.0)
    };
    let tl = ext(ox0, oy0, ix0, iy0);
    let tr = ext(ox1, oy0, ix1, iy0);
    let br = ext(ox1, oy1, ix1, iy1);
    let bl = ext(ox0, oy1, ix0, iy1);
    let pts = match side {
        0 => [tl, tr, (ix1, iy0), (ix0, iy0)],
        1 => [tr, br, (ix1, iy1), (ix1, iy0)],
        2 => [br, bl, (ix0, iy1), (ix1, iy1)],
        _ => [bl, tl, (ix0, iy0), (ix0, iy1)],
    };
    let pts: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
    vgfx::path::polygon(&pts)
}

/// Draws a border ring: `outer` is the border edge with its radii and
/// `sides` the top, right, bottom and left sides (widths already
/// snapped).
pub fn draw(outer: &RRect, sides: &[Side; 4], out: &mut Vec<Op>) {
    if !sides.iter().any(Side::shown) {
        return;
    }
    let w = (
        sides[0].width,
        sides[1].width,
        sides[2].width,
        sides[3].width,
    );
    let inner = shape_inset(outer, w);
    let first = sides.iter().find(|s| s.shown()).copied().expect("a side");
    let uniform = first.style == LineStyle::Solid
        && sides.iter().all(|s| {
            s.width == 0.0 || (s.shown() && s.style == LineStyle::Solid && s.color == first.color)
        });
    if uniform {
        if no_radii(&outer.radii) {
            // Four rectangles: exact on whole pixels.
            let (o, i) = (&outer.rect, &inner.rect);
            let rects = [
                Rect::new(o.x, o.y, o.width, i.y - o.y),
                Rect::new(o.x, i.bottom(), o.width, o.bottom() - i.bottom()),
                Rect::new(o.x, i.y, i.x - o.x, i.height),
                Rect::new(i.right(), i.y, o.right() - i.right(), i.height),
            ];
            for r in rects {
                if r.width > 0.0 && r.height > 0.0 {
                    out.push(Op::Rect {
                        rect: r,
                        color: first.color,
                    });
                }
            }
        } else {
            out.push(Op::Path {
                path: ring(outer, &inner),
                rule: FillRule::EvenOdd,
                color: first.color,
                mask: None,
            });
        }
        return;
    }
    for (k, s) in sides.iter().enumerate() {
        if !s.shown() {
            continue;
        }
        let mask = Some(wedge(&outer.rect, &inner.rect, k));
        let top_left = k == 0 || k == 3;
        let fill = |out: &mut Vec<Op>, path: Outline, color: Rgba| {
            out.push(Op::Path {
                path,
                rule: FillRule::EvenOdd,
                color,
                mask: mask.clone(),
            })
        };
        match s.style {
            LineStyle::Double if s.width >= 3.0 => {
                let third = |v: f64| (v / 3.0).round();
                let t = (third(w.0), third(w.1), third(w.2), third(w.3));
                let mid_outer = shape_inset(outer, t);
                let back = (w.0 - t.0, w.1 - t.1, w.2 - t.2, w.3 - t.3);
                let mid_inner = shape_inset(outer, back);
                let mut p = ring(outer, &mid_outer);
                p.segments.extend(ring(&mid_inner, &inner).segments);
                fill(out, p, s.color);
            }
            LineStyle::Groove | LineStyle::Ridge => {
                let half = (w.0 / 2.0, w.1 / 2.0, w.2 / 2.0, w.3 / 2.0);
                let mid = shape_inset(outer, half);
                let groove = s.style == LineStyle::Groove;
                let (a, b) = if groove == top_left {
                    (dark(s.color), s.color)
                } else {
                    (s.color, dark(s.color))
                };
                fill(out, ring(outer, &mid), a);
                fill(out, ring(&mid, &inner), b);
            }
            LineStyle::Inset | LineStyle::Outset => {
                let darker = (s.style == LineStyle::Inset) == top_left;
                let c = if darker { dark(s.color) } else { s.color };
                fill(out, ring(outer, &inner), c);
            }
            LineStyle::Dotted | LineStyle::Dashed if no_radii(&outer.radii) => {
                let mut p = Outline::new();
                dashes(&outer.rect, &inner.rect, k, s, &mut p);
                fill(out, p, s.color);
            }
            _ => fill(out, ring(outer, &inner), s.color),
        }
    }
}

/// The dots or dashes of side `k` along the band between `o` and `i`.
fn dashes(o: &Rect, i: &Rect, k: usize, s: &Side, p: &mut Outline) {
    let horizontal = k == 0 || k == 2;
    let (start, len) = if horizontal {
        (o.x, o.width)
    } else {
        (o.y, o.height)
    };
    let (b0, b1) = match k {
        0 => (o.y, i.y),
        1 => (i.right(), o.right()),
        2 => (i.bottom(), o.bottom()),
        _ => (o.x, i.x),
    };
    let w = s.width;
    let mut push = |a: f64, b: f64| {
        let (x0, y0, x1, y1) = if horizontal {
            (a, b0, b, b1)
        } else {
            (b0, a, b1, b)
        };
        if s.style == LineStyle::Dotted {
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let r = (w / 2.0) as f32;
            p.segments
                .extend(vgfx::path::ellipse(cx as f32, cy as f32, r, r).segments);
        } else {
            vgfx::path::push_rect(p, x0 as f32, y0 as f32, x1 as f32, y1 as f32, false);
        }
    };
    if s.style == LineStyle::Dotted {
        let n = ((len / w + 1.0) / 2.0).round().max(1.0);
        let step = if n > 1.0 { (len - w) / (n - 1.0) } else { 0.0 };
        for j in 0..n as usize {
            let a = if n > 1.0 {
                start + j as f64 * step
            } else {
                start + (len - w) / 2.0
            };
            push(a, a + w);
        }
    } else {
        let n = ((len + 3.0 * w) / (6.0 * w)).round().max(1.0);
        let unit = len / (2.0 * n - 1.0);
        for j in 0..n as usize {
            let a = start + 2.0 * j as f64 * unit;
            push(a, a + unit);
        }
    }
}

/// How a `border-image` part repeats along one axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageRepeat {
    Stretch,
    Repeat,
    Round,
    Space,
}

/// A resolved `border-image`.
pub struct BorderImage {
    pub image: Rc<Image>,
    /// Slice offsets into the image (top, right, bottom, left), pixels.
    pub slice: [f64; 4],
    pub fill: bool,
    /// The border image widths (top, right, bottom, left).
    pub widths: [f64; 4],
    pub area: Rect,
    pub repeat: (ImageRepeat, ImageRepeat),
}

/// One axis of an edge or the middle: the tile size along it and its
/// first tile's start and the gap.
fn tile_axis(
    r: ImageRepeat,
    start: f64,
    extent: f64,
    natural: f64,
) -> Option<(f64, f64, f64, bool)> {
    if extent <= 0.0 || natural <= 0.0 {
        return None;
    }
    match r {
        ImageRepeat::Stretch => Some((start, extent, 0.0, false)),
        ImageRepeat::Repeat => Some((start + (extent - natural) / 2.0, natural, 0.0, true)),
        ImageRepeat::Round => {
            let n = (extent / natural).round().max(1.0);
            Some((start, extent / n, 0.0, true))
        }
        ImageRepeat::Space => {
            let n = (extent / natural).floor();
            if n < 1.0 {
                return None;
            }
            let gap = (extent - n * natural) / (n + 1.0);
            Some((start + gap, natural, gap, true))
        }
    }
}

/// The nine parts of a border image.
pub fn draw_image(b: &BorderImage, out: &mut Vec<Op>) {
    let (iw, ih) = (f64::from(b.image.width), f64::from(b.image.height));
    let [st, sr, sb, sl] = b.slice.map(|v| v.max(0.0));
    let (st, sb) = if st + sb > ih {
        (ih * st / (st + sb), ih * sb / (st + sb))
    } else {
        (st, sb)
    };
    let (sl, sr) = if sl + sr > iw {
        (iw * sl / (sl + sr), iw * sr / (sl + sr))
    } else {
        (sl, sr)
    };
    let a = &b.area;
    let [mut wt, mut wr, mut wb, mut wl] = b.widths;
    let f = (a.width / (wl + wr)).min(a.height / (wt + wb));
    if f < 1.0 {
        wt *= f;
        wr *= f;
        wb *= f;
        wl *= f;
    }
    let (mx, my, mw, mh) = (a.x + wl, a.y + wt, a.width - wl - wr, a.height - wt - wb);
    let (smw, smh) = (iw - sl - sr, ih - st - sb);
    let mut part = |src: Rect, tile: Rect, gap: (f64, f64), repeat: (bool, bool), area: Rect| {
        if src.width <= 0.0 || src.height <= 0.0 || area.width <= 0.0 || area.height <= 0.0 {
            return;
        }
        out.push(Op::Image {
            image: b.image.clone(),
            src,
            tiling: Tiling {
                tile,
                gap,
                repeat,
                area,
            },
            pixelated: false,
        });
    };
    // Corners: stretched.
    let corners = [
        (Rect::new(0.0, 0.0, sl, st), Rect::new(a.x, a.y, wl, wt)),
        (
            Rect::new(iw - sr, 0.0, sr, st),
            Rect::new(a.right() - wr, a.y, wr, wt),
        ),
        (
            Rect::new(iw - sr, ih - sb, sr, sb),
            Rect::new(a.right() - wr, a.bottom() - wb, wr, wb),
        ),
        (
            Rect::new(0.0, ih - sb, sl, sb),
            Rect::new(a.x, a.bottom() - wb, wl, wb),
        ),
    ];
    for (src, dst) in corners {
        part(src, dst, (0.0, 0.0), (false, false), dst);
    }
    let (rh, rv) = b.repeat;
    // Top and bottom edges: scaled to the border width, repeated
    // horizontally.
    let edges_h = [
        (Rect::new(sl, 0.0, smw, st), a.y, wt, st),
        (Rect::new(sl, ih - sb, smw, sb), a.bottom() - wb, wb, sb),
    ];
    for (src, y, h, sh) in edges_h {
        if sh <= 0.0 || h <= 0.0 {
            continue;
        }
        let natural = smw * h / sh;
        if let Some((x, tw, gap, rep)) = tile_axis(rh, mx, mw, natural) {
            part(
                src,
                Rect::new(x, y, tw, h),
                (gap, 0.0),
                (rep, false),
                Rect::new(mx, y, mw, h),
            );
        }
    }
    let edges_v = [
        (Rect::new(0.0, st, sl, smh), a.x, wl, sl),
        (Rect::new(iw - sr, st, sr, smh), a.right() - wr, wr, sr),
    ];
    for (src, x, w, sw) in edges_v {
        if sw <= 0.0 || w <= 0.0 {
            continue;
        }
        let natural = smh * w / sw;
        if let Some((y, th, gap, rep)) = tile_axis(rv, my, mh, natural) {
            part(
                src,
                Rect::new(x, y, w, th),
                (0.0, gap),
                (false, rep),
                Rect::new(x, my, w, mh),
            );
        }
    }
    if b.fill {
        // The middle is scaled like the top and left edges (or by its
        // own size when those are empty).
        let sx = if st > 0.0 && wt > 0.0 { wt / st } else { 1.0 };
        let sy = if sl > 0.0 && wl > 0.0 { wl / sl } else { 1.0 };
        let nat_w = if rh == ImageRepeat::Stretch {
            mw
        } else {
            smw * sx
        };
        let nat_h = if rv == ImageRepeat::Stretch {
            mh
        } else {
            smh * sy
        };
        if let (Some((x, tw, gx, rx)), Some((y, th, gy, ry))) =
            (tile_axis(rh, mx, mw, nat_w), tile_axis(rv, my, mh, nat_h))
        {
            part(
                Rect::new(sl, st, smw, smh),
                Rect::new(x, y, tw, th),
                (gx, gy),
                (rx, ry),
                Rect::new(mx, my, mw, mh),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_snap_like_browsers() {
        assert_eq!(snap_width(0.3), 1.0);
        assert_eq!(snap_width(2.7), 2.0);
        assert_eq!(snap_width(0.0), 0.0);
    }

    #[test]
    fn uniform_square_border_is_four_rects() {
        let s = Side {
            width: 2.0,
            style: LineStyle::Solid,
            color: Rgba::new(0.0, 0.0, 1.0, 1.0),
        };
        let mut out = Vec::new();
        draw(
            &RRect::square(Rect::new(0.0, 0.0, 10.0, 10.0)),
            &[s; 4],
            &mut out,
        );
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|o| matches!(o, Op::Rect { .. })));
    }

    #[test]
    fn dark_shade_of_black_is_gray() {
        let d = dark(Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert!((d.r - 84.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn repeat_axis_centers_and_space_spreads() {
        assert_eq!(
            tile_axis(ImageRepeat::Repeat, 0.0, 100.0, 30.0),
            Some((35.0, 30.0, 0.0, true))
        );
        assert_eq!(
            tile_axis(ImageRepeat::Space, 0.0, 100.0, 30.0),
            Some((2.5, 30.0, 2.5, true))
        );
        assert_eq!(tile_axis(ImageRepeat::Space, 0.0, 20.0, 30.0), None);
    }
}
