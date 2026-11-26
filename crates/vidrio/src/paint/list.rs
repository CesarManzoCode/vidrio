//! The paint list (`vault/CONTRACT.md` §3.5): a flat sequence of drawing
//! operations in device pixels, already in the painting order of CSS 2
//! Appendix E. Each item carries the chain of clips that applies to it
//! (overflow clips, `clip`, rounded background and content clips);
//! group opacity opens and closes layers around a stacking context.

use std::rc::Rc;
use std::sync::Arc;

use vgfx::font::Face;
use vgfx::image::Image;
use vgfx::outline::Outline;
use vgfx::raster::FillRule;

use super::color::Rgba;
use super::gradient::Gradient;
use crate::layout::fragment::Rect;

/// Corner radii (top left, top right, bottom right, bottom left), each
/// (horizontal, vertical), in pixels.
pub type Radii = [(f64, f64); 4];

pub fn no_radii(r: &Radii) -> bool {
    r.iter().all(|&(x, y)| x <= 0.0 || y <= 0.0)
}

/// A rectangle with rounded corners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RRect {
    pub rect: Rect,
    pub radii: Radii,
}

impl RRect {
    pub fn square(rect: Rect) -> RRect {
        RRect {
            rect,
            radii: [(0.0, 0.0); 4],
        }
    }

    /// The outline of the shape.
    pub fn path(&self) -> Outline {
        let r = &self.rect;
        let radii = self.radii.map(|(x, y)| (x as f32, y as f32));
        vgfx::path::rounded_rect(
            r.x as f32,
            r.y as f32,
            r.right() as f32,
            r.bottom() as f32,
            &radii,
        )
    }
}

/// A clip shape: a rectangle (infinite sides for an axis that is not
/// clipped) with optional rounded corners.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipNode {
    pub parent: Option<Rc<ClipNode>>,
    pub shape: RRect,
}

/// A chain of clips (innermost last); `None` clips nothing.
pub type ClipChain = Option<Rc<ClipNode>>;

/// Adds a clip to a chain.
pub fn push_clip(chain: &ClipChain, shape: RRect) -> ClipChain {
    Some(Rc::new(ClipNode {
        parent: chain.clone(),
        shape,
    }))
}

/// How an image is tiled over an area.
#[derive(Clone, Debug, PartialEq)]
pub struct Tiling {
    /// One tile (the others are offset by its size plus `gap`).
    pub tile: Rect,
    pub gap: (f64, f64),
    pub repeat: (bool, bool),
    /// The area painted (tiles are clipped to it).
    pub area: Rect,
}

/// A drawing operation.
#[derive(Clone, Debug)]
pub enum Op {
    /// Starts a group composited with an opacity when it ends.
    PushLayer(f32),
    PopLayer,
    /// A solid rectangle; pixel-aligned edges are exact.
    Rect {
        rect: Rect,
        color: Rgba,
    },
    /// A filled path, optionally restricted to the inside (non-zero) of
    /// `mask`.
    Path {
        path: Outline,
        rule: FillRule,
        color: Rgba,
        mask: Option<Outline>,
    },
    /// The source rectangle `src` (image pixels) of an image, scaled to
    /// each tile.
    Image {
        image: Rc<Image>,
        src: Rect,
        tiling: Tiling,
        pixelated: bool,
    },
    /// A gradient drawn in each tile.
    Gradient {
        gradient: Rc<Gradient>,
        tiling: Tiling,
    },
    /// Glyphs at absolute pen positions (x, baseline y), optionally
    /// blurred (text shadows).
    Glyphs {
        face: Arc<Face>,
        size: f64,
        glyphs: Vec<(u16, f64, f64)>,
        color: Rgba,
        blur: f64,
    },
    /// A box shadow: `shape` is the shadow's shape (offset and spread
    /// applied) and `box_shape` the box it belongs to; an outer shadow
    /// is painted outside `box_shape`, an inset one inside it.
    Shadow {
        shape: RRect,
        box_shape: RRect,
        blur: f64,
        color: Rgba,
        inset: bool,
    },
}

/// An operation and the clips that apply to it.
#[derive(Clone, Debug)]
pub struct Item {
    pub clip: ClipChain,
    pub op: Op,
}

/// The paint list of a viewport.
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub width: u32,
    pub height: u32,
    pub items: Vec<Item>,
}
