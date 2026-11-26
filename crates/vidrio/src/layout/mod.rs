//! Layout (`vault/CONTRACT.md` §3.5): the box tree of a styled document
//! and one pass of block and inline formatting (CSS 2 §9, §10; css-text-3;
//! css-inline-3) that produces an immutable fragment tree in absolute
//! coordinates, flex layout (css-flexbox-1), grid layout (css-grid-2),
//! table layout (css-tables-3, CSS 2 §17), then positioned layout
//! (relative, sticky, absolute and fixed boxes), overflow clips and
//! scroll containers, and stacking. Only
//! `writing-mode: horizontal-tb` is laid out; logical margins, paddings
//! and insets arrive mapped to physical ones by the cascade.

pub(crate) mod bidi;
mod block;
mod boxtree;
mod content;
mod flex;
mod float;
pub mod fragment;
mod grid;
mod inline;
pub(crate) mod linebreak;
mod positioned;
mod table;
mod text;
mod values;

use std::collections::HashMap;

use vgfx::font::system::WebFace;

use crate::css::style::{StyleMap, initial_style};
use crate::dom::{Document, NodeId};

use fragment::{Fragment, FragmentKind, FragmentTree, Overflow, Rect};

/// What one layout pass shares.
pub(crate) struct Ctx {
    pub fonts: text::FontContext,
    /// The tracks a grid passes to a subgrid item before laying it out,
    /// by the address of the item's box.
    pub subgrids: HashMap<usize, grid::Inherit>,
    /// The resolved `grid-template-columns` and `-rows` of every grid
    /// container laid out (css-grid-2 §7.2.6).
    pub grid_tracks: HashMap<NodeId, [String; 2]>,
}

/// The inputs of a layout pass besides the document and its styles.
pub struct Options<'a> {
    /// The size of the initial containing block (800×600 by default,
    /// §3.5).
    pub viewport: (f64, f64),
    /// The loaded web fonts of the document, searched before the system
    /// faces.
    pub web_fonts: Vec<WebFace>,
    /// Whether the system fonts (D-15) are used too.
    pub system_fonts: bool,
    /// The natural size of an image element, when it has one.
    pub images: &'a dyn Fn(NodeId) -> Option<(f64, f64)>,
}

/// Lays out `doc` with its computed `styles`.
pub fn layout(doc: &Document, styles: &StyleMap, options: Options) -> FragmentTree {
    let mut ctx = Ctx {
        fonts: text::FontContext::new(options.web_fonts, options.system_fonts),
        subgrids: HashMap::new(),
        grid_tracks: HashMap::new(),
    };
    let (w, h) = options.viewport;
    let mut viewport = Fragment::new(FragmentKind::Viewport, initial_style());
    viewport.rect = Rect::new(0.0, 0.0, w, h);
    let mut builder = boxtree::Builder::new(doc, styles, options.images);
    let root = builder.build();
    if let Some(root) = &root {
        viewport
            .children
            .push(block::layout_root(&mut ctx, root, options.viewport));
    }
    // Every fragment is relative to its parent until here; the viewport
    // is at the origin.
    viewport.absolutize(0.0, 0.0);
    if let Some(root) = &root {
        // The viewport takes the root's `overflow`, or the body's when
        // the root's is `visible` (css-overflow-3 §3.3); `visible` is
        // `auto` there and `clip` is `hidden`.
        let mut propagated = Vec::new();
        let mut source = None;
        if let Some(r) = doc.document_element() {
            propagated.push(r);
            source = styles.get(r);
        }
        if builder.body_overflow_propagated {
            if let Some(b) = builder.body {
                propagated.push(b);
                source = styles.get(b);
            }
        }
        let used = |o: Overflow| match o {
            Overflow::Visible => Overflow::Auto,
            Overflow::Clip => Overflow::Hidden,
            o => o,
        };
        let (ox, oy) = source.map_or((Overflow::Visible, Overflow::Visible), |s| {
            values::overflow(s)
        });
        let view = positioned::Viewport {
            overflow: (used(ox), used(oy)),
            propagated,
        };
        positioned::place(&mut ctx, root, &view, &mut viewport);
    }
    FragmentTree {
        root: viewport,
        grid_tracks: ctx.grid_tracks,
    }
}
