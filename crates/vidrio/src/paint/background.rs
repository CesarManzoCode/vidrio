//! Background layers (css-backgrounds-3 §3): the comma-separated lists
//! of `background-image`, `-position-x`, `-position-y`, `-size`,
//! `-repeat`, `-attachment`, `-origin` and `-clip` split into layers
//! (as many as images; shorter lists repeat), and the placement of one
//! layer's image: its size (`cover`, `contain`, lengths and `auto` with
//! the image's natural size and ratio), its position in the background
//! positioning area and its tiling (`repeat`, `space`, `round`,
//! `no-repeat`) over the background painting area.

use crate::css::style::ComputedStyle;
use crate::css::values::Value;
use crate::css::values::image::Image as CssGradient;
use crate::layout::fragment::Rect;
use crate::layout::values::value_lp;

use super::list::Tiling;

/// The image of a layer.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerImage {
    Url(String),
    Gradient(Box<CssGradient>),
}

/// How an image repeats along one axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Repeat,
    Space,
    Round,
    NoRepeat,
}

/// A box of the box model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxArea {
    Border,
    Padding,
    Content,
}

/// One background layer.
#[derive(Clone, Debug)]
pub struct Layer {
    pub image: Option<LayerImage>,
    pub position_x: Vec<Value>,
    pub position_y: Vec<Value>,
    pub size: Vec<Value>,
    pub repeat: (Repeat, Repeat),
    pub fixed: bool,
    pub origin: BoxArea,
    pub clip: BoxArea,
}

/// The comma-separated items of a computed list property.
fn items(style: &ComputedStyle, name: &str) -> Vec<Vec<Value>> {
    let Some(values) = style.get(name).and_then(|v| v.values.clone()) else {
        return vec![Vec::new()];
    };
    let mut out = vec![Vec::new()];
    for v in values {
        if v == Value::Comma {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(v);
        }
    }
    out
}

fn area(values: &[Value], default: BoxArea) -> BoxArea {
    match values.first().and_then(Value::keyword) {
        Some("border-box") => BoxArea::Border,
        Some("padding-box") => BoxArea::Padding,
        Some("content-box") => BoxArea::Content,
        // `text` and `border-area` clip like the border box here.
        _ => default,
    }
}

fn repeat(values: &[Value]) -> (Repeat, Repeat) {
    let one = |k: &str| match k {
        "space" => Repeat::Space,
        "round" => Repeat::Round,
        "no-repeat" => Repeat::NoRepeat,
        _ => Repeat::Repeat,
    };
    let ks: Vec<&str> = values.iter().filter_map(Value::keyword).collect();
    match ks.as_slice() {
        ["repeat-x"] => (Repeat::Repeat, Repeat::NoRepeat),
        ["repeat-y"] => (Repeat::NoRepeat, Repeat::Repeat),
        [k] => (one(k), one(k)),
        [a, b, ..] => (one(a), one(b)),
        [] => (Repeat::Repeat, Repeat::Repeat),
    }
}

/// The background layers of `style`, bottom layer last (as written).
pub fn layers(style: &ComputedStyle) -> Vec<Layer> {
    let images = items(style, "background-image");
    let px = items(style, "background-position-x");
    let py = items(style, "background-position-y");
    let size = items(style, "background-size");
    let rep = items(style, "background-repeat");
    let att = items(style, "background-attachment");
    let origin = items(style, "background-origin");
    let clip = items(style, "background-clip");
    let pick = |list: &Vec<Vec<Value>>, i: usize| list[i % list.len()].clone();
    images
        .iter()
        .enumerate()
        .map(|(i, img)| Layer {
            image: match img.first() {
                Some(Value::Url(u)) => Some(LayerImage::Url(u.clone())),
                Some(Value::Image(g)) => Some(LayerImage::Gradient(g.clone())),
                _ => None,
            },
            position_x: pick(&px, i),
            position_y: pick(&py, i),
            size: pick(&size, i),
            repeat: repeat(&pick(&rep, i)),
            fixed: pick(&att, i).first().and_then(Value::keyword) == Some("fixed"),
            origin: area(&pick(&origin, i), BoxArea::Padding),
            clip: area(&pick(&clip, i), BoxArea::Border),
        })
        .collect()
}

/// The offset of a tile of `tile` pixels inside an area of `extent`
/// pixels along one axis.
fn offset(values: &[Value], extent: f64, tile: f64) -> f64 {
    let free = extent - tile;
    let lp = |v: &Value| value_lp(v).map(|l| l.resolve(free));
    match values {
        [Value::Keyword(k)] => match k.as_str() {
            "center" => free / 2.0,
            "right" | "bottom" | "x-end" | "y-end" => free,
            _ => 0.0,
        },
        [Value::Keyword(k), v] => {
            let o = lp(v).unwrap_or(0.0);
            match k.as_str() {
                "right" | "bottom" | "x-end" | "y-end" => free - o,
                "center" => free / 2.0,
                _ => o,
            }
        }
        [v] => lp(v).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// The size of a layer's image in an area of `area` (css-backgrounds-3
/// §3.9); `natural` is the image's natural size, if it has one.
pub fn image_size(size: &[Value], natural: Option<(f64, f64)>, area: (f64, f64)) -> (f64, f64) {
    let (aw, ah) = area;
    let ratio = natural
        .filter(|&(w, h)| w > 0.0 && h > 0.0)
        .map(|(w, h)| w / h);
    let lp = |v: &Value, basis: f64| -> Option<f64> {
        if v.keyword() == Some("auto") {
            None
        } else {
            value_lp(v).map(|l| l.resolve(basis).max(0.0))
        }
    };
    match size {
        [Value::Keyword(k)] if k == "cover" || k == "contain" => match ratio {
            Some(r) => {
                let by_width = (aw, aw / r);
                let by_height = (ah * r, ah);
                let cover = k == "cover";
                if (by_width.1 >= ah) == cover {
                    by_width
                } else {
                    by_height
                }
            }
            None => (aw, ah),
        },
        _ => {
            let w = size.first().and_then(|v| lp(v, aw));
            let h = size.get(1).and_then(|v| lp(v, ah));
            match (w, h) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, ratio.map_or(ah, |r| w / r)),
                (None, Some(h)) => (ratio.map_or(aw, |r| h * r), h),
                (None, None) => natural.unwrap_or((aw, ah)),
            }
        }
    }
}

/// The tiling of a layer's image positioned in `positioning` and painted
/// over `painting`; `None` if nothing is drawn.
pub fn place(
    layer: &Layer,
    natural: Option<(f64, f64)>,
    positioning: Rect,
    painting: Rect,
) -> Option<Tiling> {
    let area = (positioning.width, positioning.height);
    let (mut w, mut h) = image_size(&layer.size, natural, area);
    let auto_h = layer
        .size
        .get(1)
        .is_none_or(|v| v.keyword() == Some("auto"))
        && !matches!(
            layer.size.first().and_then(Value::keyword),
            Some("cover" | "contain")
        );
    let auto_w = layer
        .size
        .first()
        .is_none_or(|v| v.keyword() == Some("auto"));
    let (rx, ry) = layer.repeat;
    // `round` rescales the image to a whole number of tiles; with `auto`
    // on the other axis, that axis keeps the ratio.
    if rx == Repeat::Round && w > 0.0 && area.0 > 0.0 {
        let n = (area.0 / w).round().max(1.0);
        let nw = area.0 / n;
        if ry != Repeat::Round && auto_h {
            h *= nw / w;
        }
        w = nw;
    }
    if ry == Repeat::Round && h > 0.0 && area.1 > 0.0 {
        let n = (area.1 / h).round().max(1.0);
        let nh = area.1 / n;
        if rx != Repeat::Round && auto_w {
            w *= nh / h;
        }
        h = nh;
    }
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let axis = |r: Repeat, values: &[Value], extent: f64, tile: f64| -> (f64, f64, bool) {
        if r == Repeat::Space {
            let n = (extent / tile).floor();
            if n >= 2.0 {
                return (0.0, (extent - n * tile) / (n - 1.0), true);
            }
            return (offset(values, extent, tile), 0.0, false);
        }
        (offset(values, extent, tile), 0.0, r != Repeat::NoRepeat)
    };
    let (ox, gx, repeat_x) = axis(rx, &layer.position_x, area.0, w);
    let (oy, gy, repeat_y) = axis(ry, &layer.position_y, area.1, h);
    Some(Tiling {
        tile: Rect::new(
            (positioning.x + ox).round(),
            (positioning.y + oy).round(),
            w,
            h,
        ),
        gap: (gx, gy),
        repeat: (repeat_x, repeat_y),
        area: painting,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(v: f64) -> Value {
        Value::Dimension(v, "px".to_string())
    }

    fn k(s: &str) -> Value {
        Value::Keyword(s.to_string())
    }

    #[test]
    fn cover_and_contain_keep_the_ratio() {
        let natural = Some((20.0, 10.0));
        assert_eq!(
            image_size(&[k("contain")], natural, (100.0, 100.0)),
            (100.0, 50.0)
        );
        assert_eq!(
            image_size(&[k("cover")], natural, (100.0, 100.0)),
            (200.0, 100.0)
        );
        assert_eq!(
            image_size(&[px(40.0), k("auto")], natural, (100.0, 100.0)),
            (40.0, 20.0)
        );
        assert_eq!(image_size(&[k("auto")], None, (30.0, 40.0)), (30.0, 40.0));
    }

    #[test]
    fn space_and_round_fit_whole_tiles() {
        let area = Rect::new(0.0, 0.0, 100.0, 30.0);
        let mut layer = Layer {
            image: None,
            position_x: vec![Value::Percentage(0.0)],
            position_y: vec![Value::Percentage(0.0)],
            size: vec![k("auto")],
            repeat: (Repeat::Space, Repeat::NoRepeat),
            fixed: false,
            origin: BoxArea::Padding,
            clip: BoxArea::Border,
        };
        let t = place(&layer, Some((30.0, 30.0)), area, area).unwrap();
        assert_eq!(t.gap, (5.0, 0.0));
        assert_eq!(t.repeat, (true, false));
        layer.repeat = (Repeat::Round, Repeat::NoRepeat);
        let t = place(&layer, Some((30.0, 30.0)), area, area).unwrap();
        assert_eq!((t.tile.width, t.tile.height), (100.0 / 3.0, 100.0 / 3.0));
    }

    #[test]
    fn positions_measure_from_edges() {
        assert_eq!(offset(&[k("right"), px(10.0)], 100.0, 20.0), 70.0);
        assert_eq!(offset(&[Value::Percentage(50.0)], 100.0, 20.0), 40.0);
        assert_eq!(offset(&[k("center")], 100.0, 20.0), 40.0);
    }
}
