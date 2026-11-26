//! Layout (`vault/CONTRACT.md` §3.5): the box tree of a styled document
//! and one pass of block and inline formatting (CSS 2 §9, §10; css-text-3;
//! css-inline-3) that produces an immutable fragment tree in absolute
//! coordinates. Only `writing-mode: horizontal-tb` is laid out. Tables,
//! flex and grid containers are laid out as block flow roots until their
//! own layout modes exist, and absolutely positioned boxes stay at their
//! static position.

pub(crate) mod bidi;
mod block;
mod boxtree;
mod content;
mod float;
pub mod fragment;
mod inline;
pub(crate) mod linebreak;
mod text;
mod values;

use vgfx::font::system::WebFace;

use crate::css::style::{StyleMap, initial_style};
use crate::dom::{Document, NodeId};

use fragment::{Fragment, FragmentKind, FragmentTree, Rect};

/// What one layout pass shares.
pub(crate) struct Ctx {
    pub fonts: text::FontContext,
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
    };
    let (w, h) = options.viewport;
    let mut viewport = Fragment::new(FragmentKind::Viewport, initial_style());
    viewport.rect = Rect::new(0.0, 0.0, w, h);
    let mut builder = boxtree::Builder::new(doc, styles, options.images);
    if let Some(root) = builder.build() {
        viewport
            .children
            .push(block::layout_root(&mut ctx, &root, options.viewport));
    }
    // Every fragment is relative to its parent until here; the viewport
    // is at the origin.
    viewport.absolutize(0.0, 0.0);
    FragmentTree { root: viewport }
}
