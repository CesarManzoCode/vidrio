//! Rasterizing a paint list: a white canvas, then every item composited
//! source-over (premultiplied) through its clip chain, with layers for
//! group opacity. Clip chains become coverage masks once (cached by
//! node); rectangular clips with whole-pixel edges stay exact.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use vgfx::cache::GlyphCache;
use vgfx::canvas::{Clip, IRect, Paint, Pixmap, blur};
use vgfx::raster::{FillRule, Mask, fill};

use super::list::{ClipChain, ClipNode, DisplayList, Op, RRect, Tiling, no_radii};
use crate::layout::fragment::Rect;

thread_local! {
    static GLYPHS: RefCell<GlyphCache> = RefCell::new(GlyphCache::new());
}

/// The pixel rectangle of `r` with its edges rounded (and bounded).
fn irect(r: &Rect) -> IRect {
    let c = |v: f64| v.round().clamp(-1.0e6, 1.0e6) as i32;
    IRect::new(c(r.x), c(r.y), c(r.right()), c(r.bottom()))
}

fn bounds(r: &IRect) -> (i32, i32, i32, i32) {
    (r.x0, r.y0, r.x1, r.y1)
}

fn clip_for(
    chain: &ClipChain,
    cache: &mut HashMap<*const ClipNode, Rc<Clip>>,
    full: &Rc<Clip>,
) -> Rc<Clip> {
    let Some(node) = chain else {
        return full.clone();
    };
    let key = Rc::as_ptr(node);
    if let Some(c) = cache.get(&key) {
        return c.clone();
    }
    let parent = clip_for(&node.parent, cache, full);
    let shape = &node.shape;
    let rect = irect(&shape.rect);
    let mut c = parent.intersect_rect(rect);
    if !no_radii(&shape.radii) && !c.rect.is_empty() {
        let m = fill(&shape.path(), FillRule::NonZero, bounds(&c.rect));
        c = c.intersect_mask(&m);
    }
    let c = Rc::new(c);
    cache.insert(key, c.clone());
    c
}

/// Renders a paint list.
pub fn render(list: &DisplayList) -> Pixmap {
    let (w, h) = (list.width.max(1), list.height.max(1));
    let mut cur = Pixmap::new(w, h);
    for p in cur.data.chunks_exact_mut(4) {
        p.copy_from_slice(&[255, 255, 255, 255]);
    }
    let mut stack: Vec<(Pixmap, f32)> = Vec::new();
    let mut cache = HashMap::new();
    let full = Rc::new(Clip::full(w, h));
    for item in &list.items {
        match &item.op {
            Op::PushLayer(o) => {
                let prev = std::mem::replace(&mut cur, Pixmap::new(w, h));
                stack.push((prev, *o));
                continue;
            }
            Op::PopLayer => {
                if let Some((mut prev, o)) = stack.pop() {
                    prev.draw_layer(&cur, o);
                    cur = prev;
                }
                continue;
            }
            _ => {}
        }
        let clip = clip_for(&item.clip, &mut cache, &full);
        if clip.rect.is_empty() {
            continue;
        }
        draw(&mut cur, &clip, &item.op);
    }
    while let Some((mut prev, o)) = stack.pop() {
        prev.draw_layer(&cur, o);
        cur = prev;
    }
    cur
}

/// The offset of `p` inside its tile along one axis, if it falls on a
/// tile (not in a gap, not outside a non-repeating tile).
fn local(p: f64, start: f64, size: f64, gap: f64, repeat: bool) -> Option<f64> {
    let d = p - start;
    if !repeat {
        return (0.0..size).contains(&d).then_some(d);
    }
    let m = d.rem_euclid(size + gap);
    (m < size).then_some(m)
}

fn tiled(pix: &mut Pixmap, clip: &Clip, t: &Tiling, color: &dyn Fn(f64, f64) -> [u8; 4]) {
    let area = irect(&t.area).intersect(&clip.rect);
    if area.is_empty() || t.tile.width <= 0.0 || t.tile.height <= 0.0 {
        return;
    }
    let shader = |x: i32, y: i32| -> [u8; 4] {
        let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
        let lx = local(px, t.tile.x, t.tile.width, t.gap.0, t.repeat.0);
        let ly = local(py, t.tile.y, t.tile.height, t.gap.1, t.repeat.1);
        match (lx, ly) {
            (Some(lx), Some(ly)) => color(lx, ly),
            _ => [0; 4],
        }
    };
    pix.fill_rect(
        clip,
        area.x0 as f32,
        area.y0 as f32,
        area.x1 as f32,
        area.y1 as f32,
        &Paint::Shader(&shader),
    );
}

fn shape_mask(s: &RRect, b: (i32, i32, i32, i32)) -> Mask {
    fill(&s.path(), FillRule::NonZero, b)
}

fn draw(pix: &mut Pixmap, clip: &Clip, op: &Op) {
    let b = bounds(&clip.rect);
    match op {
        Op::PushLayer(_) | Op::PopLayer => {}
        Op::Rect { rect, color } => {
            pix.fill_rect(
                clip,
                rect.x as f32,
                rect.y as f32,
                rect.right() as f32,
                rect.bottom() as f32,
                &Paint::Solid(color.premultiplied()),
            );
        }
        Op::Path {
            path,
            rule,
            color,
            mask,
        } => {
            let m = fill(path, *rule, b);
            let paint = Paint::Solid(color.premultiplied());
            match mask {
                Some(mp) => {
                    let c2 = clip.intersect_mask(&fill(mp, FillRule::NonZero, b));
                    pix.fill_mask(&c2, &m, &paint);
                }
                None => pix.fill_mask(clip, &m, &paint),
            }
        }
        Op::Image {
            image,
            src,
            tiling,
            pixelated,
        } => {
            let s = (
                src.x.floor().max(0.0) as u32,
                src.y.floor().max(0.0) as u32,
                (src.right().ceil() as u32).min(image.width),
                (src.bottom().ceil() as u32).min(image.height),
            );
            let (sx, sy) = (
                src.width / tiling.tile.width,
                src.height / tiling.tile.height,
            );
            let color = |lx: f64, ly: f64| {
                let u = (src.x + lx * sx) as f32;
                let v = (src.y + ly * sy) as f32;
                if *pixelated {
                    image.sample_nearest(u, v, s)
                } else {
                    image.sample_bilinear(u, v, s)
                }
            };
            tiled(pix, clip, tiling, &color);
        }
        Op::Gradient { gradient, tiling } => {
            let color = |lx: f64, ly: f64| gradient.color_at(lx, ly);
            tiled(pix, clip, tiling, &color);
        }
        Op::Glyphs {
            face,
            size,
            glyphs,
            color,
            blur: radius,
        } => {
            let paint = Paint::Solid(color.premultiplied());
            GLYPHS.with(|cache| {
                let mut cache = cache.borrow_mut();
                let masks: Vec<(Arc<Mask>, i32, i32)> = glyphs
                    .iter()
                    .map(|&(id, x, y)| {
                        let m = cache.get(face, id, *size as f32, x as f32);
                        (m, x.floor() as i32, y.round() as i32)
                    })
                    .collect();
                if *radius <= 0.0 {
                    for (m, dx, dy) in &masks {
                        pix.fill_mask_at(clip, m, *dx, *dy, &paint);
                    }
                    return;
                }
                // Text shadow with blur: the union of the glyphs, blurred.
                let mut r: Option<IRect> = None;
                for (m, dx, dy) in &masks {
                    if m.is_empty() {
                        continue;
                    }
                    let g = IRect::new(
                        m.left + dx,
                        m.top + dy,
                        m.left + dx + m.width as i32,
                        m.top + dy + m.height as i32,
                    );
                    r = Some(match r {
                        None => g,
                        Some(a) => IRect::new(
                            a.x0.min(g.x0),
                            a.y0.min(g.y0),
                            a.x1.max(g.x1),
                            a.y1.max(g.y1),
                        ),
                    });
                }
                let Some(r) = r else {
                    return;
                };
                let (w, h) = (r.width(), r.height());
                let mut data = vec![0u8; w * h];
                for (m, dx, dy) in &masks {
                    for y in 0..m.height {
                        for x in 0..m.width {
                            let ox = (m.left + dx + x as i32 - r.x0) as usize;
                            let oy = (m.top + dy + y as i32 - r.y0) as usize;
                            let d = &mut data[oy * w + ox];
                            *d = d.saturating_add(m.get(x, y));
                        }
                    }
                }
                let union = Mask {
                    width: w as u32,
                    height: h as u32,
                    left: r.x0,
                    top: r.y0,
                    data,
                };
                pix.fill_mask(clip, &blur(&union, (*radius / 2.0) as f32), &paint);
            });
        }
        Op::Shadow {
            shape,
            box_shape,
            blur: radius,
            color,
            inset,
        } => {
            let sigma = (*radius / 2.0) as f32;
            let ext = (sigma * 3.0).ceil() as i32 + 1;
            let paint = Paint::Solid(color.premultiplied());
            let canvas = (-ext, -ext, pix.width as i32 + ext, pix.height as i32 + ext);
            if !*inset {
                let m = shape_mask(shape, canvas);
                if m.is_empty() {
                    return;
                }
                let m = if sigma > 0.0 { blur(&m, sigma) } else { m };
                let mr = IRect::new(
                    m.left,
                    m.top,
                    m.left + m.width as i32,
                    m.top + m.height as i32,
                );
                let c2 = clip.intersect_rect(mr);
                if c2.rect.is_empty() {
                    return;
                }
                let inside = shape_mask(box_shape, bounds(&c2.rect));
                let c2 = c2.subtract_mask(&inside);
                pix.fill_mask(&c2, &m, &paint);
            } else {
                let br = irect(&box_shape.rect).intersect(&clip.rect);
                if br.is_empty() {
                    return;
                }
                let region = IRect::new(br.x0 - ext, br.y0 - ext, br.x1 + ext, br.y1 + ext);
                let sm = shape_mask(shape, bounds(&region));
                let (w, h) = (region.width(), region.height());
                let mut data = vec![255u8; w * h];
                for y in 0..sm.height {
                    for x in 0..sm.width {
                        let ox = (sm.left + x as i32 - region.x0) as usize;
                        let oy = (sm.top + y as i32 - region.y0) as usize;
                        data[oy * w + ox] = 255 - sm.get(x, y);
                    }
                }
                let outside = Mask {
                    width: w as u32,
                    height: h as u32,
                    left: region.x0,
                    top: region.y0,
                    data,
                };
                let m = if sigma > 0.0 {
                    blur(&outside, sigma)
                } else {
                    outside
                };
                let c2 = clip.intersect_mask(&shape_mask(box_shape, bounds(&br)));
                pix.fill_mask(&c2, &m, &paint);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_repeat_with_gaps() {
        assert_eq!(local(5.5, 0.0, 10.0, 0.0, false), Some(5.5));
        assert_eq!(local(15.5, 0.0, 10.0, 0.0, false), None);
        assert_eq!(local(15.5, 0.0, 10.0, 0.0, true), Some(5.5));
        assert_eq!(local(12.5, 0.0, 10.0, 5.0, true), None);
        assert_eq!(local(-2.5, 0.0, 10.0, 0.0, true), Some(7.5));
    }
}
