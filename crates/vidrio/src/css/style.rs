//! Computed styles (css-cascade-5 §4, `vault/CONTRACT.md` §3.5): the
//! computed values of every element and of its `::before`, `::after`,
//! `::marker`, `::first-line`, `::first-letter` and `::placeholder`, in
//! two groups shared by `Rc` (the inherited properties and the reset
//! ones): an element that sets no inherited property shares its parent's
//! group, one that sets no reset property shares the initial group.
//! Inheritance follows the flat tree (slotted elements inherit from
//! their slot, the children of a shadow root from the host); the
//! CSS-wide keywords (`initial`, `inherit`, `unset`, `revert`,
//! `revert-layer`) are resolved here and in [`cascade::Cascaded`].
//! Values are computed with the element's font size (`em`), the root's
//! (`rem`), line heights (`lh`, `rlh`), the 800×600 viewport and
//! `currentcolor` kept as a keyword except on `color`.
//!
//! The whole document is recalculated when its nodes, attributes or
//! sheets changed since the last time ([`styles`]); `getComputedStyle`
//! reads [`resolved_value`], which for now gives the computed value of
//! every property (the ones that depend on layout come with k4-13).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;

use super::cascade::{self, Bloom, Candidate, Cascaded, Pseudo, Stylist, Winner};
use super::declaration::DeclarationBlock;
use super::media::Environment;
use super::om;
use super::properties::{self, PROPERTIES};
use super::tokenizer::{Token, tokenize};
use super::values::color::{Absolute, Color, ColorContext};
use super::values::compute::{self, Context};
use super::values::custom::{self, Registry};
use super::values::{Value, grammar, serialize_list};
use crate::dom::{DocRef, Document, NodeId};

/// A computed value: its serialization and, for properties with a
/// grammar, its values.
#[derive(Clone, Debug, PartialEq)]
pub struct CValue {
    pub text: String,
    pub values: Option<Vec<Value>>,
}

impl CValue {
    fn length(px: f64) -> CValue {
        CValue {
            text: format!("{}px", super::values::number(px)),
            values: Some(vec![Value::Dimension(px, "px".to_string())]),
        }
    }

    fn keyword(k: &str) -> CValue {
        CValue {
            text: k.to_string(),
            values: Some(vec![Value::Keyword(k.to_string())]),
        }
    }
}

/// The computed values of one group of longhands, by slot.
#[derive(Clone, Debug)]
pub struct Group(Vec<Rc<CValue>>);

/// Where each longhand of [`PROPERTIES`] lives: (inherited, slot).
struct Slots {
    of: Vec<(bool, usize)>,
}

fn slots() -> &'static Slots {
    static SLOTS: OnceLock<Slots> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let (mut inherited, mut reset) = (0, 0);
        let of = PROPERTIES
            .iter()
            .map(|p| {
                if p.inherited {
                    inherited += 1;
                    (true, inherited - 1)
                } else {
                    reset += 1;
                    (false, reset - 1)
                }
            })
            .collect();
        Slots { of }
    })
}

fn slot(name: &str) -> Option<(bool, usize)> {
    PROPERTIES
        .binary_search_by(|p| p.name.cmp(name))
        .ok()
        .map(|i| slots().of[i])
}

/// The computed style of an element or pseudo-element.
#[derive(Clone, Debug)]
pub struct ComputedStyle {
    pub inherited: Rc<Group>,
    pub reset: Rc<Group>,
    /// The computed custom properties.
    pub custom: Rc<HashMap<String, Vec<Token>>>,
    /// `font-size` in pixels.
    pub font_size: f64,
    /// The line height in pixels (`normal` is 1.2 times the font size).
    pub line_height: f64,
    pub font_weight: f64,
    /// The computed `color`.
    pub color: Absolute,
}

impl ComputedStyle {
    fn raw(&self, name: &str) -> Option<&Rc<CValue>> {
        let (inherited, s) = slot(name)?;
        Some(if inherited {
            &self.inherited.0[s]
        } else {
            &self.reset.0[s]
        })
    }

    /// The computed value of longhand `name`; a logical property gives
    /// the value of the physical one it maps to.
    pub fn get(&self, name: &str) -> Option<&CValue> {
        let name =
            properties::logical_to_physical(name, self.writing_mode(), self.rtl()).unwrap_or(name);
        self.raw(name).map(|v| &**v)
    }

    /// The serialized computed value of `name` (empty if unknown).
    pub fn text(&self, name: &str) -> String {
        self.get(name).map(|v| v.text.clone()).unwrap_or_default()
    }

    pub fn writing_mode(&self) -> &str {
        self.raw("writing-mode")
            .map_or("horizontal-tb", |v| v.text.as_str())
    }

    pub fn rtl(&self) -> bool {
        self.raw("direction").is_some_and(|v| v.text == "rtl")
    }

    /// A copy whose non-inherited longhands `names` take their computed
    /// values from `from` (unknown and inherited names are ignored).
    pub(crate) fn with_reset_from(&self, from: &ComputedStyle, names: &[&str]) -> ComputedStyle {
        let mut reset = (*self.reset).clone();
        for name in names {
            if let Some((false, s)) = slot(name) {
                reset.0[s] = from.reset.0[s].clone();
            }
        }
        ComputedStyle {
            reset: Rc::new(reset),
            ..self.clone()
        }
    }

    /// A copy with the non-inherited longhands `names` at their initial
    /// values.
    pub(crate) fn with_initial(&self, names: &[&str]) -> ComputedStyle {
        self.with_reset_from(&initial_style(), names)
    }
}

fn color_of(v: &CValue) -> Option<Absolute> {
    match v.values.as_deref()? {
        [Value::Color(c)] => match &**c {
            Color::Absolute(a) => Some(*a),
            _ => None,
        },
        _ => None,
    }
}

/// The computed value of `tokens` (without `var()`) for `name`; `None`
/// if they do not match the property's grammar.
fn compute_tokens(name: &str, tokens: &[Token], ctx: &Context) -> Option<CValue> {
    match grammar::parse_property(name, tokens) {
        Some(Some(values)) => {
            let computed = compute::compute(name, &values, ctx);
            Some(CValue {
                text: serialize_list(&computed),
                values: Some(computed),
            })
        }
        Some(None) => None,
        None => Some(CValue {
            text: custom::serialize_value(tokens),
            values: None,
        }),
    }
}

fn build_initial() -> Rc<ComputedStyle> {
    let ctx = Context::default();
    let mut inherited = Vec::new();
    let mut reset = Vec::new();
    for p in PROPERTIES {
        let v = compute_tokens(p.name, &tokenize(p.initial), &ctx).unwrap_or(CValue {
            text: p.initial.to_string(),
            values: None,
        });
        if p.inherited {
            inherited.push(Rc::new(v));
        } else {
            reset.push(Rc::new(v));
        }
    }
    // The initial `border-style` is `none`, so the computed initial
    // border widths are 0, not `medium` (see `Computer::fix_up`).
    for side in ["top", "right", "bottom", "left"] {
        if let Some((false, s)) = slot(&format!("border-{side}-width")) {
            reset[s] = Rc::new(CValue::length(0.0));
        }
    }
    let mut style = ComputedStyle {
        inherited: Rc::new(Group(inherited)),
        reset: Rc::new(Group(reset)),
        custom: Rc::new(HashMap::new()),
        font_size: ctx.font_size,
        line_height: ctx.line_height,
        font_weight: 400.0,
        color: Absolute::rgb8(0, 0, 0, 1.0),
    };
    if let Some(c) = style.raw("color").and_then(|v| color_of(v)) {
        style.color = c;
    }
    Rc::new(style)
}

thread_local! {
    static INITIAL: Rc<ComputedStyle> = build_initial();
}

/// The initial style: every longhand at its computed initial value.
pub fn initial_style() -> Rc<ComputedStyle> {
    INITIAL.with(Rc::clone)
}

// ----- computing one style -----------------------------------------------------------

/// What the cascade specifies for a property.
enum Spec {
    Inherit,
    Initial,
    /// `unset`, no declaration, or a value invalid at computed-value time.
    Unset,
    Tokens(Vec<Token>),
}

fn has_substitution(tokens: &[Token]) -> bool {
    tokens.iter().any(|t| match t {
        Token::Function(f) => {
            f.eq_ignore_ascii_case("var")
                || f.eq_ignore_ascii_case("env")
                || f.eq_ignore_ascii_case("attr")
        }
        _ => false,
    })
}

fn css_wide(tokens: &[Token]) -> Option<String> {
    let significant: Vec<&Token> = tokens.iter().filter(|t| **t != Token::Whitespace).collect();
    match significant.as_slice() {
        [Token::Ident(k)] if properties::is_css_wide_keyword(k) => Some(k.to_ascii_lowercase()),
        _ => None,
    }
}

struct Computer<'a> {
    doc: &'a Document,
    element: NodeId,
    parent: &'a ComputedStyle,
    cascaded: Cascaded<'a>,
    custom: Rc<HashMap<String, Vec<Token>>>,
    ctx: Context,
    inherited: Rc<Group>,
    reset: Rc<Group>,
}

impl Computer<'_> {
    fn attr(&self, name: &str) -> Option<String> {
        self.doc
            .get_attribute(self.element, name)
            .map(str::to_string)
    }

    /// True if the winning declaration of `name` has `var()` or `attr()`.
    fn uses_substitution(&self, name: &str) -> bool {
        match self.cascaded.winner(name) {
            Some(Winner::Declared(d)) => d.pending.is_some() || has_substitution(&d.value),
            _ => false,
        }
    }

    fn specified(&self, name: &str) -> Spec {
        let d = match self.cascaded.winner(name) {
            Some(Winner::Declared(d)) => d,
            _ => return Spec::Unset,
        };
        let attr = |n: &str| self.attr(n);
        let tokens = match d.pending {
            Some(shorthand) => {
                let Ok(value) = custom::substitute_with(&d.value, &self.custom, &attr) else {
                    return Spec::Unset;
                };
                let mut block = DeclarationBlock::default();
                block.push_raw(shorthand, &value, false);
                match block.declarations.iter().find(|x| x.name == d.name) {
                    Some(x) => x.value.clone(),
                    None => return Spec::Unset,
                }
            }
            None => {
                let Some(value) = cascade::shorthand_value(d, name) else {
                    return Spec::Unset;
                };
                if has_substitution(&value) {
                    match custom::substitute_with(&value, &self.custom, &attr) {
                        Ok(v) => v,
                        Err(_) => return Spec::Unset,
                    }
                } else {
                    value
                }
            }
        };
        match css_wide(&tokens).as_deref() {
            Some("inherit") => Spec::Inherit,
            Some("initial") => Spec::Initial,
            Some(_) => Spec::Unset,
            None => Spec::Tokens(tokens),
        }
    }

    fn current(&self, name: &str) -> Option<Rc<CValue>> {
        let (inherited, s) = slot(name)?;
        Some(if inherited {
            self.inherited.0[s].clone()
        } else {
            self.reset.0[s].clone()
        })
    }

    fn set(&mut self, name: &str, v: Rc<CValue>) {
        let Some((inherited, s)) = slot(name) else {
            return;
        };
        let group = if inherited {
            &mut self.inherited
        } else {
            &mut self.reset
        };
        if Rc::ptr_eq(&group.0[s], &v) || group.0[s].text == v.text {
            return;
        }
        Rc::make_mut(group).0[s] = v;
    }

    /// Computes the cascaded value of `name` into its group.
    fn apply(&mut self, name: &str) {
        let Some((inherited, _)) = slot(name) else {
            return;
        };
        match self.specified(name) {
            Spec::Unset => {}
            Spec::Inherit => {
                if !inherited {
                    if let Some(v) = self.parent.raw(name).cloned() {
                        self.set(name, v);
                    }
                }
            }
            Spec::Initial => {
                if inherited {
                    if let Some(v) = initial_style().raw(name).cloned() {
                        self.set(name, v);
                    }
                }
            }
            Spec::Tokens(t) => {
                if let Some(v) = compute_tokens(name, &t, &self.ctx) {
                    self.set(name, Rc::new(v));
                }
            }
        }
    }

    fn keyword(&self, name: &str) -> String {
        self.current(name)
            .map(|v| v.text.clone())
            .unwrap_or_default()
    }

    fn apply_font_size(&mut self, is_root: bool) {
        self.apply("font-size");
        if let Some(v) = self.current("font-size") {
            if let Some([Value::Dimension(px, _)]) = v.values.as_deref() {
                self.ctx.font_size = *px;
            }
        }
        if is_root {
            self.ctx.root_font_size = self.ctx.font_size;
        }
    }

    fn apply_custom(&mut self, registry: &Registry) {
        let specified: Vec<(String, Vec<Token>)> = {
            let mut names: Vec<&String> = self
                .cascaded
                .names()
                .filter(|n| n.starts_with("--"))
                .collect();
            names.sort();
            names
                .into_iter()
                .map(|n| {
                    let v = match self.cascaded.winner(n) {
                        Some(Winner::Declared(d)) => d.value.clone(),
                        _ => tokenize("unset"),
                    };
                    (n.clone(), v)
                })
                .collect()
        };
        if specified.is_empty() && registry.is_empty() {
            return;
        }
        let attr = |n: &str| self.attr(n);
        let computed = custom::compute_custom_properties(
            &specified,
            &self.parent.custom,
            registry,
            &self.ctx,
            &attr,
        );
        self.custom = Rc::new(computed);
    }

    /// The fix-ups of computed values that depend on other properties.
    fn fix_up(&mut self, is_root: bool) {
        // A border side whose style is `none` or `hidden` has no width;
        // otherwise a width left at its initial value is `medium` (the
        // initial group holds 0, the width under the initial style).
        for side in ["top", "right", "bottom", "left"] {
            let style = self.keyword(&format!("border-{side}-style"));
            let width = format!("border-{side}-width");
            if style == "none" || style == "hidden" {
                self.set(&width, Rc::new(CValue::length(0.0)));
            } else if matches!(self.specified(&width), Spec::Initial | Spec::Unset) {
                if let Some(v) = compute_tokens(&width, &tokenize("medium"), &self.ctx) {
                    self.set(&width, Rc::new(v));
                }
            }
        }
        // Absolutely positioned boxes do not float; the root, floats and
        // absolutely positioned boxes are blockified (css-display-3 §2.7).
        let position = self.keyword("position");
        let absolute = position == "absolute" || position == "fixed";
        if absolute && self.keyword("float") != "none" {
            self.set("float", Rc::new(CValue::keyword("none")));
        }
        if is_root || absolute || self.keyword("float") != "none" {
            let display = self.keyword("display");
            let block = match display.as_str() {
                "inline" | "inline-block" | "table-row-group" | "table-column"
                | "table-column-group" | "table-header-group" | "table-footer-group"
                | "table-row" | "table-cell" | "table-caption" | "run-in" => Some("block"),
                "contents" if is_root => Some("block"),
                "inline-table" => Some("table"),
                "inline-flex" => Some("flex"),
                "inline-grid" => Some("grid"),
                "inline flow-root" => Some("flow-root"),
                "inline list-item" => Some("list-item"),
                _ => None,
            };
            if let Some(b) = block {
                self.set("display", Rc::new(CValue::keyword(b)));
            }
        }
    }
}

/// The computed style of `element` (or of one of its pseudo-elements)
/// from the declarations that apply to it, its parent's style and the
/// root element's (`None` when `element` is the root).
fn compute_style(
    doc: &Document,
    element: NodeId,
    candidates: &[Candidate],
    parent: &ComputedStyle,
    root: Option<&ComputedStyle>,
    registry: &Registry,
    is_root: bool,
) -> ComputedStyle {
    let initial = initial_style();
    let ctx = Context {
        font_size: parent.font_size,
        parent_font_size: parent.font_size,
        root_font_size: root.map_or(initial.font_size, |r| r.font_size),
        line_height: parent.line_height,
        root_line_height: root.map_or(initial.line_height, |r| r.line_height),
        parent_font_weight: parent.font_weight,
        color: ColorContext::default(),
        ..Context::default()
    };
    let mut c = Computer {
        doc,
        element,
        parent,
        cascaded: Cascaded::new(candidates),
        custom: parent.custom.clone(),
        ctx,
        inherited: parent.inherited.clone(),
        reset: initial.reset.clone(),
    };

    // The writing mode and direction decide what logical properties are.
    c.apply("direction");
    c.apply("writing-mode");
    let wm = c.keyword("writing-mode");
    let rtl = c.keyword("direction") == "rtl";
    c.ctx.vertical = wm.starts_with("vertical") || wm.starts_with("sideways");
    c.cascaded.map_logical(&wm, rtl);

    // Fonts first: `em` everywhere else depends on them.
    let font_size_later = c.uses_substitution("font-size");
    if !font_size_later {
        c.apply_font_size(is_root);
    }
    c.apply_custom(registry);
    if font_size_later {
        c.apply_font_size(is_root);
    }
    c.apply("line-height");
    let line_height = match c.current("line-height").and_then(|v| v.values.clone()) {
        Some(v) => match v.as_slice() {
            [Value::Number(n)] => n * c.ctx.font_size,
            [Value::Dimension(px, _)] => *px,
            _ => 1.2 * c.ctx.font_size,
        },
        None => 1.2 * c.ctx.font_size,
    };
    c.ctx.line_height = line_height;
    if is_root {
        c.ctx.root_line_height = line_height;
    }
    c.apply("font-weight");
    let font_weight = match c.current("font-weight").and_then(|v| v.values.clone()) {
        Some(v) => match v.as_slice() {
            [Value::Number(n)] => *n,
            _ => parent.font_weight,
        },
        None => parent.font_weight,
    };

    // `color`: `currentcolor` is the parent's color.
    c.ctx.color.current = Some(parent.color);
    c.apply("color");
    let color = c
        .current("color")
        .and_then(|v| color_of(&v))
        .unwrap_or(parent.color);
    c.ctx.color.current = None;

    let done = [
        "direction",
        "writing-mode",
        "font-size",
        "line-height",
        "font-weight",
        "color",
    ];
    let mut names: Vec<String> = c
        .cascaded
        .names()
        .filter(|n| !n.starts_with("--") && !done.contains(&n.as_str()))
        .cloned()
        .collect();
    names.sort();
    for n in &names {
        c.apply(n);
    }
    c.fix_up(is_root);

    ComputedStyle {
        inherited: c.inherited,
        reset: c.reset,
        custom: c.custom,
        font_size: c.ctx.font_size,
        line_height,
        font_weight,
        color,
    }
}

// ----- the styles of a document --------------------------------------------------------

/// The computed styles of the elements of a document and of their
/// pseudo-elements.
pub struct StyleMap {
    styles: HashMap<NodeId, Rc<ComputedStyle>>,
    pseudos: HashMap<(NodeId, Pseudo), Rc<ComputedStyle>>,
    root: Option<Rc<ComputedStyle>>,
    registry: Registry,
}

impl StyleMap {
    /// The style of `element` (`None` if it is not in the document).
    pub fn get(&self, element: NodeId) -> Option<&Rc<ComputedStyle>> {
        self.styles.get(&element)
    }

    /// The style of a pseudo-element some rule styles.
    pub fn pseudo(&self, element: NodeId, pseudo: Pseudo) -> Option<&Rc<ComputedStyle>> {
        self.pseudos.get(&(element, pseudo))
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }
}

/// The element `element` inherits from: its slot, the parent element,
/// or the host for the children of a shadow root.
fn inheritance_parent(
    doc: &Document,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    element: NodeId,
) -> Option<NodeId> {
    if let Some(slot) = doc.assigned_slot_internal(element) {
        if styles.contains_key(&slot) {
            return Some(slot);
        }
    }
    let p = doc.parent(element)?;
    if doc.is_element(p) {
        return Some(p);
    }
    doc.shadow_root_host(p)
}

fn element_children(doc: &Document, node: NodeId) -> Vec<NodeId> {
    doc.children(node).filter(|&c| doc.is_element(c)).collect()
}

/// Whether `pseudo` of an element with `style` gets a style even without
/// rules of its own.
fn pseudo_wanted(doc: &Document, element: NodeId, style: &ComputedStyle, pseudo: Pseudo) -> bool {
    match pseudo {
        Pseudo::Marker => style.text("display").contains("list-item"),
        Pseudo::Placeholder => {
            doc.is_html_element(element, "input") || doc.is_html_element(element, "textarea")
        }
        _ => true,
    }
}

/// Recalculates every style of `doc` (a walk of the flat tree with the
/// bloom filter of the ancestors).
fn recalc(doc: &Document, stylist: &Stylist) -> StyleMap {
    enum Step {
        Enter(NodeId),
        Exit(NodeId),
    }
    let mut map = StyleMap {
        styles: HashMap::new(),
        pseudos: HashMap::new(),
        root: None,
        registry: stylist.registry.clone(),
    };
    let initial = initial_style();
    let mut bloom = Bloom::default();
    let mut stack: Vec<Step> = element_children(doc, NodeId::DOCUMENT)
        .into_iter()
        .rev()
        .map(Step::Enter)
        .collect();
    while let Some(step) = stack.pop() {
        let e = match step {
            Step::Exit(e) => {
                bloom.pop(doc, e);
                continue;
            }
            Step::Enter(e) => e,
        };
        let is_root = doc.parent(e) == Some(NodeId::DOCUMENT);
        let parent = inheritance_parent(doc, &map.styles, e)
            .and_then(|p| map.styles.get(&p).cloned())
            .unwrap_or_else(|| initial.clone());
        let root = if is_root { None } else { map.root.clone() };
        let candidates = cascade::collect(stylist, doc, e, None, Some(&bloom));
        let style = Rc::new(compute_style(
            doc,
            e,
            &candidates,
            &parent,
            root.as_deref(),
            &stylist.registry,
            is_root,
        ));
        if is_root && map.root.is_none() {
            map.root = Some(style.clone());
        }
        let root = map.root.clone();
        for p in Pseudo::ALL {
            if !pseudo_wanted(doc, e, &style, p) {
                continue;
            }
            let pc = cascade::collect(stylist, doc, e, Some(p), Some(&bloom));
            if pc.is_empty() && p != Pseudo::Marker {
                continue;
            }
            let ps = compute_style(
                doc,
                e,
                &pc,
                &style,
                root.as_deref(),
                &stylist.registry,
                false,
            );
            map.pseudos.insert((e, p), Rc::new(ps));
        }
        map.styles.insert(e, style);
        bloom.push(doc, e);
        stack.push(Step::Exit(e));
        stack.extend(element_children(doc, e).into_iter().rev().map(Step::Enter));
        // The shadow tree comes first: slotted children inherit from
        // their slots.
        if let Some(sr) = doc.shadow_root_of(e) {
            stack.extend(element_children(doc, sr).into_iter().rev().map(Step::Enter));
        }
    }
    map
}

struct CacheEntry {
    doc: Weak<RefCell<Document>>,
    key: (u64, u64),
    map: Rc<StyleMap>,
}

thread_local! {
    static CACHE: RefCell<Vec<CacheEntry>> = const { RefCell::new(Vec::new()) };
}

/// The styles of `doc`: recalculated completely when a node, an
/// attribute, a shadow root or a style sheet changed since the last
/// recalculation, otherwise the previous ones.
pub fn styles(doc: &DocRef) -> Rc<StyleMap> {
    let env = Environment::default();
    cascade::ua_sheet();
    let sheets = cascade::tree_sheets(doc, &env);
    let key = (doc.borrow().style_generation(), om::sheets_generation());
    let cached = CACHE.with(|c| {
        c.borrow()
            .iter()
            .find(|e| e.doc.as_ptr() == Rc::as_ptr(doc) && e.key == key)
            .map(|e| e.map.clone())
    });
    if let Some(map) = cached {
        return map;
    }
    let stylist = Stylist::build(doc, &sheets, &env);
    let map = Rc::new(recalc(&doc.borrow(), &stylist));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|e| e.doc.strong_count() > 0 && e.doc.as_ptr() != Rc::as_ptr(doc));
        c.push(CacheEntry {
            doc: Rc::downgrade(doc),
            key,
            map: map.clone(),
        });
    });
    map
}

/// The computed style of `element`, if it is in `doc`'s document tree
/// or in one of its shadow trees.
pub fn computed_style(doc: &DocRef, element: NodeId) -> Option<Rc<ComputedStyle>> {
    styles(doc).get(element).cloned()
}

/// The computed style of the pseudo-element `pseudo` of `element`.
pub fn pseudo_style(doc: &DocRef, element: NodeId, pseudo: Pseudo) -> Option<Rc<ComputedStyle>> {
    let map = styles(doc);
    let style = map.get(element)?.clone();
    if let Some(s) = map.pseudo(element, pseudo) {
        return Some(s.clone());
    }
    let d = doc.borrow();
    Some(Rc::new(compute_style(
        &d,
        element,
        &[],
        &style,
        map.root.as_deref(),
        &map.registry,
        false,
    )))
}

// ----- resolved values -----------------------------------------------------------------

fn resolve_value(v: &Value, current: Absolute) -> Value {
    match v {
        Value::Color(c) => match c.resolve(current, false) {
            Some(a) => Value::Color(Box::new(Color::Absolute(a))),
            None => v.clone(),
        },
        Value::Function(name, args) => Value::Function(
            name.clone(),
            args.iter().map(|a| resolve_value(a, current)).collect(),
        ),
        Value::Block(items) => {
            Value::Block(items.iter().map(|a| resolve_value(a, current)).collect())
        }
        Value::Image(i) => {
            let value = |x: &Value| resolve_value(x, current);
            let color = |c: &Color| c.resolve(current, false).map_or(c.clone(), Color::Absolute);
            Value::Image(Box::new(i.map(&value, &color)))
        }
        _ => v.clone(),
    }
}

/// A computed value with `currentcolor` resolved against `current`.
fn resolve_text(v: &CValue, current: Absolute) -> String {
    match &v.values {
        Some(values) if v.text.contains("currentcolor") => serialize_list(
            &values
                .iter()
                .map(|x| resolve_value(x, current))
                .collect::<Vec<_>>(),
        ),
        _ => v.text.clone(),
    }
}

/// The resolved value of `property` (CSSOM §6.7.2.1) as far as it does
/// not depend on layout: the computed value with `currentcolor`
/// resolved; shorthands serialize their longhands and custom properties
/// their tokens. Unknown properties give the empty string.
pub fn resolved_value(style: &ComputedStyle, property: &str) -> String {
    if property.starts_with("--") {
        return style
            .custom
            .get(property)
            .map(|t| custom::serialize_value(t))
            .unwrap_or_default();
    }
    let lower = property.to_ascii_lowercase();
    let name = properties::canonical(&lower).unwrap_or(lower.as_str());
    if let Some(v) = style.get(name) {
        return resolve_text(v, style.color);
    }
    if name == "all" {
        return String::new();
    }
    if let Some(longhands) = properties::longhands_of(name) {
        let mut block = DeclarationBlock::default();
        for l in longhands {
            if let Some(v) = style.get(l) {
                block.push_raw(l, &tokenize(&resolve_text(v, style.color)), false);
            }
        }
        return block.property_value(name);
    }
    String::new()
}

/// The properties a computed style declaration lists: the longhands in
/// lexicographic order, then the custom properties.
pub fn resolved_names(style: &ComputedStyle) -> Vec<String> {
    let mut out: Vec<String> = PROPERTIES.iter().map(|p| p.name.to_string()).collect();
    let mut custom: Vec<String> = style.custom.keys().cloned().collect();
    custom.sort();
    out.extend(custom);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_of(html: &str) -> DocRef {
        Rc::new(RefCell::new(crate::html::parse_document(html, true)))
    }

    fn by_id(doc: &DocRef, id: &str) -> NodeId {
        let d = doc.borrow();
        d.inclusive_descendants(NodeId::DOCUMENT)
            .into_iter()
            .find(|&n| d.get_attribute(n, "id") == Some(id))
            .expect("an element with that id")
    }

    #[test]
    fn groups_are_shared() {
        let doc = doc_of(
            "<div id=a style='color: red'><span id=b></span><i id=c style='margin-top: 1px'></i></div>",
        );
        let a = computed_style(&doc, by_id(&doc, "a")).expect("a style");
        let b = computed_style(&doc, by_id(&doc, "b")).expect("a style");
        let c = computed_style(&doc, by_id(&doc, "c")).expect("a style");
        assert!(Rc::ptr_eq(&a.inherited, &b.inherited));
        assert!(Rc::ptr_eq(&b.reset, &initial_style().reset));
        assert!(!Rc::ptr_eq(&c.reset, &initial_style().reset));
        assert_eq!(resolved_value(&b, "color"), "rgb(255, 0, 0)");
    }

    #[test]
    fn ua_sheet_and_hints() {
        let doc = doc_of("<p id=p>x</p><table id=t bgcolor=red></table><div id=h hidden></div>");
        let p = computed_style(&doc, by_id(&doc, "p")).expect("a style");
        assert_eq!(p.text("display"), "block");
        assert_eq!(p.text("margin-top"), "16px");
        let t = computed_style(&doc, by_id(&doc, "t")).expect("a style");
        assert_eq!(t.text("background-color"), "rgb(255, 0, 0)");
        let h = computed_style(&doc, by_id(&doc, "h")).expect("a style");
        assert_eq!(h.text("display"), "none");
    }

    #[test]
    fn border_widths_follow_border_styles() {
        let doc = doc_of(
            "<b id=a></b><b id=b style='border-top-style: solid'></b>\
             <b id=c style='border-style: solid; border-top-width: initial; border-left-width: 1px'></b>\
             <b id=d style='border: dotted; border-right-style: hidden'></b>",
        );
        assert_eq!(initial_style().text("border-top-width"), "0px");
        let a = computed_style(&doc, by_id(&doc, "a")).expect("a style");
        assert_eq!(a.text("border-top-width"), "0px");
        let b = computed_style(&doc, by_id(&doc, "b")).expect("a style");
        assert_eq!(b.text("border-top-width"), "3px");
        assert_eq!(b.text("border-left-width"), "0px");
        let c = computed_style(&doc, by_id(&doc, "c")).expect("a style");
        assert_eq!(c.text("border-top-width"), "3px");
        assert_eq!(c.text("border-left-width"), "1px");
        let d = computed_style(&doc, by_id(&doc, "d")).expect("a style");
        assert_eq!(d.text("border-top-width"), "3px");
        assert_eq!(d.text("border-right-width"), "0px");
    }

    #[test]
    fn reset_longhands_copy_between_styles() {
        let doc = doc_of("<table id=t style='float: left; margin-top: 3px; color: red'></table>");
        let t = computed_style(&doc, by_id(&doc, "t")).expect("a style");
        let initial = initial_style();
        let moved = initial.with_reset_from(&t, &["float", "margin-top", "color"]);
        assert_eq!(moved.text("float"), "left");
        assert_eq!(moved.text("margin-top"), "3px");
        // `color` is inherited: it is not copied.
        assert_eq!(moved.text("color"), initial.text("color"));
        let cleared = t.with_initial(&["float", "margin-top"]);
        assert_eq!(cleared.text("float"), "none");
        assert_eq!(cleared.text("margin-top"), "0px");
        assert_eq!(cleared.text("display"), "table");
    }

    #[test]
    fn recalc_follows_attribute_changes() {
        let doc = doc_of("<div id=a></div>");
        let a = by_id(&doc, "a");
        assert_eq!(
            computed_style(&doc, a).expect("a style").text("display"),
            "block"
        );
        doc.borrow_mut()
            .set_attribute(a, "style", "display: inline-block");
        assert_eq!(
            computed_style(&doc, a).expect("a style").text("display"),
            "inline-block"
        );
    }
}
