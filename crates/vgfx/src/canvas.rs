//! A premultiplied RGBA8 pixmap with source-over composition, clips
//! (a pixel rectangle with an optional coverage mask, so a stack of
//! clips is their intersection), group layers composited with an
//! opacity, and a separable Gaussian blur of coverage masks (shadows).
//!
//! Exactness: a fill whose coverage and clip are full writes its color
//! unchanged, so pixel-aligned boxes come out as exact solid colors.

use crate::raster::Mask;

/// A pixel rectangle `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> IRect {
        IRect { x0, y0, x1, y1 }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersect(&self, o: &IRect) -> IRect {
        IRect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }

    pub fn width(&self) -> usize {
        (self.x1 - self.x0).max(0) as usize
    }

    pub fn height(&self) -> usize {
        (self.y1 - self.y0).max(0) as usize
    }
}

/// A clip: what is outside `rect` is clipped away; inside, `mask` (one
/// byte per pixel of `rect`, rows top to bottom) scales coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub rect: IRect,
    pub mask: Option<Vec<u8>>,
}

/// Multiplies two 8-bit fractions.
pub fn mul255(a: u32, b: u32) -> u32 {
    let x = a * b + 128;
    (x + (x >> 8)) >> 8
}

impl Clip {
    /// No clipping inside a `width` × `height` pixmap.
    pub fn full(width: u32, height: u32) -> Clip {
        Clip {
            rect: IRect::new(0, 0, width as i32, height as i32),
            mask: None,
        }
    }

    /// The coverage the clip lets through at (`x`, `y`).
    pub fn coverage(&self, x: i32, y: i32) -> u32 {
        let r = &self.rect;
        if x < r.x0 || y < r.y0 || x >= r.x1 || y >= r.y1 {
            return 0;
        }
        match &self.mask {
            None => 255,
            Some(m) => u32::from(m[(y - r.y0) as usize * r.width() + (x - r.x0) as usize]),
        }
    }

    /// The clip intersected with a pixel rectangle.
    pub fn intersect_rect(&self, r: IRect) -> Clip {
        let rect = self.rect.intersect(&r);
        if rect.is_empty() {
            return Clip {
                rect: IRect::default(),
                mask: None,
            };
        }
        let mask = self.mask.as_ref().map(|_| {
            let mut out = Vec::with_capacity(rect.width() * rect.height());
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    out.push(self.coverage(x, y) as u8);
                }
            }
            out
        });
        Clip { rect, mask }
    }

    /// The clip intersected with a coverage mask (zero outside it).
    pub fn intersect_mask(&self, m: &Mask) -> Clip {
        let mr = IRect::new(
            m.left,
            m.top,
            m.left + m.width as i32,
            m.top + m.height as i32,
        );
        let rect = self.rect.intersect(&mr);
        if rect.is_empty() {
            return Clip {
                rect: IRect::default(),
                mask: None,
            };
        }
        let mut out = Vec::with_capacity(rect.width() * rect.height());
        let mut all_full = true;
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let c = mul255(
                    self.coverage(x, y),
                    u32::from(m.get((x - m.left) as u32, (y - m.top) as u32)),
                );
                all_full &= c == 255;
                out.push(c as u8);
            }
        }
        Clip {
            rect,
            mask: (!all_full).then_some(out),
        }
    }

    /// The clip with the region of `m` removed (coverage times 1 − m).
    pub fn subtract_mask(&self, m: &Mask) -> Clip {
        let mut out = Vec::with_capacity(self.rect.width() * self.rect.height());
        for y in self.rect.y0..self.rect.y1 {
            for x in self.rect.x0..self.rect.x1 {
                let inside = if x >= m.left && y >= m.top {
                    u32::from(m.get((x - m.left) as u32, (y - m.top) as u32))
                } else {
                    0
                };
                out.push(mul255(self.coverage(x, y), 255 - inside) as u8);
            }
        }
        Clip {
            rect: self.rect,
            mask: Some(out),
        }
    }
}

/// What a fill paints: one premultiplied color, or a premultiplied
/// color per device pixel.
pub enum Paint<'a> {
    Solid([u8; 4]),
    Shader(&'a dyn Fn(i32, i32) -> [u8; 4]),
}

impl Paint<'_> {
    fn at(&self, x: i32, y: i32) -> [u8; 4] {
        match self {
            Paint::Solid(c) => *c,
            Paint::Shader(f) => f(x, y),
        }
    }
}

/// Composites premultiplied `src` scaled by `cov` over `dst`.
#[inline]
pub fn blend(dst: &mut [u8], src: [u8; 4], cov: u32) {
    if cov == 0 || src[3] == 0 && src[..3] == [0, 0, 0] {
        return;
    }
    let s = if cov >= 255 {
        src
    } else {
        src.map(|c| mul255(u32::from(c), cov) as u8)
    };
    if s[3] == 255 {
        dst[..4].copy_from_slice(&s);
        return;
    }
    let inv = 255 - u32::from(s[3]);
    for k in 0..4 {
        dst[k] = (u32::from(s[k]) + mul255(u32::from(dst[k]), inv)).min(255) as u8;
    }
}

/// A premultiplied RGBA8 pixmap, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Pixmap {
    /// A transparent pixmap.
    pub fn new(width: u32, height: u32) -> Pixmap {
        Pixmap {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }

    pub fn bounds(&self) -> IRect {
        IRect::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Fills the rectangle `[x0, x1) × [y0, y1)` (fractional edges get
    /// their exact area coverage).
    pub fn fill_rect(&mut self, clip: &Clip, x0: f32, y0: f32, x1: f32, y1: f32, paint: &Paint) {
        if !(x1 > x0 && y1 > y0) {
            return;
        }
        let r = IRect::new(
            x0.floor() as i32,
            y0.floor() as i32,
            x1.ceil() as i32,
            y1.ceil() as i32,
        )
        .intersect(&clip.rect)
        .intersect(&self.bounds());
        if r.is_empty() {
            return;
        }
        let span = |p: i32, a: f32, b: f32| -> f32 {
            let lo = (p as f32).max(a);
            let hi = ((p + 1) as f32).min(b);
            (hi - lo).clamp(0.0, 1.0)
        };
        let w = self.width as usize;
        for y in r.y0..r.y1 {
            let cy = span(y, y0, y1);
            for x in r.x0..r.x1 {
                let cx = span(x, x0, x1);
                let cov = if cx >= 1.0 && cy >= 1.0 {
                    255
                } else {
                    (cx * cy * 255.0 + 0.5) as u32
                };
                let cov = mul255(cov, clip.coverage(x, y));
                if cov == 0 {
                    continue;
                }
                let i = (y as usize * w + x as usize) * 4;
                blend(&mut self.data[i..i + 4], paint.at(x, y), cov);
            }
        }
    }

    /// Fills through a coverage mask placed in device pixels.
    pub fn fill_mask(&mut self, clip: &Clip, mask: &Mask, paint: &Paint) {
        self.fill_mask_at(clip, mask, 0, 0, paint);
    }

    /// Fills through a coverage mask moved by (`dx`, `dy`) pixels.
    pub fn fill_mask_at(&mut self, clip: &Clip, mask: &Mask, dx: i32, dy: i32, paint: &Paint) {
        let (left, top) = (mask.left + dx, mask.top + dy);
        let r = IRect::new(
            left,
            top,
            left + mask.width as i32,
            top + mask.height as i32,
        )
        .intersect(&clip.rect)
        .intersect(&self.bounds());
        let w = self.width as usize;
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let m = u32::from(mask.get((x - left) as u32, (y - top) as u32));
                if m == 0 {
                    continue;
                }
                let cov = mul255(m, clip.coverage(x, y));
                if cov == 0 {
                    continue;
                }
                let i = (y as usize * w + x as usize) * 4;
                blend(&mut self.data[i..i + 4], paint.at(x, y), cov);
            }
        }
    }

    /// Composites `src` (same size) over this pixmap with `opacity`.
    pub fn draw_layer(&mut self, src: &Pixmap, opacity: f32) {
        let a = (opacity.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        if a == 0 {
            return;
        }
        for (d, s) in self.data.chunks_exact_mut(4).zip(src.data.chunks_exact(4)) {
            blend(d, [s[0], s[1], s[2], s[3]], a);
        }
    }
}

/// The mask blurred with a Gaussian of standard deviation `sigma`
/// pixels; it grows by three deviations on every side.
pub fn blur(mask: &Mask, sigma: f32) -> Mask {
    if sigma <= 0.0 || mask.is_empty() {
        return mask.clone();
    }
    let r = (sigma * 3.0).ceil() as usize;
    let kernel: Vec<f32> = {
        let k: Vec<f32> = (0..=2 * r)
            .map(|i| {
                let d = i as f32 - r as f32;
                (-d * d / (2.0 * sigma * sigma)).exp()
            })
            .collect();
        let sum: f32 = k.iter().sum();
        k.into_iter().map(|v| v / sum).collect()
    };
    let (w, h) = (mask.width as usize, mask.height as usize);
    let (ow, oh) = (w + 2 * r, h + 2 * r);
    // Horizontal pass into a buffer of ow × h.
    let mut tmp = vec![0f32; ow * h];
    for y in 0..h {
        for x in 0..w {
            let v = f32::from(mask.data[y * w + x]);
            if v == 0.0 {
                continue;
            }
            for (k, &kv) in kernel.iter().enumerate() {
                tmp[y * ow + x + k] += v * kv;
            }
        }
    }
    // Vertical pass into ow × oh.
    let mut out = vec![0f32; ow * oh];
    for y in 0..h {
        for x in 0..ow {
            let v = tmp[y * ow + x];
            if v == 0.0 {
                continue;
            }
            for (k, &kv) in kernel.iter().enumerate() {
                out[(y + k) * ow + x] += v * kv;
            }
        }
    }
    Mask {
        width: ow as u32,
        height: oh as u32,
        left: mask.left - r as i32,
        top: mask.top - r as i32,
        data: out
            .into_iter()
            .map(|v| (v + 0.5).min(255.0) as u8)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_aligned_fill_is_exact() {
        let mut p = Pixmap::new(4, 4);
        let clip = Clip::full(4, 4);
        p.fill_rect(&clip, 1.0, 1.0, 3.0, 3.0, &Paint::Solid([10, 20, 30, 255]));
        assert_eq!(p.pixel(1, 1), [10, 20, 30, 255]);
        assert_eq!(p.pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(p.pixel(3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn half_covered_pixel_blends() {
        let mut p = Pixmap::new(2, 1);
        let clip = Clip::full(2, 1);
        p.fill_rect(&clip, 0.0, 0.0, 1.5, 1.0, &Paint::Solid([255, 0, 0, 255]));
        assert_eq!(p.pixel(1, 0), [128, 0, 0, 128]);
    }

    #[test]
    fn layers_composite_with_opacity() {
        let mut base = Pixmap::new(1, 1);
        base.data = vec![0, 0, 255, 255];
        let mut layer = Pixmap::new(1, 1);
        layer.data = vec![255, 0, 0, 255];
        base.draw_layer(&layer, 0.5);
        assert_eq!(base.pixel(0, 0), [128, 0, 127, 255]);
    }

    #[test]
    fn blur_keeps_the_total_and_spreads() {
        let m = Mask {
            width: 1,
            height: 1,
            left: 5,
            top: 5,
            data: vec![255],
        };
        let b = blur(&m, 1.0);
        assert_eq!((b.width, b.left), (7, 2));
        let total: u32 = b.data.iter().map(|&v| u32::from(v)).sum();
        assert!((240..=270).contains(&total));
        assert!(b.get(3, 3) > b.get(2, 3));
    }
}
