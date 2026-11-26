//! The box tree (CSS 2 §9.2, css-display-3): one box per element of the
//! flat tree that is not `display: none` (`display: contents` gives its
//! children to its parent), `::before`, `::after` and `::marker` boxes
//! with their generated content, anonymous block boxes around inline
//! content next to blocks, inline boxes split around the blocks inside
//! them, `::first-letter` boxes, replaced elements (`img`, `iframe`,
//! `video`, `canvas`, `embed`, checkboxes and radio buttons) and the
//! other form controls as inline-block containers of their text.
//!
//! Inline content is stored flat: text, atomic inlines, floats and
//! out-of-flow boxes, with markers where inline boxes open and close.

use std::rc::Rc;

use crate::css::cascade::Pseudo;
use crate::css::style::{ComputedStyle, StyleMap, initial_style};
use crate::dom::{Document, Namespace, NodeData, NodeId};

use super::content::{self, Counters, ListAttributes, Quotes};
use super::fragment::ReplacedKind;
use super::values::{self, Display, Float, Inner, Outer, kw};

/// A box that is not an inline box.
#[derive(Debug)]
pub(crate) struct BoxNode {
    pub style: Rc<ComputedStyle>,
    pub node: Option<NodeId>,
    pub pseudo: Option<Pseudo>,
    pub kind: BoxKind,
}

#[derive(Debug)]
pub(crate) enum BoxKind {
    Container(Container),
    Replaced(Replaced),
}

/// A block container (block-level, atomic inline-level, floating or out
/// of flow).
#[derive(Debug)]
pub(crate) struct Container {
    pub contents: Contents,
    pub anonymous: bool,
    /// Establishes an independent block formatting context.
    pub bfc_root: bool,
    /// An atomic inline (`inline-block` and form controls).
    pub inline_level: bool,
    /// The `::marker` box placed outside the list item.
    pub marker: Option<Box<BoxNode>>,
    /// The style of `::first-line`.
    pub first_line: Option<Rc<ComputedStyle>>,
    /// The natural size of a form control, in characters and lines of
    /// its font.
    pub control: Option<ControlSize>,
    /// Lines never wrap (single-line form controls).
    pub single_line: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ControlSize {
    pub chars: Option<f64>,
    pub lines: Option<f64>,
}

#[derive(Debug)]
pub(crate) enum Contents {
    Blocks(Vec<BoxNode>),
    Inline(InlineContent),
}

/// A replaced element.
#[derive(Debug)]
pub(crate) struct Replaced {
    pub kind: ReplacedKind,
    pub natural_width: Option<f64>,
    pub natural_height: Option<f64>,
    pub inline_level: bool,
}

impl Replaced {
    /// The natural aspect ratio (width / height).
    pub fn natural_ratio(&self) -> Option<f64> {
        match (self.natural_width, self.natural_height) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some(w / h),
            _ => None,
        }
    }
}

/// An inline box (its parts are on lines).
#[derive(Clone, Debug)]
pub(crate) struct InlineBox {
    pub style: Rc<ComputedStyle>,
    pub node: Option<NodeId>,
    pub pseudo: Option<Pseudo>,
    /// The box continues one split by a block: no start edge.
    pub split_start: bool,
    /// The box is split by a block after this part: no end edge.
    pub split_end: bool,
}

#[derive(Debug)]
pub(crate) enum InlineItem {
    Text {
        style: Rc<ComputedStyle>,
        text: String,
    },
    Open(usize),
    Close(usize),
    Atomic(BoxNode),
    Float(BoxNode),
    OutOfFlow(BoxNode),
    /// `<br>`.
    Break(Rc<ComputedStyle>),
    /// `<wbr>`.
    WordBreak,
}

/// The content of an inline formatting context.
#[derive(Debug, Default)]
pub(crate) struct InlineContent {
    pub boxes: Vec<InlineBox>,
    pub items: Vec<InlineItem>,
}

/// White space that `white-space-collapse: collapse` removes.
pub(crate) fn is_collapsible_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{C}')
}

pub(crate) fn collapses(style: &ComputedStyle) -> bool {
    matches!(
        kw(style, "white-space-collapse").as_str(),
        "collapse" | "discard"
    )
}

impl InlineContent {
    /// True if the content makes line boxes that are not empty (CSS 2
    /// §9.4.2): text other than collapsible white space, atomic inlines,
    /// forced breaks, or inline boxes with inline margins, borders or
    /// paddings.
    pub fn is_significant(&self) -> bool {
        self.items.iter().any(|item| match item {
            InlineItem::Text { style, text } => {
                if collapses(style) {
                    text.chars().any(|c| !is_collapsible_space(c))
                } else if kw(style, "white-space-collapse") == "preserve-breaks" {
                    text.chars().any(|c| !matches!(c, ' ' | '\t'))
                } else {
                    !text.is_empty()
                }
            }
            InlineItem::Atomic(_) | InlineItem::Break(_) => true,
            InlineItem::Open(i) => {
                let s = &self.boxes[*i].style;
                [
                    "margin-left",
                    "margin-right",
                    "padding-left",
                    "padding-right",
                ]
                .iter()
                .any(|p| values::lp(s, p).is_some_and(|l| l.resolve(100.0) != 0.0))
                    || values::border(s).horizontal() != 0.0
            }
            _ => false,
        })
    }
}

/// The style of an anonymous box inside a box with style `parent`.
pub(crate) fn anonymous_style(parent: &ComputedStyle) -> Rc<ComputedStyle> {
    let initial = initial_style();
    Rc::new(ComputedStyle {
        inherited: parent.inherited.clone(),
        reset: initial.reset.clone(),
        custom: parent.custom.clone(),
        font_size: parent.font_size,
        line_height: parent.line_height,
        font_weight: parent.font_weight,
        color: parent.color,
    })
}

/// Collects the children of a block container.
struct Sink {
    blocks: Vec<BoxNode>,
    run: InlineContent,
    open: Vec<usize>,
}

impl Sink {
    fn new() -> Sink {
        Sink {
            blocks: Vec::new(),
            run: InlineContent::default(),
            open: Vec::new(),
        }
    }

    fn push(&mut self, item: InlineItem) {
        self.run.items.push(item);
    }

    fn open(&mut self, b: InlineBox) {
        let i = self.run.boxes.len();
        self.run.boxes.push(b);
        self.run.items.push(InlineItem::Open(i));
        self.open.push(i);
    }

    fn close(&mut self) {
        if let Some(i) = self.open.pop() {
            self.run.items.push(InlineItem::Close(i));
        }
    }

    /// A block-level box: inline boxes open around it are split (CSS 2
    /// §9.2.1.1).
    fn push_block(&mut self, parent: &ComputedStyle, b: BoxNode) {
        let open = std::mem::take(&mut self.open);
        for &i in open.iter().rev() {
            self.run.boxes[i].split_end = true;
            self.run.items.push(InlineItem::Close(i));
        }
        let reopen: Vec<InlineBox> = open
            .iter()
            .map(|&i| InlineBox {
                split_start: true,
                split_end: false,
                ..self.run.boxes[i].clone()
            })
            .collect();
        self.flush(parent);
        self.blocks.push(b);
        for b in reopen {
            self.open(b);
        }
    }

    /// Wraps the pending inline content in an anonymous block box, or
    /// moves its floats and out-of-flow boxes out when it only holds
    /// collapsible white space.
    fn flush(&mut self, parent: &ComputedStyle) {
        let run = std::mem::take(&mut self.run);
        if run.items.is_empty() {
            return;
        }
        if run.is_significant() {
            self.blocks.push(BoxNode {
                style: anonymous_style(parent),
                node: None,
                pseudo: None,
                kind: BoxKind::Container(Container {
                    contents: Contents::Inline(run),
                    anonymous: true,
                    bfc_root: false,
                    inline_level: false,
                    marker: None,
                    first_line: None,
                    control: None,
                    single_line: false,
                }),
            });
        } else {
            for item in run.items {
                if let InlineItem::Float(b) | InlineItem::OutOfFlow(b) = item {
                    self.blocks.push(b);
                }
            }
        }
    }

    fn finish(mut self, parent: &ComputedStyle) -> Contents {
        while !self.open.is_empty() {
            self.close();
        }
        if self.blocks.is_empty() {
            return Contents::Inline(self.run);
        }
        self.flush(parent);
        Contents::Blocks(self.blocks)
    }

    /// The children of a flex container: every contiguous run of inline
    /// content becomes an anonymous block, so that it is one flex item
    /// (css-flexbox-1 §4), and runs of only collapsible white space are
    /// not rendered.
    fn finish_items(mut self, parent: &ComputedStyle) -> Contents {
        while !self.open.is_empty() {
            self.close();
        }
        self.flush(parent);
        Contents::Blocks(self.blocks)
    }
}

/// Builds the box tree of a document.
pub(crate) struct Builder<'a> {
    doc: &'a Document,
    styles: &'a StyleMap,
    images: &'a dyn Fn(NodeId) -> Option<(f64, f64)>,
    counters: Counters,
    quotes: Quotes,
    /// The body's `overflow` goes to the viewport (CSS 2 §11.1.1).
    pub body_overflow_propagated: bool,
    pub body: Option<NodeId>,
    /// The boxes being built are children of a flex or grid container:
    /// they are blockified and do not float (css-display-3 §2.7,
    /// css-flexbox-1 §4, css-grid-2 §6).
    flex_parent: bool,
}

impl<'a> Builder<'a> {
    pub fn new(
        doc: &'a Document,
        styles: &'a StyleMap,
        images: &'a dyn Fn(NodeId) -> Option<(f64, f64)>,
    ) -> Builder<'a> {
        Builder {
            doc,
            styles,
            images,
            counters: Counters::default(),
            quotes: Quotes::default(),
            body_overflow_propagated: false,
            body: None,
            flex_parent: false,
        }
    }

    /// The root element's box.
    pub fn build(&mut self) -> Option<BoxNode> {
        let root = self.doc.document_element()?;
        let style = self.styles.get(root)?.clone();
        if values::display(&style) == Display::None {
            return None;
        }
        self.body = self.doc.body();
        if let Some(b) = self.body {
            let root_visible =
                kw(&style, "overflow-x") == "visible" && kw(&style, "overflow-y") == "visible";
            self.body_overflow_propagated = root_visible
                && self.doc.parent(b) == Some(root)
                && self.doc.is_html_element(root, "html");
        }
        let mut sink = Sink::new();
        self.counters.push();
        self.element(root, &mut sink);
        self.counters.pop();
        let contents = sink.finish(&style);
        match contents {
            Contents::Blocks(mut blocks) => {
                let i = blocks.iter().position(|b| b.node == Some(root))?;
                let mut b = blocks.swap_remove(i);
                if let BoxKind::Container(c) = &mut b.kind {
                    c.bfc_root = true;
                }
                Some(b)
            }
            Contents::Inline(mut ic) => ic.items.drain(..).find_map(|item| match item {
                InlineItem::Atomic(mut b)
                | InlineItem::Float(mut b)
                | InlineItem::OutOfFlow(mut b)
                    if b.node == Some(root) =>
                {
                    if let BoxKind::Container(c) = &mut b.kind {
                        c.bfc_root = true;
                        c.inline_level = false;
                    }
                    Some(b)
                }
                _ => None,
            }),
        }
    }

    fn attr(&self, e: NodeId, name: &str) -> Option<String> {
        self.doc.get_attribute(e, name).map(str::to_string)
    }

    fn local_name(&self, e: NodeId) -> Option<String> {
        let d = self.doc.element(e)?;
        if d.name.ns != Some(Namespace::Html) {
            return None;
        }
        Some(d.name.local.as_str().to_string())
    }

    /// The children of `e` in the flat tree.
    fn flat_children(&self, e: NodeId) -> Vec<NodeId> {
        if let Some(sr) = self.doc.shadow_root_of(e) {
            return self.doc.children(sr).collect();
        }
        if self.doc.is_slot(e) {
            let assigned = self.doc.assigned_nodes(e, false);
            if !assigned.is_empty() {
                return assigned;
            }
        }
        self.doc.children(e).collect()
    }

    fn list_attributes(&self, e: NodeId) -> ListAttributes {
        let int = |n: &str| self.attr(e, n).and_then(|v| v.trim().parse::<i64>().ok());
        let mut a = ListAttributes::default();
        match self.local_name(e).as_deref() {
            Some("ol") => {
                a.start = int("start");
                a.reversed = self.attr(e, "reversed").is_some();
                a.items = self
                    .doc
                    .children(e)
                    .filter(|&c| {
                        self.styles.get(c).is_some_and(|s| {
                            matches!(
                                values::display(s),
                                Display::Box {
                                    list_item: true,
                                    ..
                                }
                            )
                        })
                    })
                    .count() as i64;
            }
            Some("li") => a.value = int("value"),
            _ => {}
        }
        a
    }

    fn element(&mut self, e: NodeId, sink: &mut Sink) {
        let Some(style) = self.styles.get(e).cloned() else {
            return;
        };
        let display = values::display(&style);
        if display == Display::None {
            return;
        }
        let list_item = matches!(
            display,
            Display::Box {
                list_item: true,
                ..
            }
        );
        // `display: contents` on replaced elements and form controls
        // computes to `none` (css-display-3 §2.8, Appendix B).
        if display == Display::Contents
            && self.doc.element(e).is_some_and(|d| {
                d.name.ns == Some(Namespace::Html)
                    && matches!(
                        d.name.local.as_str(),
                        "img"
                            | "iframe"
                            | "video"
                            | "audio"
                            | "canvas"
                            | "embed"
                            | "object"
                            | "input"
                            | "textarea"
                            | "select"
                            | "meter"
                            | "progress"
                            | "br"
                            | "wbr"
                    )
            })
        {
            return;
        }
        let attrs = self.list_attributes(e);
        content::apply_counters(&mut self.counters, &style, list_item, attrs);
        if display == Display::Contents {
            self.counters.push();
            self.pseudo(e, Pseudo::Before, sink);
            self.children(e, &style, sink);
            self.pseudo(e, Pseudo::After, sink);
            self.counters.pop();
            return;
        }
        let local = self.local_name(e);
        match local.as_deref() {
            Some("br") => {
                sink.push(InlineItem::Break(style));
                return;
            }
            Some("wbr") => {
                sink.push(InlineItem::WordBreak);
                return;
            }
            _ => {}
        }
        if let Some(r) = self.replaced(e, local.as_deref()) {
            let inline_level = !self.flex_parent
                && matches!(
                    display,
                    Display::Box {
                        outer: Outer::Inline,
                        ..
                    }
                );
            let b = BoxNode {
                style: style.clone(),
                node: Some(e),
                pseudo: None,
                kind: BoxKind::Replaced(Replaced { inline_level, ..r }),
            };
            self.attach(b, &style, inline_level, sink);
            return;
        }
        self.build_box(style, Some(e), None, Kids::Element(e), sink);
    }

    /// The replaced element `e` is, with its natural size.
    fn replaced(&self, e: NodeId, local: Option<&str>) -> Option<Replaced> {
        let r = |kind, w: Option<f64>, h: Option<f64>| {
            Some(Replaced {
                kind,
                natural_width: w,
                natural_height: h,
                inline_level: true,
            })
        };
        let dim = |n: &str, default: f64| {
            self.attr(e, n)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| *v >= 0.0)
                .unwrap_or(default)
        };
        match local? {
            "img" => match (self.images)(e) {
                Some((w, h)) => r(ReplacedKind::Image, Some(w), Some(h)),
                None => {
                    if self.attr(e, "alt").is_some_and(|a| !a.is_empty()) {
                        None
                    } else {
                        r(ReplacedKind::Image, Some(0.0), Some(0.0))
                    }
                }
            },
            "iframe" => r(ReplacedKind::Iframe, Some(300.0), Some(150.0)),
            "video" => r(ReplacedKind::Video, Some(300.0), Some(150.0)),
            "embed" => r(ReplacedKind::Embed, Some(300.0), Some(150.0)),
            "canvas" => r(
                ReplacedKind::Canvas,
                Some(dim("width", 300.0)),
                Some(dim("height", 150.0)),
            ),
            "meter" => r(ReplacedKind::Control, Some(80.0), Some(16.0)),
            "progress" => r(ReplacedKind::Control, Some(160.0), Some(16.0)),
            "input" => {
                let t = self
                    .attr(e, "type")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                match t.as_str() {
                    "checkbox" | "radio" => r(ReplacedKind::Check, Some(13.0), Some(13.0)),
                    "range" => r(ReplacedKind::Control, Some(129.0), Some(16.0)),
                    "color" => r(ReplacedKind::Control, Some(50.0), Some(27.0)),
                    "image" => match (self.images)(e) {
                        Some((w, h)) => r(ReplacedKind::Image, Some(w), Some(h)),
                        None => r(ReplacedKind::Image, Some(0.0), Some(0.0)),
                    },
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Adds a finished box to `sink` by its positioning scheme.
    fn attach(&mut self, b: BoxNode, parent: &ComputedStyle, inline_level: bool, sink: &mut Sink) {
        let s = b.style.clone();
        if values::out_of_flow(&s) {
            sink.push(InlineItem::OutOfFlow(b));
        } else if values::float(&s) != Float::None && !self.flex_parent {
            sink.push(InlineItem::Float(b));
        } else if inline_level {
            sink.push(InlineItem::Atomic(b));
        } else {
            sink.push_block(parent, b);
        }
    }

    /// The children of `e` (flat tree): elements and text.
    fn children(&mut self, e: NodeId, style: &Rc<ComputedStyle>, sink: &mut Sink) {
        for c in self.flat_children(e) {
            match self.doc.data(c) {
                NodeData::Element(_) => self.element(c, sink),
                NodeData::Text(t) | NodeData::CDataSection(t) => {
                    if !t.is_empty() {
                        sink.push(InlineItem::Text {
                            style: style.clone(),
                            text: t.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// `::before` or `::after` of `e`.
    fn pseudo(&mut self, e: NodeId, which: Pseudo, sink: &mut Sink) {
        let Some(style) = self.styles.pseudo(e, which).cloned() else {
            return;
        };
        let display = values::display(&style);
        if display == Display::None {
            return;
        }
        let doc = self.doc;
        let attr = |n: &str| doc.get_attribute(e, n).map(str::to_string);
        let list_item = matches!(
            display,
            Display::Box {
                list_item: true,
                ..
            }
        );
        content::apply_counters(
            &mut self.counters,
            &style,
            list_item,
            ListAttributes::default(),
        );
        let Some(text) = content::content_text(&style, &self.counters, &mut self.quotes, &attr)
        else {
            return;
        };
        if display == Display::Contents {
            if !text.is_empty() {
                sink.push(InlineItem::Text { style, text });
            }
            return;
        }
        self.build_box(style, Some(e), Some(which), Kids::Text(text), sink);
    }

    /// The `::marker` of list item `e`: its text, or `None`.
    fn marker_text(
        &mut self,
        e: NodeId,
        style: &ComputedStyle,
    ) -> Option<(Rc<ComputedStyle>, String)> {
        let mstyle = self.styles.pseudo(e, Pseudo::Marker).cloned()?;
        let doc = self.doc;
        let attr = |n: &str| doc.get_attribute(e, n).map(str::to_string);
        let normal = matches!(kw(&mstyle, "content").as_str(), "normal");
        let text = if normal {
            content::marker_text(style, &self.counters)?
        } else {
            content::content_text(&mstyle, &self.counters, &mut self.quotes, &attr)?
        };
        Some((mstyle, text))
    }

    /// Builds a box that is not replaced: an inline box, or a block
    /// container with its children.
    fn build_box(
        &mut self,
        style: Rc<ComputedStyle>,
        node: Option<NodeId>,
        pseudo: Option<Pseudo>,
        kids: Kids,
        sink: &mut Sink,
    ) {
        let Display::Box {
            outer,
            inner,
            list_item,
        } = values::display(&style)
        else {
            return;
        };
        let blockify = self.flex_parent;
        let outer = if blockify { Outer::Block } else { outer };
        let floats = values::float(&style) != Float::None && !blockify;
        let oof = values::out_of_flow(&style);
        let local = node
            .filter(|_| pseudo.is_none())
            .and_then(|e| self.local_name(e));
        let control = self.control(node.filter(|_| pseudo.is_none()), local.as_deref());
        if outer == Outer::Inline && inner == Inner::Flow && !floats && !oof && control.is_none() {
            sink.open(InlineBox {
                style: style.clone(),
                node,
                pseudo,
                split_start: false,
                split_end: false,
            });
            self.counters.push();
            self.kids(&kids, &style, sink);
            self.counters.pop();
            sink.close();
            return;
        }
        let mut inner_sink = Sink::new();
        self.counters.push();
        self.flex_parent = matches!(inner, Inner::Flex | Inner::Grid);
        let mut marker = None;
        if list_item {
            if let (Some(e), None) = (node, pseudo) {
                if let Some((mstyle, text)) = self.marker_text(e, &style) {
                    if kw(&style, "list-style-position") == "inside" {
                        inner_sink.open(InlineBox {
                            style: mstyle.clone(),
                            node: Some(e),
                            pseudo: Some(Pseudo::Marker),
                            split_start: false,
                            split_end: false,
                        });
                        inner_sink.push(InlineItem::Text {
                            style: mstyle,
                            text,
                        });
                        inner_sink.close();
                    } else {
                        let mut ic = InlineContent::default();
                        ic.items.push(InlineItem::Text {
                            style: mstyle.clone(),
                            text,
                        });
                        marker = Some(Box::new(BoxNode {
                            style: mstyle,
                            node: Some(e),
                            pseudo: Some(Pseudo::Marker),
                            kind: BoxKind::Container(Container {
                                contents: Contents::Inline(ic),
                                anonymous: false,
                                bfc_root: true,
                                inline_level: true,
                                marker: None,
                                first_line: None,
                                control: None,
                                single_line: true,
                            }),
                        }));
                    }
                }
            }
        }
        let (control_size, single_line) = match &control {
            Some(c) => {
                self.control_contents(c, &style, &mut inner_sink);
                (c.size, c.single_line)
            }
            None => {
                self.kids(&kids, &style, &mut inner_sink);
                (None, false)
            }
        };
        self.counters.pop();
        self.flex_parent = blockify;
        let mut contents = if matches!(inner, Inner::Flex | Inner::Grid) {
            inner_sink.finish_items(&style)
        } else {
            inner_sink.finish(&style)
        };
        if let (Some(e), None) = (node, pseudo) {
            if let Some(fl) = self.styles.pseudo(e, Pseudo::FirstLetter).cloned() {
                first_letter(&mut contents, fl, e);
            }
        }
        let first_line = match (node, pseudo) {
            (Some(e), None) => self.styles.pseudo(e, Pseudo::FirstLine).cloned(),
            _ => None,
        };
        let is_body = node.is_some() && node == self.body && pseudo.is_none();
        let overflow_bfc =
            values::overflow_establishes_bfc(&style) && !(is_body && self.body_overflow_propagated);
        let inline_level = outer == Outer::Inline;
        let bfc_root = inner != Inner::Flow
            || overflow_bfc
            || floats
            || oof
            || inline_level
            || control.is_some()
            || blockify;
        let b = BoxNode {
            style: style.clone(),
            node,
            pseudo,
            kind: BoxKind::Container(Container {
                contents,
                anonymous: false,
                bfc_root,
                inline_level,
                marker,
                first_line,
                control: control_size,
                single_line,
            }),
        };
        self.attach(b, &style, inline_level, sink);
    }

    fn kids(&mut self, kids: &Kids, style: &Rc<ComputedStyle>, sink: &mut Sink) {
        match kids {
            Kids::Element(e) => {
                let e = *e;
                self.pseudo(e, Pseudo::Before, sink);
                self.children(e, style, sink);
                self.pseudo(e, Pseudo::After, sink);
            }
            Kids::Text(t) => {
                if !t.is_empty() {
                    sink.push(InlineItem::Text {
                        style: style.clone(),
                        text: t.clone(),
                    });
                }
            }
        }
    }

    /// The form control `e` is, if any (not counting the replaced ones).
    fn control(&self, e: Option<NodeId>, local: Option<&str>) -> Option<Control> {
        let e = e?;
        let text_of = |n: NodeId| self.doc.text_content(n).unwrap_or_default();
        match local? {
            "input" => {
                let t = self
                    .attr(e, "type")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let size = self
                    .attr(e, "size")
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|v| *v > 0.0)
                    .unwrap_or(20.0);
                match t.as_str() {
                    "submit" | "reset" | "button" => {
                        let label = self.attr(e, "value").unwrap_or_else(|| {
                            match t.as_str() {
                                "submit" => "Submit",
                                "reset" => "Reset",
                                _ => "",
                            }
                            .to_string()
                        });
                        Some(Control {
                            text: label,
                            placeholder: false,
                            size: None,
                            single_line: true,
                            node: e,
                        })
                    }
                    "file" => Some(Control {
                        text: "Browse\u{2026}".to_string(),
                        placeholder: false,
                        size: None,
                        single_line: true,
                        node: e,
                    }),
                    _ => {
                        let value = self.attr(e, "value").unwrap_or_default();
                        let (text, placeholder) = if value.is_empty() {
                            (self.attr(e, "placeholder").unwrap_or_default(), true)
                        } else if t == "password" {
                            ("\u{25CF}".repeat(value.chars().count()), false)
                        } else {
                            (value, false)
                        };
                        Some(Control {
                            text,
                            placeholder,
                            size: Some(ControlSize {
                                chars: Some(size),
                                lines: Some(1.0),
                            }),
                            single_line: true,
                            node: e,
                        })
                    }
                }
            }
            "textarea" => {
                let num = |n: &str, d: f64| {
                    self.attr(e, n)
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .filter(|v| *v > 0.0)
                        .unwrap_or(d)
                };
                let value = text_of(e);
                let (text, placeholder) = if value.is_empty() {
                    (self.attr(e, "placeholder").unwrap_or_default(), true)
                } else {
                    (value, false)
                };
                Some(Control {
                    text,
                    placeholder,
                    size: Some(ControlSize {
                        chars: Some(num("cols", 20.0)),
                        lines: Some(num("rows", 2.0)),
                    }),
                    single_line: false,
                    node: e,
                })
            }
            "select" => {
                let options: Vec<NodeId> = self
                    .doc
                    .inclusive_descendants(e)
                    .into_iter()
                    .filter(|&n| self.doc.is_html_element(n, "option"))
                    .collect();
                let chosen = options
                    .iter()
                    .copied()
                    .find(|&o| self.attr(o, "selected").is_some())
                    .or_else(|| options.first().copied());
                let text = chosen.map(text_of).unwrap_or_default();
                Some(Control {
                    text: text.trim().to_string(),
                    placeholder: false,
                    size: None,
                    single_line: true,
                    node: e,
                })
            }
            _ => None,
        }
    }

    fn control_contents(&mut self, c: &Control, style: &Rc<ComputedStyle>, sink: &mut Sink) {
        if c.text.is_empty() {
            return;
        }
        let text_style = if c.placeholder {
            self.styles
                .pseudo(c.node, Pseudo::Placeholder)
                .cloned()
                .unwrap_or_else(|| style.clone())
        } else {
            style.clone()
        };
        sink.push(InlineItem::Text {
            style: text_style,
            text: c.text.clone(),
        });
    }
}

/// The children a box gets.
enum Kids {
    Element(NodeId),
    Text(String),
}

struct Control {
    text: String,
    placeholder: bool,
    size: Option<ControlSize>,
    single_line: bool,
    node: NodeId,
}

/// The first inline formatting context of `contents` (descending into
/// the first in-flow block child).
fn first_inline(contents: &mut Contents) -> Option<&mut InlineContent> {
    match contents {
        Contents::Inline(ic) => Some(ic),
        Contents::Blocks(blocks) => {
            let first = blocks.iter_mut().find(|b| {
                !values::out_of_flow(&b.style) && values::float(&b.style) == Float::None
            })?;
            match &mut first.kind {
                BoxKind::Container(c) if !c.inline_level => first_inline(&mut c.contents),
                _ => None,
            }
        }
    }
}

fn is_punctuation(c: char) -> bool {
    matches!(
        vjs::unicode::general_category(c as u32),
        "Ps" | "Pe" | "Pi" | "Pf" | "Po"
    )
}

/// `::first-letter` (css-pseudo-4 §2.4): the first typographic letter
/// unit of the first line, with the punctuation around it, becomes an
/// inline box (or a float) with `style`.
fn first_letter(contents: &mut Contents, style: Rc<ComputedStyle>, e: NodeId) {
    let Some(ic) = first_inline(contents) else {
        return;
    };
    let mut at = None;
    for (i, item) in ic.items.iter().enumerate() {
        match item {
            InlineItem::Open(_)
            | InlineItem::Close(_)
            | InlineItem::Float(_)
            | InlineItem::OutOfFlow(_)
            | InlineItem::WordBreak => {}
            InlineItem::Text { text, .. } => {
                if text.chars().any(|c| !is_collapsible_space(c)) {
                    at = Some(i);
                    break;
                }
            }
            InlineItem::Atomic(_) | InlineItem::Break(_) => return,
        }
    }
    let Some(i) = at else {
        return;
    };
    let (text_style, text) = match &ic.items[i] {
        InlineItem::Text { style, text } => (style.clone(), text.clone()),
        _ => return,
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut k = 0;
    while k < chars.len() && is_collapsible_space(chars[k].1) {
        k += 1;
    }
    let start = k;
    while k < chars.len() && is_punctuation(chars[k].1) {
        k += 1;
    }
    if k >= chars.len() || is_collapsible_space(chars[k].1) {
        return;
    }
    k += 1;
    while k < chars.len() && super::linebreak::is_grapheme_extend(chars[k].1) {
        k += 1;
    }
    while k < chars.len() && is_punctuation(chars[k].1) {
        k += 1;
    }
    let byte = |k: usize| chars.get(k).map_or(text.len(), |c| c.0);
    let (lead, letter, rest) = (
        text[..byte(start)].to_string(),
        text[byte(start)..byte(k)].to_string(),
        text[byte(k)..].to_string(),
    );
    let mut replacement: Vec<InlineItem> = Vec::new();
    if !lead.is_empty() {
        replacement.push(InlineItem::Text {
            style: text_style.clone(),
            text: lead,
        });
    }
    if values::float(&style) != Float::None {
        let mut inner = InlineContent::default();
        inner.items.push(InlineItem::Text {
            style: style.clone(),
            text: letter,
        });
        replacement.push(InlineItem::Float(BoxNode {
            style,
            node: Some(e),
            pseudo: Some(Pseudo::FirstLetter),
            kind: BoxKind::Container(Container {
                contents: Contents::Inline(inner),
                anonymous: false,
                bfc_root: true,
                inline_level: false,
                marker: None,
                first_line: None,
                control: None,
                single_line: false,
            }),
        }));
    } else {
        let b = ic.boxes.len();
        ic.boxes.push(InlineBox {
            style: style.clone(),
            node: Some(e),
            pseudo: Some(Pseudo::FirstLetter),
            split_start: false,
            split_end: false,
        });
        replacement.push(InlineItem::Open(b));
        replacement.push(InlineItem::Text {
            style,
            text: letter,
        });
        replacement.push(InlineItem::Close(b));
    }
    if !rest.is_empty() {
        replacement.push(InlineItem::Text {
            style: text_style,
            text: rest,
        });
    }
    let tail = ic.items.split_off(i + 1);
    ic.items.pop();
    ic.items.extend(replacement);
    ic.items.extend(tail);
}
