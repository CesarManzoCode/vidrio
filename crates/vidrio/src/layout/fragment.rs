//! The fragment tree (`vault/CONTRACT.md` §3.5): the immutable output of
//! one layout pass. Every fragment carries its border box in absolute
//! coordinates (CSS pixels, origin at the top left corner of the initial
//! containing block), its used margins, borders and paddings, the node
//! and pseudo-element that generated it and their computed style.
//! Line boxes and text runs are fragments too; text fragments hold their
//! positioned glyphs, ready to paint. Box fragments also say whether
//! they clip their content, are scroll containers (with their scrollable
//! overflow area) and how they take part in stacking (CSS 2 §9.9).

use std::rc::Rc;
use std::sync::Arc;

use vgfx::font::Face;

use crate::css::cascade::Pseudo;
use crate::css::style::ComputedStyle;
use crate::dom::NodeId;

/// A rectangle: its top left corner, width and height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    /// The rectangle with every side moved inward by `s`.
    pub fn inset(&self, s: &Sides) -> Rect {
        Rect {
            x: self.x + s.left,
            y: self.y + s.top,
            width: (self.width - s.left - s.right).max(0.0),
            height: (self.height - s.top - s.bottom).max(0.0),
        }
    }

    /// The rectangle with every side moved outward by `s`.
    pub fn outset(&self, s: &Sides) -> Rect {
        Rect {
            x: self.x - s.left,
            y: self.y - s.top,
            width: self.width + s.left + s.right,
            height: self.height + s.top + s.bottom,
        }
    }
}

/// Four physical side values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sides {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Sides {
    pub fn new(top: f64, right: f64, bottom: f64, left: f64) -> Sides {
        Sides {
            top,
            right,
            bottom,
            left,
        }
    }

    pub fn horizontal(&self) -> f64 {
        self.left + self.right
    }

    pub fn vertical(&self) -> f64 {
        self.top + self.bottom
    }

    pub fn add(&self, o: &Sides) -> Sides {
        Sides::new(
            self.top + o.top,
            self.right + o.right,
            self.bottom + o.bottom,
            self.left + o.left,
        )
    }
}

/// What a replaced fragment shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplacedKind {
    Image,
    Iframe,
    Video,
    Canvas,
    Embed,
    /// A checkbox or a radio button.
    Check,
    /// Another form control without inner text (`range`, `color`, …).
    Control,
}

/// What a fragment is.
#[derive(Clone, Debug)]
pub enum FragmentKind {
    /// The initial containing block (the viewport); the root element's
    /// fragment is its child.
    Viewport,
    /// A block-level box in normal flow (also the root element's box).
    Block { anonymous: bool, bfc_root: bool },
    /// A floating box.
    Float,
    /// An atomic inline-level box that is not replaced (`inline-block`
    /// and the form controls laid out as one).
    InlineBlock,
    /// A replaced element (block-level, inline-level or floating).
    Replaced(ReplacedKind),
    /// The part of an inline box on one line. `start` and `end` say
    /// whether the box's start and end edges (border, padding and margin)
    /// are on this fragment.
    Inline { start: bool, end: bool },
    /// A line box; `baseline` is the absolute y of its alphabetic
    /// baseline.
    Line { baseline: f64 },
    /// A run of text with one font and one direction.
    Text(TextFragment),
    /// A `::marker` box placed outside its list item.
    Marker,
    /// An absolutely or fixed positioned block container (positioned
    /// replaced elements are `Replaced`).
    OutOfFlow,
    /// The static position of an out-of-flow box before positioned
    /// layout places it: its `x` and `right()` are the left and right
    /// static positions, its `y` the top one. A finished tree has none.
    Placeholder,
}

/// A used `overflow-x` or `overflow-y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

impl Overflow {
    /// The value makes the box a scroll container.
    pub fn scrolls(self) -> bool {
        matches!(self, Overflow::Hidden | Overflow::Scroll | Overflow::Auto)
    }
}

/// A scroll container (css-overflow-3 §2.3). Scroll bars take no space
/// (their width is 0 in every mode), so the scrollport is the padding
/// box.
#[derive(Clone, Debug, PartialEq)]
pub struct Scroll {
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub scrollport: Rect,
    /// The scrollable overflow area (css-overflow-3 §2.2), at least the
    /// scrollport; it does not extend into the unreachable side (above
    /// the scrollport, and left of it in `ltr` or right of it in `rtl`).
    pub scrollable: Rect,
    /// The scroll position: the offset of the scrollport into the area
    /// from its scroll origin. Always 0, 0 until scrolling exists.
    pub offset: (f64, f64),
}

/// The overflow clip of a box: the interval each clipped axis keeps
/// (`None` for an axis that is not clipped).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clip {
    pub x: Option<(f64, f64)>,
    pub y: Option<(f64, f64)>,
}

/// How a box takes part in stacking (CSS 2 §9.9.1, Appendix E).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stacking {
    /// Painted in its parent's layers.
    #[default]
    None,
    /// Positioned with `z-index: auto`: painted as if it created a
    /// stacking context with z-index 0, without being one.
    Positioned,
    /// Creates a stacking context with this z-index.
    Context(i32),
}

/// One glyph to paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub id: u16,
    /// The absolute x of the glyph origin (offsets included).
    pub x: f64,
    /// The offset of the glyph origin from the baseline, downward.
    pub y: f64,
}

/// A run of shaped text.
#[derive(Clone, Debug)]
pub struct TextFragment {
    /// The text after white space processing and `text-transform`.
    pub text: String,
    /// The face of every glyph (`None` when no font was found: the
    /// glyphs are then empty boxes of the fallback metrics).
    pub face: Option<Arc<Face>>,
    pub font_size: f64,
    /// The absolute y of the baseline.
    pub baseline: f64,
    pub glyphs: Vec<PlacedGlyph>,
    /// The bidi embedding level of the run (odd: right to left).
    pub level: u8,
    /// The ascent and descent of the font, in pixels.
    pub ascent: f64,
    pub descent: f64,
}

/// A fragment: one box (or part of one) after layout.
#[derive(Clone, Debug)]
pub struct Fragment {
    pub kind: FragmentKind,
    pub node: Option<NodeId>,
    pub pseudo: Option<Pseudo>,
    pub style: Rc<ComputedStyle>,
    /// The border box.
    pub rect: Rect,
    pub margin: Sides,
    pub border: Sides,
    pub padding: Sides,
    pub children: Vec<Fragment>,
    /// The scroll container this box is, if it is one.
    pub scroll: Option<Box<Scroll>>,
    /// The overflow clip of the box's content.
    pub clip: Option<Clip>,
    pub stacking: Stacking,
    /// Where a replaced element draws its content (`object-fit` and
    /// `object-position`, css-images-3 §5.5), in absolute coordinates
    /// (relative to the border box until the tree is absolute).
    pub object: Option<Rect>,
}

impl Fragment {
    pub(crate) fn new(kind: FragmentKind, style: Rc<ComputedStyle>) -> Fragment {
        Fragment {
            kind,
            node: None,
            pseudo: None,
            style,
            rect: Rect::default(),
            margin: Sides::default(),
            border: Sides::default(),
            padding: Sides::default(),
            children: Vec::new(),
            scroll: None,
            clip: None,
            stacking: Stacking::None,
            object: None,
        }
    }

    /// The box is visible (`visibility: collapse` hides boxes that are
    /// not table rows and columns, css-display-3 §4).
    pub fn is_visible(&self) -> bool {
        matches!(self.style.text("visibility").as_str(), "" | "visible")
    }

    pub fn padding_rect(&self) -> Rect {
        self.rect.inset(&self.border)
    }

    pub fn content_rect(&self) -> Rect {
        self.rect.inset(&self.border.add(&self.padding))
    }

    pub fn margin_rect(&self) -> Rect {
        self.rect.outset(&self.margin)
    }

    /// The fragment and its descendants in tree order.
    pub fn descendants(&self) -> Vec<&Fragment> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(f) = stack.pop() {
            out.push(f);
            stack.extend(f.children.iter().rev());
        }
        out
    }

    /// The fragments of `node` (an element generates several when an
    /// inline box spans lines), in tree order.
    pub fn fragments_of(&self, node: NodeId) -> Vec<&Fragment> {
        self.descendants()
            .into_iter()
            .filter(|f| f.node == Some(node) && f.pseudo.is_none())
            .collect()
    }

    /// The text fragments under this fragment, in tree order.
    pub fn texts(&self) -> Vec<(&Fragment, &TextFragment)> {
        self.descendants()
            .into_iter()
            .filter_map(|f| match &f.kind {
                FragmentKind::Text(t) => Some((f, t)),
                _ => None,
            })
            .collect()
    }

    /// Moves the fragment and its subtree from coordinates relative to
    /// its parent's border box to absolute ones (`dx`, `dy` = the
    /// parent's absolute origin).
    pub(crate) fn absolutize(&mut self, dx: f64, dy: f64) {
        self.rect.x += dx;
        self.rect.y += dy;
        let (x, y) = (self.rect.x, self.rect.y);
        if let Some(o) = &mut self.object {
            o.x += x;
            o.y += y;
        }
        match &mut self.kind {
            FragmentKind::Line { baseline } => *baseline += y,
            FragmentKind::Text(t) => {
                t.baseline += y;
                for g in &mut t.glyphs {
                    g.x += x;
                }
            }
            _ => {}
        }
        for c in &mut self.children {
            c.absolutize(x, y);
        }
    }

    /// Moves the fragment and its subtree by (`dx`, `dy`) once the tree
    /// is absolute.
    pub(crate) fn shift(&mut self, dx: f64, dy: f64) {
        self.rect.x += dx;
        self.rect.y += dy;
        if let Some(o) = &mut self.object {
            o.x += dx;
            o.y += dy;
        }
        if let Some(sc) = &mut self.scroll {
            sc.scrollport.x += dx;
            sc.scrollport.y += dy;
            sc.scrollable.x += dx;
            sc.scrollable.y += dy;
        }
        if let Some(c) = &mut self.clip {
            if let Some((a, b)) = &mut c.x {
                *a += dx;
                *b += dx;
            }
            if let Some((a, b)) = &mut c.y {
                *a += dy;
                *b += dy;
            }
        }
        match &mut self.kind {
            FragmentKind::Line { baseline } => *baseline += dy,
            FragmentKind::Text(t) => {
                t.baseline += dy;
                for g in &mut t.glyphs {
                    g.x += dx;
                }
            }
            _ => {}
        }
        for c in &mut self.children {
            c.shift(dx, dy);
        }
    }

    /// Moves the fragment by (`dx`, `dy`) in its parent's coordinates.
    pub(crate) fn translate(&mut self, dx: f64, dy: f64) {
        self.rect.x += dx;
        self.rect.y += dy;
    }
}

/// The result of a layout pass.
#[derive(Clone, Debug)]
pub struct FragmentTree {
    /// The viewport fragment.
    pub root: Fragment,
}

impl FragmentTree {
    /// The fragments of `node`, in tree order.
    pub fn fragments_of(&self, node: NodeId) -> Vec<&Fragment> {
        self.root.fragments_of(node)
    }

    /// The first fragment of `node`.
    pub fn first_fragment(&self, node: NodeId) -> Option<&Fragment> {
        self.root
            .descendants()
            .into_iter()
            .find(|f| f.node == Some(node) && f.pseudo.is_none())
    }

    /// The union of the border boxes of `node`'s fragments.
    pub fn border_box(&self, node: NodeId) -> Option<Rect> {
        let frags = self.fragments_of(node);
        let first = frags.first()?;
        let mut r = first.rect;
        for f in &frags[1..] {
            let x = r.x.min(f.rect.x);
            let y = r.y.min(f.rect.y);
            let right = r.right().max(f.rect.right());
            let bottom = r.bottom().max(f.rect.bottom());
            r = Rect::new(x, y, right - x, bottom - y);
        }
        Some(r)
    }
}
