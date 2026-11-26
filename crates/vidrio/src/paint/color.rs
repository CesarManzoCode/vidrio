//! Colors for painting (CSS Color 4 §13): every computed color, in any
//! color space, converted to sRGB, mapped into its gamut with the CSS
//! gamut mapping algorithm (chroma reduction in Oklch with the deltaEOK
//! "just noticeable difference" of 0.02), with `currentcolor` and the
//! other unresolved forms resolved against the element's `color`.

use crate::css::style::ComputedStyle;
use crate::css::values::Value;
use crate::css::values::color::{Absolute, Color, Space, convert_coords};

/// A straight-alpha sRGB color with components in [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub fn new(r: f32, g: f32, b: f32, a: f32) -> Rgba {
        Rgba { r, g, b, a }
    }

    pub fn is_transparent(&self) -> bool {
        self.a <= 0.0
    }

    /// The color with its alpha multiplied by `f`.
    pub fn fade(&self, f: f32) -> Rgba {
        Rgba {
            a: self.a * f,
            ..*self
        }
    }

    /// 8-bit premultiplied RGBA (channels rounded before multiplying,
    /// so an opaque color keeps its exact 8-bit value).
    pub fn premultiplied(&self) -> [u8; 4] {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        vgfx::image::premultiply([q(self.r), q(self.g), q(self.b), q(self.a)])
    }
}

fn in_gamut(rgb: &[f64; 3]) -> bool {
    const E: f64 = 1e-6;
    rgb.iter().all(|&v| (-E..=1.0 + E).contains(&v))
}

fn clip(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(|v| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) })
}

fn delta_eok(a: [f64; 3], b: [f64; 3]) -> f64 {
    let x = convert_coords(Space::Srgb, Space::Oklab, a);
    let y = convert_coords(Space::Srgb, Space::Oklab, b);
    ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)).sqrt()
}

/// sRGB coordinates of `c` inside the sRGB gamut (CSS Color 4 §13.2).
pub fn gamut_map(c: &Absolute) -> [f64; 3] {
    let srgb = c.to_srgb();
    let rgb = [srgb[0], srgb[1], srgb[2]];
    if in_gamut(&rgb) {
        return clip(rgb);
    }
    let o = c.convert(Space::Oklch);
    let l = o.c[0].unwrap_or(0.0);
    if l >= 1.0 {
        return [1.0, 1.0, 1.0];
    }
    if l <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let hue = o.c[2].unwrap_or(0.0);
    let to_rgb = |chroma: f64| convert_coords(Space::Oklch, Space::Srgb, [l, chroma, hue]);
    const JND: f64 = 0.02;
    const EPSILON: f64 = 0.0001;
    let (mut min, mut max) = (0.0, o.c[1].unwrap_or(0.0));
    let mut min_in_gamut = true;
    let current = to_rgb(max);
    let mut clipped = clip(current);
    if delta_eok(clipped, current) < JND {
        return clipped;
    }
    while max - min > EPSILON {
        let chroma = (min + max) / 2.0;
        let current = to_rgb(chroma);
        if min_in_gamut && in_gamut(&current) {
            min = chroma;
            continue;
        }
        clipped = clip(current);
        let e = delta_eok(clipped, current);
        if e < JND {
            if JND - e < EPSILON {
                return clipped;
            }
            min_in_gamut = false;
            min = chroma;
        } else {
            max = chroma;
        }
    }
    clipped
}

/// The painted color of an absolute color.
pub fn rgba(c: &Absolute) -> Rgba {
    let [r, g, b] = gamut_map(c);
    let a = c.alpha.unwrap_or(0.0).clamp(0.0, 1.0);
    Rgba::new(r as f32, g as f32, b as f32, a as f32)
}

/// A computed `<color>` resolved for painting (`currentcolor` is the
/// element's `color`).
pub fn resolve(c: &Color, style: &ComputedStyle) -> Rgba {
    match c {
        Color::Absolute(a) => rgba(a),
        _ => c
            .resolve(style.color, false)
            .map_or(Rgba::TRANSPARENT, |a| rgba(&a)),
    }
}

/// The color of property `name` (`currentcolor` when it holds none).
pub fn of(style: &ComputedStyle, name: &str) -> Rgba {
    match style.get(name).and_then(|v| v.values.as_deref()) {
        Some([Value::Color(c)]) => resolve(c, style),
        Some([Value::Keyword(k)]) if k == "transparent" => Rgba::TRANSPARENT,
        _ => rgba(&style.color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_gamut_colors_are_unchanged() {
        let c = Absolute::rgb8(255, 0, 128, 1.0);
        assert_eq!(rgba(&c).premultiplied(), [255, 0, 128, 255]);
    }

    #[test]
    fn out_of_gamut_colors_land_inside() {
        // A saturated Display P3 green is outside sRGB.
        let c = Absolute {
            space: Space::DisplayP3,
            c: [Some(0.0), Some(1.0), Some(0.0)],
            alpha: Some(1.0),
            legacy: false,
        };
        let [r, g, b] = gamut_map(&c);
        assert!((0.0..=1.0).contains(&r) && (0.0..=1.0).contains(&b));
        assert!(g > 0.9);
        // Lightness beyond white maps to white.
        let w = Absolute {
            space: Space::Oklch,
            c: [Some(1.2), Some(0.3), Some(30.0)],
            alpha: Some(1.0),
            legacy: false,
        };
        assert_eq!(gamut_map(&w), [1.0, 1.0, 1.0]);
    }
}
