//! Building the paint list from the fragment tree in the painting order
//! of CSS 2 Appendix E:
//!
//! 1. the canvas background (the root element's, or the `body`'s when
//!    the root has none: css-backgrounds-3 §2.11.2);
//! 2. for each stacking context: its root's background and borders, the
//!    child contexts with negative `z-index`, the backgrounds and borders
//!    of its in-flow block-level descendants in tree order, its floats
//!    (each painted atomically), its inline content (line boxes with
//!    inline backgrounds, text and decorations, atomic inlines painted
//!    atomically, replaced content), the positioned descendants with
//!    `z-index: auto` or 0 and the contexts with z-index 0 in tree order,
//!    the contexts with positive `z-index`, and last the outlines;
//!    group opacity wraps a context in a layer.
//!
//! Each item gets the clips of its containing block chain: overflow
//! clips (rounded by `border-radius` when both axes clip), CSS 2 `clip`
//! on absolutely positioned boxes (an absolutely positioned box escapes
//! the clips of the boxes between it and its containing block), and the
//! rounded clips of backgrounds and replaced content. Box geometry is
//! snapped to whole pixels like the reference browsers; glyphs keep
//! their subpixel positions.

use std::collections::HashMap;
use std::rc::Rc;

use vgfx::image::Image;
use vgfx::raster::FillRule;

use super::background::{self, BoxArea, LayerImage};
use super::border::{self, BorderImage, ImageRepeat, LineStyle, Side, snap_rect, snap_width};
use super::color::{self, Rgba};
use super::gradient::Gradient;
use super::list::{ClipChain, DisplayList, Item, Op, RRect, Radii, Tiling, no_radii, push_clip};
use crate::css::style::ComputedStyle;
use crate::css::values::Value;
use crate::dom::NodeId;
use crate::layout::fragment::{
    Fragment, FragmentKind, FragmentTree, Rect, ReplacedKind, Sides, Stacking, TextFragment,
};
use crate::layout::values::{self, Position, kw, px, value_lp};

/// What painting needs besides the fragment tree: decoded images.
pub trait Resources {
    /// The image of a CSS `url()` as written in a computed value.
    fn css_image(&self, url: &str) -> Option<Rc<Image>>;
    /// The image an element shows (`<img>`).
    fn element_image(&self, node: NodeId) -> Option<Rc<Image>>;
}

/// No images at all.
pub struct NoResources;

impl Resources for NoResources {
    fn css_image(&self, _: &str) -> Option<Rc<Image>> {
        None
    }

    fn element_image(&self, _: NodeId) -> Option<Rc<Image>> {
        None
    }
}

/// A text decoration in effect (from a decorating box).
#[derive(Clone, Debug)]
struct Decoration {
    underline: bool,
    overline: bool,
    through: bool,
    color: Rgba,
    style: String,
    thickness: Option<f64>,
    offset: Option<f64>,
}

/// A parsed `box-shadow` or `text-shadow`.
#[derive(Clone, Copy, Debug)]
struct Shadow {
    x: f64,
    y: f64,
    blur: f64,
    spread: f64,
    color: Rgba,
    inset: bool,
}

type Key = *const Fragment;

struct Builder<'a> {
    res: &'a dyn Resources,
    items: Vec<Item>,
    /// Per fragment: the clips of its own painting and of its children.
    clips: HashMap<Key, (ClipChain, ClipChain)>,
    viewport: Rect,
    /// The root element, whose background is the canvas's.
    root: Option<NodeId>,
    /// The `body` when its background went to the canvas.
    body_propagated: Option<NodeId>,
    decorations: Vec<Decoration>,
}

/// Builds the paint list of a laid out viewport; `body` is the
/// document's `body` element (HTML documents), whose background goes to
/// the canvas when the root element has none.
pub fn build(tree: &FragmentTree, res: &dyn Resources, body: Option<NodeId>) -> DisplayList {
    let vp = &tree.root;
    let mut b = Builder {
        res,
        items: Vec::new(),
        clips: HashMap::new(),
        viewport: vp.rect,
        root: None,
        body_propagated: None,
        decorations: Vec::new(),
    };
    b.assign_clips(vp, &None, &None, &None);
    b.canvas(vp, body);
    b.context(vp);
    DisplayList {
        width: vp.rect.width.max(0.0).round() as u32,
        height: vp.rect.height.max(0.0).round() as u32,
        items: b.items,
    }
}

// ----- reading styles ----------------------------------------------------------------------

fn is_box(f: &Fragment) -> bool {
    matches!(
        f.kind,
        FragmentKind::Block { .. }
            | FragmentKind::Float
            | FragmentKind::InlineBlock
            | FragmentKind::Replaced(_)
            | FragmentKind::Inline { .. }
            | FragmentKind::Marker
            | FragmentKind::OutOfFlow
    )
}

fn layered(f: &Fragment) -> bool {
    !matches!(f.stacking, Stacking::None)
}

fn opacity(style: &ComputedStyle) -> f32 {
    match style.get("opacity").and_then(|v| v.values.as_deref()) {
        Some([Value::Number(n)]) => n.clamp(0.0, 1.0) as f32,
        Some([Value::Percentage(p)]) => (p / 100.0).clamp(0.0, 1.0) as f32,
        _ => 1.0,
    }
}

/// The comma-separated items of a computed value.
fn comma_items(style: &ComputedStyle, name: &str) -> Vec<Vec<Value>> {
    let Some(values) = style.get(name).and_then(|v| v.values.clone()) else {
        return Vec::new();
    };
    let mut out = vec![Vec::new()];
    for v in values {
        if v == Value::Comma {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(v);
        }
    }
    out.retain(|i| !i.is_empty());
    out
}

fn shadows(style: &ComputedStyle, name: &str) -> Vec<Shadow> {
    let mut out = Vec::new();
    for item in comma_items(style, name) {
        if item.iter().any(|v| v.keyword() == Some("none")) {
            continue;
        }
        let mut lengths = Vec::new();
        let mut color = None;
        let mut inset = false;
        for v in &item {
            match v {
                Value::Keyword(k) if k == "inset" => inset = true,
                Value::Color(c) => color = Some(color::resolve(c, style)),
                v => {
                    if let Some(l) = value_lp(v) {
                        lengths.push(l.resolve(0.0));
                    }
                }
            }
        }
        if lengths.len() < 2 {
            continue;
        }
        out.push(Shadow {
            x: lengths[0],
            y: lengths[1],
            blur: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: lengths.get(3).copied().unwrap_or(0.0),
            color: color.unwrap_or_else(|| color::rgba(&style.color)),
            inset,
        });
    }
    out
}

/// Expands one to four values to (top, right, bottom, left).
fn four<T: Clone>(v: &[T]) -> Option<[T; 4]> {
    Some(match v {
        [a] => [a.clone(), a.clone(), a.clone(), a.clone()],
        [a, b] => [a.clone(), b.clone(), a.clone(), b.clone()],
        [a, b, c] => [a.clone(), b.clone(), c.clone(), b.clone()],
        [a, b, c, d, ..] => [a.clone(), b.clone(), c.clone(), d.clone()],
        [] => return None,
    })
}

/// The corner radii of a box with border box `rect`, scaled to fit.
fn radii(style: &ComputedStyle, rect: &Rect) -> Radii {
    let names = [
        "border-top-left-radius",
        "border-top-right-radius",
        "border-bottom-right-radius",
        "border-bottom-left-radius",
    ];
    let r = names.map(|n| {
        let vals: Vec<_> = style
            .get(n)
            .and_then(|v| v.values.as_deref())
            .unwrap_or(&[])
            .iter()
            .filter_map(value_lp)
            .collect();
        match vals.as_slice() {
            [a] => (a.resolve(rect.width), a.resolve(rect.height)),
            [a, b, ..] => (a.resolve(rect.width), b.resolve(rect.height)),
            [] => (0.0, 0.0),
        }
    });
    if no_radii(&r) {
        return [(0.0, 0.0); 4];
    }
    border::fit(&r, rect)
}

/// A length in pixels from a serialized length (`10px`, `1.5em`).
fn parse_length(s: &str, style: &ComputedStyle) -> Option<f64> {
    let s = s.trim();
    let split = s
        .find(|c: char| c.is_ascii_alphabetic() || c == '%')
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    match unit.to_ascii_lowercase().as_str() {
        "" | "px" => Some(n),
        "em" => Some(n * style.font_size),
        u => crate::css::values::to_canonical(n, u)
            .filter(|(_, c)| *c == "px")
            .map(|(v, _)| v),
    }
}

/// The CSS 2 `clip` rectangle of an absolutely positioned box with
/// border box `bb`.
fn clip_rect(style: &ComputedStyle, bb: &Rect) -> Option<Rect> {
    let text = style.text("clip").to_ascii_lowercase();
    let inner = text.strip_prefix("rect(")?.strip_suffix(')')?;
    let parts: Vec<&str> = inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 4 {
        return None;
    }
    let v = |i: usize, auto: f64| {
        if parts[i] == "auto" {
            Some(auto)
        } else {
            parse_length(parts[i], style)
        }
    };
    let top = v(0, 0.0)?;
    let right = v(1, bb.width)?;
    let bottom = v(2, bb.height)?;
    let left = v(3, 0.0)?;
    Some(Rect::new(
        bb.x + left,
        bb.y + top,
        (right - left).max(0.0),
        (bottom - top).max(0.0),
    ))
}

fn decoration(style: &ComputedStyle) -> Option<Decoration> {
    let line = kw(style, "text-decoration-line");
    let has = |w: &str| line.split_ascii_whitespace().any(|x| x == w);
    let (underline, overline, through) = (has("underline"), has("overline"), has("line-through"));
    if !(underline || overline || through) {
        return None;
    }
    let length = |name: &str| match style.get(name).and_then(|v| v.values.as_deref()) {
        Some([Value::Keyword(_)]) | None => None,
        Some([v]) => value_lp(v).map(|l| l.resolve(style.font_size)),
        _ => None,
    };
    Some(Decoration {
        underline,
        overline,
        through,
        color: color::of(style, "text-decoration-color"),
        style: kw(style, "text-decoration-style"),
        thickness: length("text-decoration-thickness"),
        offset: length("text-underline-offset"),
    })
}

fn pixelated(style: &ComputedStyle) -> bool {
    matches!(
        kw(style, "image-rendering").as_str(),
        "pixelated" | "crisp-edges" | "-moz-crisp-edges"
    )
}

/// The shapes of a box: border, padding and content edges.
#[derive(Clone, Copy, Debug)]
struct Shapes {
    border: RRect,
    padding: RRect,
    content: RRect,
    widths: (f64, f64, f64, f64),
}

impl Shapes {
    fn of(&self, a: BoxArea) -> RRect {
        match a {
            BoxArea::Border => self.border,
            BoxArea::Padding => self.padding,
            BoxArea::Content => self.content,
        }
    }
}

fn inset_shape(s: &RRect, d: (f64, f64, f64, f64)) -> RRect {
    RRect {
        rect: Rect::new(
            s.rect.x + d.3,
            s.rect.y + d.0,
            (s.rect.width - d.1 - d.3).max(0.0),
            (s.rect.height - d.0 - d.2).max(0.0),
        ),
        radii: border::inset(&s.radii, d),
    }
}

fn shapes(f: &Fragment) -> Shapes {
    let bb = snap_rect(f.rect);
    let w = (
        snap_width(f.border.top),
        snap_width(f.border.right),
        snap_width(f.border.bottom),
        snap_width(f.border.left),
    );
    let border = RRect {
        rect: bb,
        radii: radii(&f.style, &bb),
    };
    let padding = inset_shape(&border, w);
    let p = &f.padding;
    let content = inset_shape(
        &padding,
        (
            p.top.round(),
            p.right.round(),
            p.bottom.round(),
            p.left.round(),
        ),
    );
    Shapes {
        border,
        padding,
        content,
        widths: w,
    }
}

fn outset(r: &Rect, d: f64) -> Rect {
    r.outset(&Sides::new(d, d, d, d))
}

/// The radius of a shadow's corner for spread `s` (css-backgrounds-3
/// §7.2: radii grow by the spread, less so when they are small).
fn spread_radius(r: f64, s: f64) -> f64 {
    if r <= 0.0 {
        return 0.0;
    }
    if s > 0.0 && r < s {
        let ratio = r / s;
        return r + s * (1.0 + (ratio - 1.0).powi(3));
    }
    (r + s).max(0.0)
}

// ----- the builder -------------------------------------------------------------------------

impl Builder<'_> {
    fn emit(&mut self, clip: &ClipChain, op: Op) {
        self.items.push(Item {
            clip: clip.clone(),
            op,
        });
    }

    fn own_clip(&self, f: &Fragment) -> ClipChain {
        self.clips.get(&(f as Key)).and_then(|c| c.0.clone())
    }

    /// Computes the clip chains of `f` and its subtree: `inherited` is
    /// the parent's content clip, `abs` and `fixed` the content clips of
    /// the containing blocks of absolutely and fixed positioned
    /// descendants.
    fn assign_clips(
        &mut self,
        f: &Fragment,
        inherited: &ClipChain,
        abs: &ClipChain,
        fixed: &ClipChain,
    ) {
        let pos = values::position(&f.style);
        let escapes = matches!(f.kind, FragmentKind::OutOfFlow | FragmentKind::Replaced(_))
            && matches!(pos, Position::Absolute | Position::Fixed);
        let mut own = if escapes {
            if pos == Position::Fixed {
                fixed.clone()
            } else {
                abs.clone()
            }
        } else {
            inherited.clone()
        };
        if escapes {
            if let Some(r) = clip_rect(&f.style, &f.rect) {
                own = push_clip(&own, RRect::square(snap_rect(r)));
            }
        }
        let mut content = own.clone();
        if let Some(c) = &f.clip {
            let big = 1.0e7;
            let (x0, x1) = c.x.unwrap_or((-big, big));
            let (y0, y1) = c.y.unwrap_or((-big, big));
            let rect = snap_rect(Rect::new(x0, y0, x1 - x0, y1 - y0));
            let radii = if c.x.is_some() && c.y.is_some() && is_box(f) {
                let s = shapes(f);
                if s.padding.rect == rect {
                    s.padding.radii
                } else {
                    [(0.0, 0.0); 4]
                }
            } else {
                [(0.0, 0.0); 4]
            };
            content = push_clip(&content, RRect { rect, radii });
        }
        let boxy = is_box(f) || matches!(f.kind, FragmentKind::Viewport);
        let next_abs = if boxy && values::contains_absolute(&f.style) {
            content.clone()
        } else {
            abs.clone()
        };
        let next_fixed = if boxy && values::contains_fixed(&f.style) {
            content.clone()
        } else {
            fixed.clone()
        };
        self.clips.insert(f as Key, (own, content.clone()));
        for c in &f.children {
            self.assign_clips(c, &content, &next_abs, &next_fixed);
        }
    }

    /// The canvas: white, then the root's background (or the body's)
    /// over the whole viewport, positioned against the root element.
    fn canvas(&mut self, vp: &Fragment, body: Option<NodeId>) {
        let Some(root) = vp.children.iter().find(|c| c.node.is_some()) else {
            return;
        };
        self.root = root.node;
        let empty = |s: &ComputedStyle| {
            color::of(s, "background-color").is_transparent()
                && background::layers(s).iter().all(|l| l.image.is_none())
        };
        let mut style = root.style.clone();
        if empty(&style) {
            if let Some(b) = body {
                if let Some(bf) = root.descendants().into_iter().find(|f| f.node == Some(b)) {
                    style = bf.style.clone();
                    self.body_propagated = Some(b);
                }
            }
        }
        let s = shapes(root);
        let viewport = self.viewport;
        self.background(&style, &s, &None, Some(viewport));
    }

    fn fill_shape(&mut self, clip: &ClipChain, s: &RRect, color: Rgba) {
        if color.is_transparent() || s.rect.width <= 0.0 || s.rect.height <= 0.0 {
            return;
        }
        if no_radii(&s.radii) {
            self.emit(
                clip,
                Op::Rect {
                    rect: s.rect,
                    color,
                },
            );
        } else {
            self.emit(
                clip,
                Op::Path {
                    path: s.path(),
                    rule: FillRule::NonZero,
                    color,
                    mask: None,
                },
            );
        }
    }

    /// Background color and layers; `painting` replaces the painting
    /// area (the canvas).
    fn background(
        &mut self,
        style: &ComputedStyle,
        s: &Shapes,
        clip: &ClipChain,
        painting: Option<Rect>,
    ) {
        let layers = background::layers(style);
        let color = color::of(style, "background-color");
        let bottom = layers.last().map_or(BoxArea::Border, |l| l.clip);
        let area_of = |a: BoxArea| painting.map_or_else(|| s.of(a), RRect::square);
        self.fill_shape(clip, &area_of(bottom), color);
        for layer in layers.iter().rev() {
            let Some(img) = &layer.image else {
                continue;
            };
            let area = area_of(layer.clip);
            if area.rect.width <= 0.0 || area.rect.height <= 0.0 {
                continue;
            }
            let positioning = if layer.fixed {
                self.viewport
            } else {
                s.of(layer.origin).rect
            };
            let layer_clip = if no_radii(&area.radii) {
                clip.clone()
            } else {
                push_clip(clip, area)
            };
            match img {
                LayerImage::Url(u) => {
                    let Some(image) = self.res.css_image(u) else {
                        continue;
                    };
                    let natural = (f64::from(image.width), f64::from(image.height));
                    if let Some(tiling) =
                        background::place(layer, Some(natural), positioning, area.rect)
                    {
                        let src = Rect::new(0.0, 0.0, natural.0, natural.1);
                        self.emit(
                            &layer_clip,
                            Op::Image {
                                image,
                                src,
                                tiling,
                                pixelated: pixelated(style),
                            },
                        );
                    }
                }
                LayerImage::Gradient(g) => {
                    if let Some(tiling) = background::place(layer, None, positioning, area.rect) {
                        let gradient =
                            Gradient::new(g, tiling.tile.width, tiling.tile.height, style);
                        self.emit(
                            &layer_clip,
                            Op::Gradient {
                                gradient: Rc::new(gradient),
                                tiling,
                            },
                        );
                    }
                }
            }
        }
    }

    /// Background, shadows and borders of a box.
    fn decorations(&mut self, f: &Fragment) {
        if !is_box(f) || !f.is_visible() || f.empty_hidden {
            return;
        }
        let clip = self.own_clip(f);
        let s = shapes(f);
        let style = f.style.clone();
        let list = shadows(&style, "box-shadow");
        for sh in list.iter().rev().filter(|s| !s.inset) {
            let grown = outset(&s.border.rect, sh.spread);
            let shape = RRect {
                rect: Rect::new(grown.x + sh.x, grown.y + sh.y, grown.width, grown.height),
                radii: s
                    .border
                    .radii
                    .map(|(x, y)| (spread_radius(x, sh.spread), spread_radius(y, sh.spread))),
            };
            if shape.rect.width > 0.0 && shape.rect.height > 0.0 && !sh.color.is_transparent() {
                self.emit(
                    &clip,
                    Op::Shadow {
                        shape,
                        box_shape: s.border,
                        blur: sh.blur,
                        color: sh.color,
                        inset: false,
                    },
                );
            }
        }
        let propagated =
            f.node.is_some() && (f.node == self.root || f.node == self.body_propagated);
        if !propagated {
            self.background(&style, &s, &clip, None);
        }
        for sh in list.iter().rev().filter(|s| s.inset) {
            let shrunk = inset_shape(&s.padding, (sh.spread, sh.spread, sh.spread, sh.spread));
            let shape = RRect {
                rect: Rect::new(
                    shrunk.rect.x + sh.x,
                    shrunk.rect.y + sh.y,
                    shrunk.rect.width,
                    shrunk.rect.height,
                ),
                radii: shrunk.radii,
            };
            if !sh.color.is_transparent() {
                self.emit(
                    &clip,
                    Op::Shadow {
                        shape,
                        box_shape: s.padding,
                        blur: sh.blur,
                        color: sh.color,
                        inset: true,
                    },
                );
            }
        }
        if f.collapsed_borders.is_some() {
            return;
        }
        if self.border_image(f, &s, &clip) {
            return;
        }
        let side = |name: &str, width: f64| Side {
            width,
            style: LineStyle::parse(&kw(&style, &format!("border-{name}-style"))),
            color: color::of(&style, &format!("border-{name}-color")),
        };
        let sides = [
            side("top", s.widths.0),
            side("right", s.widths.1),
            side("bottom", s.widths.2),
            side("left", s.widths.3),
        ];
        let mut ops = Vec::new();
        border::draw(&s.border, &sides, &mut ops);
        for op in ops {
            self.emit(&clip, op);
        }
    }

    /// Draws `border-image` instead of the border styles when it has an
    /// image that can be displayed.
    fn border_image(&mut self, f: &Fragment, s: &Shapes, clip: &ClipChain) -> bool {
        let style = &f.style;
        let source = match style
            .get("border-image-source")
            .and_then(|v| v.values.as_deref())
        {
            Some([v @ (Value::Url(_) | Value::Image(_))]) => v.clone(),
            _ => return false,
        };
        let w = s.widths;
        let bw = [w.0, w.1, w.2, w.3];
        let list = |name: &str| -> Vec<Value> {
            style
                .get(name)
                .and_then(|v| v.values.clone())
                .unwrap_or_default()
        };
        let outsets = four(&list("border-image-outset")).unwrap_or_else(|| {
            let z = Value::Number(0.0);
            [z.clone(), z.clone(), z.clone(), z]
        });
        let out_of = |i: usize| match &outsets[i] {
            Value::Number(n) | Value::Integer(n) => n * bw[i],
            v => value_lp(v).map_or(0.0, |l| l.resolve(0.0)),
        };
        let bb = s.border.rect;
        let area = Rect::new(
            bb.x - out_of(3),
            bb.y - out_of(0),
            bb.width + out_of(1) + out_of(3),
            bb.height + out_of(0) + out_of(2),
        );
        let image = match &source {
            Value::Url(u) => match self.res.css_image(u) {
                Some(i) => i,
                None => return false,
            },
            Value::Image(g) => {
                if area.width <= 0.0 || area.height <= 0.0 {
                    return true;
                }
                Rc::new(Gradient::new(g, area.width, area.height, style).rasterize())
            }
            _ => return false,
        };
        let (iw, ih) = (f64::from(image.width), f64::from(image.height));
        let slice_values = list("border-image-slice");
        let fill = slice_values.iter().any(|v| v.keyword() == Some("fill"));
        let nums: Vec<Value> = slice_values
            .into_iter()
            .filter(|v| v.keyword().is_none())
            .collect();
        let slices = four(&nums).unwrap_or_else(|| {
            let p = Value::Percentage(100.0);
            [p.clone(), p.clone(), p.clone(), p]
        });
        let slice = std::array::from_fn(|i| {
            let basis = if i % 2 == 0 { ih } else { iw };
            match &slices[i] {
                Value::Number(n) | Value::Integer(n) => *n,
                Value::Percentage(p) => basis * p / 100.0,
                _ => 0.0,
            }
        });
        let widths_values = four(&list("border-image-width")).unwrap_or_else(|| {
            let one = Value::Number(1.0);
            [one.clone(), one.clone(), one.clone(), one]
        });
        let widths = std::array::from_fn(|i| {
            let basis = if i % 2 == 0 { area.height } else { area.width };
            match &widths_values[i] {
                Value::Number(n) | Value::Integer(n) => n * bw[i],
                Value::Keyword(k) if k == "auto" => slice[i],
                v => value_lp(v).map_or(bw[i], |l| l.resolve(basis)),
            }
        });
        let reps: Vec<ImageRepeat> = list("border-image-repeat")
            .iter()
            .filter_map(Value::keyword)
            .map(|k| match k {
                "repeat" => ImageRepeat::Repeat,
                "round" => ImageRepeat::Round,
                "space" => ImageRepeat::Space,
                _ => ImageRepeat::Stretch,
            })
            .collect();
        let repeat = match reps.as_slice() {
            [a] => (*a, *a),
            [a, b, ..] => (*a, *b),
            [] => (ImageRepeat::Stretch, ImageRepeat::Stretch),
        };
        let mut ops = Vec::new();
        border::draw_image(
            &BorderImage {
                image,
                slice,
                fill,
                widths,
                area,
                repeat,
            },
            &mut ops,
        );
        for op in ops {
            self.emit(clip, op);
        }
        true
    }

    /// The borders a table box and its cells paint in the collapsing
    /// border model, after all their backgrounds.
    fn collapsed_borders(&mut self, table: &Fragment) {
        let mut stack = vec![table];
        while let Some(f) = stack.pop() {
            if let Some(sides) = &f.collapsed_borders {
                if f.is_visible() {
                    let clip = self.own_clip(f);
                    let r = f.rect;
                    let w: Vec<f64> = sides.iter().map(|s| snap_width(s.width)).collect();
                    let bands = [
                        Rect::new(
                            r.x - w[3] / 2.0,
                            r.y - w[0] / 2.0,
                            r.width + (w[1] + w[3]) / 2.0,
                            w[0],
                        ),
                        Rect::new(
                            r.right() - w[1] / 2.0,
                            r.y - w[0] / 2.0,
                            w[1],
                            r.height + (w[0] + w[2]) / 2.0,
                        ),
                        Rect::new(
                            r.x - w[3] / 2.0,
                            r.bottom() - w[2] / 2.0,
                            r.width + (w[1] + w[3]) / 2.0,
                            w[2],
                        ),
                        Rect::new(
                            r.x - w[3] / 2.0,
                            r.y - w[0] / 2.0,
                            w[3],
                            r.height + (w[0] + w[2]) / 2.0,
                        ),
                    ];
                    for (k, side) in sides.iter().enumerate() {
                        let Some(src) = &side.source else {
                            continue;
                        };
                        let name = side.side.name();
                        let st = LineStyle::parse(&kw(src, &format!("border-{name}-style")));
                        if !st.visible() || w[k] <= 0.0 {
                            continue;
                        }
                        let c = color::of(src, &format!("border-{name}-color"));
                        if c.is_transparent() {
                            continue;
                        }
                        let rect = snap_rect(bands[k]);
                        self.emit(&clip, Op::Rect { rect, color: c });
                    }
                }
            }
            for c in f.children.iter().rev() {
                if !is_table_box(c) {
                    stack.push(c);
                }
            }
        }
    }

    // ----- Appendix E ------------------------------------------------------------------

    /// Paints a stacking context rooted at `r`.
    fn context(&mut self, r: &Fragment) {
        let op = opacity(&r.style);
        let layer = op < 1.0 && !matches!(r.kind, FragmentKind::Viewport);
        if layer {
            self.items.push(Item {
                clip: None,
                op: Op::PushLayer(op),
            });
        }
        let saved = std::mem::take(&mut self.decorations);
        if !matches!(r.kind, FragmentKind::Viewport) {
            self.decorations(r);
        }
        let (mut neg, mut zero, mut pos) = (Vec::new(), Vec::new(), Vec::new());
        collect(r, &mut neg, &mut zero, &mut pos);
        neg.sort_by_key(|f| z(f));
        pos.sort_by_key(|f| z(f));
        for f in neg {
            self.context(f);
        }
        self.layers(r);
        for f in zero {
            if matches!(f.stacking, Stacking::Context(_)) {
                self.context(f);
            } else {
                self.pseudo_context(f);
            }
        }
        for f in pos {
            self.context(f);
        }
        self.outlines(r, true);
        self.decorations = saved;
        if layer {
            self.items.push(Item {
                clip: None,
                op: Op::PopLayer,
            });
        }
    }

    /// Paints a box as if it created a stacking context, leaving its
    /// positioned descendants and child contexts to the enclosing one
    /// (positioned boxes with `z-index: auto`, floats, atomic inlines).
    fn pseudo_context(&mut self, r: &Fragment) {
        let saved = std::mem::take(&mut self.decorations);
        self.decorations(r);
        self.layers(r);
        self.decorations = saved;
    }

    /// Steps 4 to 7 of Appendix E for the descendants of `r`.
    fn layers(&mut self, r: &Fragment) {
        self.blocks(r);
        if is_table_box(r) {
            self.collapsed_borders(r);
        }
        self.floats(r);
        let pushed = self.push_decoration(r);
        if matches!(r.kind, FragmentKind::Replaced(_)) {
            self.replaced(r);
        }
        self.inlines(r);
        if pushed {
            self.decorations.pop();
        }
    }

    /// Backgrounds and borders of in-flow block-level descendants.
    fn blocks(&mut self, f: &Fragment) {
        for c in &f.children {
            if layered(c) {
                continue;
            }
            match c.kind {
                FragmentKind::Block { .. } | FragmentKind::OutOfFlow => {
                    self.decorations(c);
                    self.blocks(c);
                    if is_table_box(c) {
                        self.collapsed_borders(c);
                    }
                }
                FragmentKind::Replaced(_) if !inline_parent(f) => self.decorations(c),
                _ => {}
            }
        }
    }

    /// Floats, each painted atomically.
    fn floats(&mut self, f: &Fragment) {
        for c in &f.children {
            if layered(c) {
                continue;
            }
            match c.kind {
                FragmentKind::Float => self.pseudo_context(c),
                FragmentKind::InlineBlock | FragmentKind::Replaced(_) => {}
                _ => self.floats(c),
            }
        }
    }

    /// Inline content: inline boxes, text, atomic inlines and replaced
    /// content, in tree order.
    fn inlines(&mut self, f: &Fragment) {
        let parent_inline = matches!(
            f.kind,
            FragmentKind::Line { .. } | FragmentKind::Inline { .. }
        );
        for c in &f.children {
            if layered(c) {
                continue;
            }
            match &c.kind {
                FragmentKind::Float | FragmentKind::Placeholder => {}
                FragmentKind::Text(t) => self.text(c, t),
                FragmentKind::InlineBlock => self.pseudo_context(c),
                FragmentKind::Replaced(_) => {
                    if parent_inline {
                        self.pseudo_context(c);
                    } else {
                        self.replaced(c);
                    }
                }
                FragmentKind::Inline { .. } | FragmentKind::Marker => {
                    self.decorations(c);
                    let pushed = self.push_decoration(c);
                    self.inlines(c);
                    if pushed {
                        self.decorations.pop();
                    }
                }
                FragmentKind::Block { .. } | FragmentKind::OutOfFlow => {
                    let pushed = self.push_decoration(c);
                    self.inlines(c);
                    if pushed {
                        self.decorations.pop();
                    }
                }
                _ => self.inlines(c),
            }
        }
    }

    fn push_decoration(&mut self, f: &Fragment) -> bool {
        if !is_box(f) {
            return false;
        }
        match decoration(&f.style) {
            Some(d) => {
                self.decorations.push(d);
                true
            }
            None => false,
        }
    }

    /// The outlines of the boxes painted in the context of `r`.
    fn outlines(&mut self, f: &Fragment, root: bool) {
        if root || !matches!(f.stacking, Stacking::Context(_)) {
            if is_box(f) {
                self.outline(f);
            }
            for c in &f.children {
                self.outlines(c, false);
            }
        }
    }

    fn outline(&mut self, f: &Fragment) {
        let style = &f.style;
        let ls = LineStyle::parse(&kw(style, "outline-style"));
        if !ls.visible() || !f.is_visible() {
            return;
        }
        let width = snap_width(px(style, "outline-width"));
        if width <= 0.0 {
            return;
        }
        let offset = px(style, "outline-offset").round();
        let color = match style.get("outline-color").and_then(|v| v.values.as_deref()) {
            Some([Value::Color(c)]) => color::resolve(c, style),
            _ => color::rgba(&style.color),
        };
        let bb = snap_rect(f.rect);
        let grow = offset + width;
        let rect = outset(&bb, grow);
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let r = radii(style, &bb).map(|(x, y)| {
            if x > 0.0 && y > 0.0 {
                ((x + grow).max(0.0), (y + grow).max(0.0))
            } else {
                (0.0, 0.0)
            }
        });
        let side = Side {
            width,
            style: ls,
            color,
        };
        let mut ops = Vec::new();
        border::draw(&RRect { rect, radii: r }, &[side; 4], &mut ops);
        let clip = self.own_clip(f);
        for op in ops {
            self.emit(&clip, op);
        }
    }

    /// The image of a replaced element, in its `object-fit` box and
    /// clipped to its content box.
    fn replaced(&mut self, f: &Fragment) {
        if !f.is_visible() || !matches!(f.kind, FragmentKind::Replaced(ReplacedKind::Image)) {
            return;
        }
        let Some(image) = f.node.and_then(|n| self.res.element_image(n)) else {
            return;
        };
        let s = shapes(f);
        let dest = f.object.unwrap_or_else(|| f.content_rect());
        let tile = Rect::new(dest.x.round(), dest.y.round(), dest.width, dest.height);
        let mut clip = self.own_clip(f);
        if !no_radii(&s.content.radii) {
            clip = push_clip(&clip, s.content);
        }
        let src = Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height));
        self.emit(
            &clip,
            Op::Image {
                image,
                src,
                tiling: Tiling {
                    tile,
                    gap: (0.0, 0.0),
                    repeat: (false, false),
                    area: s.content.rect,
                },
                pixelated: pixelated(&f.style),
            },
        );
    }

    /// A run of text: shadows, underlines and overlines, the glyphs,
    /// then line-throughs (css-text-decor-3 §8).
    fn text(&mut self, f: &Fragment, t: &TextFragment) {
        if !f.is_visible() {
            return;
        }
        let Some(face) = t.face.clone() else {
            return;
        };
        let clip = self.own_clip(f);
        let color = color::rgba(&f.style.color);
        let glyphs: Vec<(u16, f64, f64)> = t
            .glyphs
            .iter()
            .map(|g| (g.id, g.x, t.baseline + g.y))
            .collect();
        for sh in shadows(&f.style, "text-shadow").iter().rev() {
            if sh.color.is_transparent() {
                continue;
            }
            let moved = glyphs
                .iter()
                .map(|&(id, x, y)| (id, x + sh.x, y + sh.y))
                .collect();
            self.emit(
                &clip,
                Op::Glyphs {
                    face: face.clone(),
                    size: t.font_size,
                    glyphs: moved,
                    color: sh.color,
                    blur: sh.blur,
                },
            );
        }
        let m = face.metrics().scaled(t.font_size as f32);
        let size = t.font_size;
        let (x0, x1) = (f.rect.x, f.rect.right());
        let decos = self.decorations.clone();
        let line = |d: &Decoration, top: f64, thick: f64, out: &mut Vec<Op>| {
            decoration_line(d, x0, x1, top, thick, out);
        };
        let mut before = Vec::new();
        let mut after = Vec::new();
        for d in &decos {
            let font_thick = if m.underline_thickness > 0.0 {
                f64::from(m.underline_thickness)
            } else {
                size / 14.0
            };
            let thick = d.thickness.unwrap_or(font_thick).max(1.0).round();
            if d.underline {
                let pos = if m.underline_position != 0.0 {
                    -f64::from(m.underline_position)
                } else {
                    size / 10.0
                };
                let top = match d.offset {
                    Some(o) => (t.baseline + o).round(),
                    None => (t.baseline + pos - thick / 2.0).round(),
                };
                line(d, top, thick, &mut before);
            }
            if d.overline {
                line(d, (t.baseline - t.ascent).round(), thick, &mut before);
            }
            if d.through {
                let st = if m.strikeout_size > 0.0 {
                    f64::from(m.strikeout_size)
                } else {
                    font_thick
                };
                let st = d.thickness.unwrap_or(st).max(1.0).round();
                let center = if m.strikeout_position > 0.0 {
                    f64::from(m.strikeout_position)
                } else {
                    t.ascent * 0.3
                };
                line(d, (t.baseline - center - st / 2.0).round(), st, &mut after);
            }
        }
        for op in before {
            self.emit(&clip, op);
        }
        if !color.is_transparent() {
            self.emit(
                &clip,
                Op::Glyphs {
                    face,
                    size: t.font_size,
                    glyphs,
                    color,
                    blur: 0.0,
                },
            );
        }
        for op in after {
            self.emit(&clip, op);
        }
    }
}

/// One decoration line from `x0` to `x1` with its top at `top`.
fn decoration_line(d: &Decoration, x0: f64, x1: f64, top: f64, t: f64, out: &mut Vec<Op>) {
    if d.color.is_transparent() || x1 <= x0 {
        return;
    }
    let color = d.color;
    let mut rect = |x: f64, y: f64, w: f64, h: f64| {
        out.push(Op::Rect {
            rect: Rect::new(x, y, w, h),
            color,
        });
    };
    match d.style.as_str() {
        "double" => {
            rect(x0, top, x1 - x0, t);
            rect(x0, top + 2.0 * t, x1 - x0, t);
        }
        "dotted" | "dashed" => {
            let (on, off) = if d.style == "dotted" {
                (t, t)
            } else {
                (3.0 * t, 3.0 * t)
            };
            let mut x = x0;
            while x < x1 {
                rect(x, top, on.min(x1 - x), t);
                x += on + off;
            }
        }
        "wavy" => {
            let amp = t.max(1.0);
            let step = 2.0 * amp;
            let mut upper = Vec::new();
            let mut x = x0;
            let mut k = 0;
            while x <= x1 + step {
                let y = top + if k % 2 == 0 { 0.0 } else { 2.0 * amp };
                upper.push((x.min(x1), y));
                x += step;
                k += 1;
            }
            let mut pts: Vec<(f32, f32)> =
                upper.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
            pts.extend(upper.iter().rev().map(|&(x, y)| (x as f32, (y + t) as f32)));
            out.push(Op::Path {
                path: vgfx::path::polygon(&pts),
                rule: FillRule::NonZero,
                color,
                mask: None,
            });
        }
        _ => rect(x0, top, x1 - x0, t),
    }
}

fn z(f: &Fragment) -> i32 {
    match f.stacking {
        Stacking::Context(z) => z,
        _ => 0,
    }
}

/// Splits the layered descendants of a context root into the negative,
/// zero (and `auto`) and positive lists, in tree order.
fn collect<'a>(
    f: &'a Fragment,
    neg: &mut Vec<&'a Fragment>,
    zero: &mut Vec<&'a Fragment>,
    pos: &mut Vec<&'a Fragment>,
) {
    for c in &f.children {
        match c.stacking {
            Stacking::Context(z) => {
                if z < 0 {
                    neg.push(c);
                } else if z > 0 {
                    pos.push(c);
                } else {
                    zero.push(c);
                }
            }
            Stacking::Positioned => {
                zero.push(c);
                collect(c, neg, zero, pos);
            }
            Stacking::None => collect(c, neg, zero, pos),
        }
    }
}

fn inline_parent(f: &Fragment) -> bool {
    matches!(
        f.kind,
        FragmentKind::Line { .. } | FragmentKind::Inline { .. }
    )
}

/// The table box of a table (not its wrapper, which has a child of the
/// same element).
fn is_table_box(f: &Fragment) -> bool {
    matches!(kw(&f.style, "display").as_str(), "table" | "inline-table")
        && f.node.is_some()
        && !f
            .children
            .iter()
            .any(|c| c.node == f.node && c.pseudo == f.pseudo)
}
